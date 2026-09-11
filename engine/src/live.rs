//! Live Level II (DESIGN.md, live sweeps as built): the real-time chunk bucket polled with
//! `nexrad-data`'s pull-based `ChunkIterator`, one task per selected station
//! (DESIGN.md, engine runtime). Each chunk's radials feed an `Assembler`
//! that keeps the lowest cut of the current volume; every chunk that grows
//! or completes it is reported as an `Event::Sweep`, which `main.rs` turns
//! into a republished texture and a `state` broadcast, so the sweep paints
//! chunk by chunk as the antenna turns.
//!
//! Joining a volume in progress: the iterator hands over the newest chunk
//! and, when that is not the volume's first, the Start chunk; the chunks in
//! between that the VCP maps to the lowest cut are downloaded once, so the
//! last complete lowest sweep shows within seconds of selecting a station
//! and the next volume paints live. Every network call sits under a
//! `tokio::time::timeout`, since the client sets none. The bucket is public
//! and needs no credentials; nothing here runs until a client selects a
//! station, so launch still fetches nothing.

use crate::sweep::Sweep;
use chrono::{SecondsFormat, Utc};
use nexrad_data::aws::realtime::{
    Chunk, ChunkIdentifier, ChunkIterator, DownloadedChunk, VolumeIndex, download_chunk,
    list_chunks_in_volume,
};
use nexrad_data::result::{Error, aws::AWSError};
use nexrad_data::volume::Record;
use nexrad_model::data::{Radial, RadialStatus};
use std::time::Duration;
use tokio::{
    sync::mpsc::Sender,
    time::{Instant, error::Elapsed, sleep, timeout, timeout_at},
};

/// NOAA's real-time bucket, named in frame provenance (`nexrad-data` owns
/// the address).
pub const BUCKET: &str = "unidata-nexrad-level2-chunks";
/// Finding the latest volume is a binary search of listings; one chunk is
/// one request.
const START_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Between polls when the next chunk is not there yet: the iterator's
/// estimate when it has one, clamped; `IDLE` when it has none. Chunks land
/// every 4–12 s.
const IDLE: Duration = Duration::from_secs(2);
const MIN_WAIT: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(10);
/// After a failed call; doubles up to the maximum while the bucket stays
/// unreachable. One failure is retried quietly (a pooled connection the
/// bucket closed, say); the second in a row reports `offline`, and this
/// many start over from discovery.
const BACK_OFF: Duration = Duration::from_secs(5);
const MAX_BACK_OFF: Duration = Duration::from_secs(60);
const OFFLINE_AFTER: u32 = 2;
const RESTART_AFTER: u32 = 4;
/// Higher cuts of the same volume still arrive every 4–12 s after the
/// lowest cut ends. Bound the time without a chunk across empty polls,
/// network errors, and requests still in flight: the iterator may be parked
/// on a chunk that will never appear (a rotated volume or skipped sequence).
/// Restart discovery instead of waiting until the UI goes UNAVAILABLE.
const QUIET_RESTART: Duration = Duration::from_secs(90);
/// Chunks replayed from a volume's start when the VCP could not be read and
/// so no chunk can be mapped to a cut: the lowest cut of a super-resolution
/// volume spans about four.
const BLIND_REPLAY: usize = 12;
/// Volumes before the current one fetched on joining a station, newest
/// first, so a fresh station has a loop to play rather than one frame. The
/// fetch starts after `BACKFILL_DELAY`, so a hand-off passed while panning
/// costs nothing, and ends with the poller. The bucket rotates volume
/// numbers 1–999 and keeps a few hours; a volume's lowest cut is within
/// its first `BACKFILL_CHUNKS` chunks or is given up on.
const BACKFILL_VOLUMES: usize = 12;
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
const BACKFILL_CHUNKS: usize = 16;

fn live_log(site: &str, message: impl std::fmt::Display) {
    eprintln!(
        "{} Live {site}: {message}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    );
}

/// A rediscovery (or a respawned poller) that finds only a sweep already
/// in the catalog must not publish it again. The first join after a
/// `select_site` still may, so `loading` can clear.
fn skip_catalogued_replay(skip_known: bool, start_ms: i64, known: &[i64]) -> bool {
    skip_known && known.contains(&start_ms)
}

/// What the poller reports to `main.rs`.
pub enum Event {
    /// An earlier volume's complete lowest cut, fetched on joining: the
    /// timeline gains history while the frame on screen stands.
    Backfill {
        site: String,
        sweep: Sweep,
        provenance: String,
    },
    /// The lowest cut of the current volume grew or completed; `provenance`
    /// names the bucket, volume, and chunks it came from.
    Sweep {
        site: String,
        sweep: Sweep,
        complete: bool,
        provenance: String,
    },
    /// The bucket could not be reached or read; the frame on screen stands.
    Offline { site: String, reason: String },
    /// The bucket answered and holds no volume for the station: the feed is
    /// up and the station has published nothing.
    Silent { site: String, reason: String },
}

/// The lowest cut of the current volume, radial by radial, in arrival order.
#[derive(Default)]
pub struct Assembler {
    radials: Vec<Radial>,
    /// The cut has ended: its last radial said so, or the next cut began.
    done: bool,
    volume: String,
    first_chunk: String,
    last_chunk: String,
}

/// The sweep after a chunk changed it.
pub struct Update {
    pub sweep: Sweep,
    pub complete: bool,
    pub provenance: String,
}

impl Assembler {
    /// The cut's first collection time, once a radial is in.
    pub fn start_ms(&self) -> Option<i64> {
        self.radials.first().map(Radial::collection_timestamp)
    }
    /// Feed one chunk's radials. A Start chunk, or any chunk of a volume
    /// other than the current one (a late poll that missed the Start),
    /// begins a new volume and discards the previous cut, finished or not.
    /// Returns the sweep when the chunk added radials to the lowest cut or
    /// ended it.
    pub fn feed(
        &mut self,
        starts_volume: bool,
        volume: &str,
        chunk: &str,
        radials: Vec<Radial>,
    ) -> Result<Option<Update>, String> {
        if starts_volume || volume != self.volume {
            self.radials.clear();
            self.done = false;
            self.volume = volume.to_owned();
            self.first_chunk = chunk.to_owned();
        }
        let mut changed = false;
        for radial in radials {
            if self.done {
                break;
            }
            if radial.elevation_number() == 1 {
                let ends = matches!(radial.radial_status(), RadialStatus::ElevationEnd);
                self.radials.push(radial);
                changed = true;
                if ends {
                    self.done = true;
                }
            } else if !self.radials.is_empty() {
                // The next cut's first radial: the lowest cut is complete.
                self.done = true;
                changed = true;
            }
        }
        if !changed || self.radials.is_empty() {
            return Ok(None);
        }
        self.last_chunk = chunk.to_owned();
        Ok(Some(Update {
            sweep: Sweep::from_radials(&self.radials)?,
            complete: self.done,
            provenance: format!(
                "{BUCKET}/{}/{}..{}",
                self.volume, self.first_chunk, self.last_chunk
            ),
        }))
    }
}

/// Every radial in a chunk, in order. A Start chunk is an Archive II header
/// and one or more records; the others are one record.
pub fn radials_of(chunk: &Chunk) -> Result<Vec<Radial>, String> {
    let decode = |record: &Record| -> Result<Vec<Radial>, String> {
        let record = if record.compressed() {
            record
                .decompress()
                .map_err(|e| format!("decompressing record: {e}"))?
        } else {
            Record::new(record.data().to_vec())
        };
        record
            .radials()
            .map_err(|e| format!("decoding record: {e}"))
    };
    match chunk {
        Chunk::Start(file) => {
            let mut radials = Vec::new();
            for record in file
                .records()
                .map_err(|e| format!("splitting records: {e}"))?
            {
                radials.extend(decode(&record)?);
            }
            Ok(radials)
        }
        Chunk::IntermediateOrEnd(record) => decode(record),
    }
}

/// Feed a downloaded chunk to the assembler and report what changed.
/// `false` when the event channel is closed (the poller was replaced).
/// `known` is the catalogued (and already-delivered) start times to skip
/// on a rediscovery; live chunks pass `None` so a growing cut still paints.
async fn deliver(
    assembler: &mut Assembler,
    site: &str,
    chunk: &DownloadedChunk,
    events: &Sender<Event>,
    known: Option<&[i64]>,
) -> bool {
    let id = &chunk.identifier;
    let volume = format!("{}/{:03}", id.site(), id.volume().as_number());
    let radials = match radials_of(&chunk.chunk) {
        Ok(radials) => radials,
        Err(e) => {
            live_log(site, format_args!("chunk {}: {e}", id.name()));
            return true;
        }
    };
    let starts = matches!(chunk.chunk, Chunk::Start(_));
    match assembler.feed(starts, &volume, id.name(), radials) {
        Ok(Some(update)) => {
            if known.is_some_and(|times| skip_catalogued_replay(true, update.sweep.start_ms, times))
            {
                return true;
            }
            events
                .send(Event::Sweep {
                    site: site.to_owned(),
                    sweep: update.sweep,
                    complete: update.complete,
                    provenance: update.provenance,
                })
                .await
                .is_ok()
        }
        Ok(None) => true,
        Err(e) => {
            live_log(site, format_args!("chunk {}: {e}", id.name()));
            true
        }
    }
}

async fn offline(events: &Sender<Event>, site: &str, reason: String) -> bool {
    events
        .send(Event::Offline {
            site: site.to_owned(),
            reason,
        })
        .await
        .is_ok()
}

/// The chunks of `newest`'s volume before it that carry the lowest cut,
/// downloaded in order: sequence 1 (unless `have_start`) up to the newest,
/// those the VCP maps to elevation 1, or the first `BLIND_REPLAY` when the
/// VCP is unknown. Used when joining a volume in progress and when a poll
/// reaches the next volume after more than its first chunk landed.
async fn earlier_chunks(
    site: &str,
    iterator: &ChunkIterator,
    newest: &ChunkIdentifier,
    have_start: bool,
) -> Vec<DownloadedChunk> {
    let mut chunks = Vec::new();
    let first = if have_start { 2 } else { 1 };
    if newest.sequence() <= first {
        return chunks;
    }
    let volume = *newest.volume();
    let ids = match timeout(CALL_TIMEOUT, list_chunks_in_volume(site, volume, 100)).await {
        Ok(Ok(ids)) => ids,
        Ok(Err(e)) => {
            live_log(
                site,
                format_args!("listing volume {}: {e}", volume.as_number()),
            );
            return chunks;
        }
        Err(_) => {
            live_log(
                site,
                format_args!("listing volume {} timed out", volume.as_number()),
            );
            return chunks;
        }
    };
    for id in ids {
        let sequence = id.sequence();
        if sequence < first || sequence >= newest.sequence() {
            continue;
        }
        let wanted = match iterator.elevation_mapper() {
            Some(mapper) => mapper.get_sequence_elevation_number(sequence) == Some(1),
            None => sequence <= BLIND_REPLAY,
        };
        if !wanted {
            continue;
        }
        match timeout(CALL_TIMEOUT, download_chunk(site, &id)).await {
            Ok(Ok((identifier, chunk))) => chunks.push(DownloadedChunk {
                identifier,
                chunk,
                attempts: 1,
            }),
            Ok(Err(e)) => live_log(site, format_args!("replaying {}: {e}", id.name())),
            Err(_) => live_log(site, format_args!("replaying {} timed out", id.name())),
        }
    }
    chunks.sort_by_key(|chunk| chunk.identifier.sequence());
    chunks
}

/// The volume `back` places before `current` in the bucket's 1–999 rotation.
fn previous_volume(current: VolumeIndex, back: usize) -> VolumeIndex {
    let n = current.as_number();
    let back = back % 999;
    VolumeIndex::new(if n > back { n - back } else { n + 999 - back })
}

/// Fetch the lowest cut of the `BACKFILL_VOLUMES` volumes before `current`,
/// newest first, and report each complete one as `Event::Backfill`. A
/// volume whose start time is already catalogued (`cached`) costs one
/// chunk; a volume with no Start chunk in the listing is skipped; a listing
/// failure ends the backfill, since the bucket is not answering.
async fn backfill(site: String, events: Sender<Event>, current: VolumeIndex, cached: Vec<i64>) {
    sleep(BACKFILL_DELAY).await;
    let mut fetched = 0;
    for back in 1..=BACKFILL_VOLUMES {
        let volume = previous_volume(current, back);
        let mut ids = match timeout(CALL_TIMEOUT, list_chunks_in_volume(&site, volume, 100)).await {
            Ok(Ok(ids)) => ids,
            Ok(Err(e)) => {
                live_log(
                    &site,
                    format_args!("backfill listing volume {}: {e}", volume.as_number()),
                );
                return;
            }
            Err(_) => {
                live_log(
                    &site,
                    format_args!("backfill listing volume {} timed out", volume.as_number()),
                );
                return;
            }
        };
        ids.sort_by_key(ChunkIdentifier::sequence);
        if ids.first().map(ChunkIdentifier::sequence) != Some(1) {
            continue;
        }
        let name = format!("{site}/{:03}", volume.as_number());
        let mut assembler = Assembler::default();
        for id in ids.iter().take(BACKFILL_CHUNKS) {
            let (identifier, chunk) = match timeout(CALL_TIMEOUT, download_chunk(&site, id)).await {
                Ok(Ok(got)) => got,
                Ok(Err(e)) => {
                    live_log(&site, format_args!("backfill {}: {e}", id.name()));
                    break;
                }
                Err(_) => {
                    live_log(&site, format_args!("backfill {} timed out", id.name()));
                    break;
                }
            };
            let radials = match radials_of(&chunk) {
                Ok(radials) => radials,
                Err(e) => {
                    live_log(&site, format_args!("backfill {}: {e}", id.name()));
                    break;
                }
            };
            let starts = matches!(chunk, Chunk::Start(_));
            let update = match assembler.feed(starts, &name, identifier.name(), radials) {
                Ok(update) => update,
                Err(e) => {
                    live_log(&site, format_args!("backfill {}: {e}", id.name()));
                    break;
                }
            };
            if assembler.start_ms().is_some_and(|ms| cached.contains(&ms)) {
                break;
            }
            if let Some(update) = update.filter(|u| u.complete) {
                let sent = events
                    .send(Event::Backfill {
                        site: site.clone(),
                        sweep: update.sweep,
                        provenance: update.provenance,
                    })
                    .await;
                if sent.is_err() {
                    return;
                }
                fetched += 1;
                break;
            }
        }
    }
    live_log(&site, format_args!("backfilled {fetched} earlier volumes"));
}

/// Aborts its task when dropped, so a poller replaced mid-backfill takes
/// the downloads with it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Consume empty polls without extending the deadline for chunk progress.
/// The callback waits between empty polls and returns errors as results.
async fn wait_for_progress<S, T, F: std::future::Future<Output = (S, Option<T>)>>(
    deadline: Instant,
    mut state: S,
    mut poll: impl FnMut(S) -> F,
) -> Result<(S, T), Elapsed> {
    timeout_at(deadline, async {
        loop {
            let (next_state, result) = poll(state).await;
            state = next_state;
            if let Some(result) = result {
                return (state, result);
            }
        }
    })
    .await
}

/// Poll `site` until the task is aborted or the event channel closes.
/// `cached` holds the start times of the frames already catalogued for the
/// station, so the backfill does not fetch them again and a rediscovery
/// does not republish them. `skip_known` is true when this poller is a
/// respawn on a station already on screen (cleanup or reselect).
pub async fn poll(site: String, events: Sender<Event>, cached: Vec<i64>, skip_known: bool) {
    let mut back_off = BACK_OFF;
    let mut backfilling: Option<AbortOnDrop> = None;
    let mut known = cached;
    let mut skip_known = skip_known;
    loop {
        let init = match timeout(START_TIMEOUT, ChunkIterator::start(&site)).await {
            Ok(Ok(init)) => init,
            Ok(Err(e)) => {
                // An empty listing is the bucket's answer, not its absence.
                let event = if matches!(e, Error::AWS(AWSError::LatestVolumeNotFound)) {
                    Event::Silent {
                        site: site.clone(),
                        reason: "the bucket holds no volume for this station".into(),
                    }
                } else {
                    Event::Offline {
                        site: site.clone(),
                        reason: format!("finding the latest volume: {e}"),
                    }
                };
                if events.send(event).await.is_err() {
                    return;
                }
                sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
            Err(_) => {
                if !offline(&events, &site, "finding the latest volume timed out".into()).await {
                    return;
                }
                sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
        };
        back_off = BACK_OFF;
        let mut iterator = init.iterator;
        let mut assembler = Assembler::default();

        // Replay the volume so far: the Start chunk, the chunks between it
        // and the newest that the VCP maps to the lowest cut, and the newest.
        let newest = init.latest_chunk;
        let mut replay = earlier_chunks(
            &site,
            &iterator,
            &newest.identifier,
            init.start_chunk.is_some(),
        )
        .await;
        if let Some(start) = init.start_chunk {
            replay.insert(0, start);
        }
        live_log(
            &site,
            format_args!(
                "joined volume {} at chunk {}, replaying {} chunks",
                newest.identifier.volume().as_number(),
                newest.identifier.name(),
                replay.len()
            ),
        );
        if backfilling.is_none() {
            backfilling = Some(AbortOnDrop(tokio::spawn(backfill(
                site.clone(),
                events.clone(),
                *newest.identifier.volume(),
                known.clone(),
            ))));
        }
        replay.push(newest);
        let mut previous_volume = None;
        {
            let replay_known = skip_known.then_some(known.as_slice());
            for chunk in &replay {
                previous_volume = Some(*chunk.identifier.volume());
                if !deliver(&mut assembler, &site, chunk, &events, replay_known).await {
                    return;
                }
            }
        }
        if let Some(start_ms) = assembler.start_ms()
            && !known.contains(&start_ms)
        {
            known.push(start_ms);
        }
        skip_known = true;

        let mut failures = 0;
        let mut deadline = Instant::now() + QUIET_RESTART;
        loop {
            let next = wait_for_progress(deadline, iterator, async |mut iterator| {
                let result = match timeout(CALL_TIMEOUT, iterator.try_next()).await {
                    Ok(Ok(None)) => {
                        let wait = iterator
                            .time_until_next()
                            .and_then(|d| d.to_std().ok())
                            .unwrap_or(IDLE)
                            .clamp(MIN_WAIT, MAX_WAIT);
                        sleep(wait).await;
                        None
                    }
                    result => Some(result),
                };
                (iterator, result)
            })
            .await;
            let Ok((next_iterator, next)) = next else {
                live_log(
                    &site,
                    format_args!(
                        "no chunk progress for {}s; rediscovering latest volume",
                        QUIET_RESTART.as_secs()
                    ),
                );
                break;
            };
            iterator = next_iterator;
            match next {
                Ok(Ok(Some(chunk))) => {
                    failures = 0;
                    deadline = Instant::now() + QUIET_RESTART;
                    // The iterator enters the next volume at its newest chunk;
                    // a poll that arrived after more than the Start chunk
                    // landed fetches the ones it skipped first.
                    if previous_volume != Some(*chunk.identifier.volume()) {
                        previous_volume = Some(*chunk.identifier.volume());
                        for skipped in
                            earlier_chunks(&site, &iterator, &chunk.identifier, false).await
                        {
                            if !deliver(&mut assembler, &site, &skipped, &events, None).await {
                                return;
                            }
                        }
                    }
                    if !deliver(&mut assembler, &site, &chunk, &events, None).await {
                        return;
                    }
                    if let Some(start_ms) = assembler.start_ms()
                        && !known.contains(&start_ms)
                    {
                        known.push(start_ms);
                    }
                }
                Ok(Ok(None)) => unreachable!("empty polls are consumed by wait_for_progress"),
                Ok(Err(e)) => {
                    failures += 1;
                    let reason = format!("fetching the next chunk: {e} ({e:?})");
                    if failures < OFFLINE_AFTER {
                        live_log(&site, format_args!("{reason}; retrying"));
                    } else if !offline(&events, &site, reason).await {
                        return;
                    }
                    if failures >= RESTART_AFTER {
                        break;
                    }
                    sleep(BACK_OFF).await;
                }
                Err(_) => {
                    failures += 1;
                    let reason = "fetching the next chunk timed out".to_owned();
                    if failures < OFFLINE_AFTER {
                        live_log(&site, format_args!("{reason}; retrying"));
                    } else if !offline(&events, &site, reason).await {
                        return;
                    }
                    if failures >= RESTART_AFTER {
                        break;
                    }
                    sleep(BACK_OFF).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::lowest_reflectivity;
    use nexrad_data::volume::File;
    use std::fs;

    #[test]
    fn earlier_volumes_wrap_through_the_rotation() {
        assert_eq!(previous_volume(VolumeIndex::new(598), 1).as_number(), 597);
        assert_eq!(previous_volume(VolumeIndex::new(3), 5).as_number(), 997);
        assert_eq!(previous_volume(VolumeIndex::new(5), 5).as_number(), 999);
        assert_eq!(previous_volume(VolumeIndex::new(1), 1).as_number(), 999);
        assert_eq!(previous_volume(VolumeIndex::new(999), 12).as_number(), 987);
    }

    #[test]
    fn missing_chunk_cannot_wait_forever() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let result =
                    wait_for_progress(Instant::now() + Duration::from_millis(30), (), async |()| {
                        sleep(Duration::from_millis(1)).await;
                        ((), None::<()>)
                    })
                    .await;
                assert!(result.is_err(), "missing chunks must trigger rediscovery");
            });
    }

    #[test]
    fn progress_deadline_cancels_a_request_still_in_flight() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let result =
                    wait_for_progress(Instant::now() + Duration::from_millis(30), (), async |()| {
                        sleep(Duration::from_secs(60)).await;
                        ((), Some("late chunk"))
                    })
                    .await;
                assert!(result.is_err());
            });
    }

    #[test]
    fn delayed_chunk_is_delivered_before_deadline() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let result =
                    wait_for_progress(Instant::now() + Duration::from_secs(5), 0, async |calls| {
                        let calls = calls + 1;
                        sleep(Duration::from_millis(1)).await;
                        (calls, (calls == 3).then_some("next chunk"))
                    })
                    .await;
                assert_eq!(result.unwrap(), (3, "next chunk"));
            });
    }

    #[test]
    fn intermittent_errors_do_not_extend_progress_deadline() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let deadline = Instant::now() + Duration::from_millis(30);
                let (_, error) =
                    wait_for_progress(deadline, (), async |()| ((), Some(Err::<(), _>("reset"))))
                        .await
                        .unwrap();
                assert_eq!(error, Err("reset"));
                let result = wait_for_progress(deadline, (), async |()| {
                    sleep(Duration::from_millis(1)).await;
                    ((), None::<()>)
                })
                .await;
                assert!(result.is_err());
            });
    }

    #[test]
    fn a_catalogued_replay_is_skipped_after_the_first_join() {
        let start = 1_367_082_403_000;
        assert!(
            !skip_catalogued_replay(false, start, &[start]),
            "the first join after select_site still publishes, so loading can clear"
        );
        assert!(
            !skip_catalogued_replay(true, start, &[start + 1]),
            "a newer volume is not in the catalog"
        );
        assert!(
            skip_catalogued_replay(true, start, &[start]),
            "a rediscovery of the same sweep must not republish it"
        );
    }

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/KTLX20130520_201643_V06.gz"
    );

    /// Every radial of the fixture volume in decoded order (the archive is
    /// one uncompressed record, so records cannot stand in for chunks).
    fn all_radials() -> Vec<Radial> {
        let archive = fs::read(FIXTURE).unwrap();
        let file = File::new(archive).decompress().unwrap();
        let mut radials = Vec::new();
        for record in file.records().unwrap() {
            radials.extend(radials_of(&Chunk::IntermediateOrEnd(record)).unwrap());
        }
        radials
    }

    /// Slices of the volume's radials stand in for chunks. Fed in order, the
    /// assembler must report a growing partial sweep with the blank row and
    /// then, on the slice carrying the cut's last radial, the same complete
    /// sweep the fixture decoder produces; later cuts change nothing.
    #[test]
    fn chunks_assemble_the_lowest_cut_radial_by_radial() {
        let expected = lowest_reflectivity(&fs::read(FIXTURE).unwrap()).unwrap();
        let radials = all_radials();
        assert_eq!(radials.len(), 8280);
        let mut assembler = Assembler::default();
        let mut partials = Vec::new();
        let mut complete = None;
        for (i, slice) in radials.chunks(100).enumerate() {
            let name = format!("20130520-201643-{:03}-I", i + 1);
            let update = assembler
                .feed(i == 0, "KTLX/001", &name, slice.to_vec())
                .unwrap();
            match update {
                Some(update) if update.complete => {
                    assert!(complete.is_none(), "completed twice");
                    assert_eq!(
                        update.provenance,
                        format!("{BUCKET}/KTLX/001/20130520-201643-001-I..{name}")
                    );
                    complete = Some(update.sweep);
                }
                Some(update) => {
                    assert!(complete.is_none(), "grew after completing");
                    // A partial sweep leaves a gap, so it carries the blank row.
                    assert_eq!(update.sweep.rows(), update.sweep.rays.len() as u32 + 1);
                    partials.push(update.sweep.rays.len());
                }
                None => {}
            }
        }
        assert_eq!(partials, [100, 200, 300, 400, 500, 600, 700]);
        let complete = complete.expect("the lowest cut completed");
        assert_eq!(complete.rays.len(), expected.rays.len());
        assert_eq!(complete.rows(), 720);
        assert_eq!(
            (complete.start_ms, complete.end_ms),
            (expected.start_ms, expected.end_ms)
        );
        for (a, b) in complete.rays.iter().zip(&expected.rays) {
            assert_eq!(a.azimuth_deg, b.azimuth_deg);
            assert_eq!(a.time_ms, b.time_ms);
            assert!(a.codes == b.codes);
        }
        // A new volume's Start chunk discards the finished cut.
        let update = assembler
            .feed(true, "KTLX/002", "x-001-S", radials[..50].to_vec())
            .unwrap()
            .expect("the new volume's first radials");
        assert!(!update.complete);
        assert_eq!(update.sweep.rays.len(), 50);
        // So does a chunk of yet another volume that arrives without its
        // Start (a late poll): the cut begins from what arrived.
        let update = assembler
            .feed(false, "KTLX/003", "y-002-I", radials[120..240].to_vec())
            .unwrap()
            .expect("the later volume's radials");
        assert!(!update.complete);
        assert_eq!(update.sweep.rays.len(), 120);
        assert!(update.provenance.ends_with("KTLX/003/y-002-I..y-002-I"));
    }

    /// Without the cut's last radial (a dropped chunk), the next cut's first
    /// radial completes the sweep with what arrived.
    #[test]
    fn the_next_cut_completes_a_sweep_missing_its_last_radial() {
        let radials = all_radials();
        let mut assembler = Assembler::default();
        assert!(
            assembler
                .feed(true, "v", "a", radials[..700].to_vec())
                .unwrap()
                .is_some_and(|u| !u.complete)
        );
        let update = assembler
            .feed(false, "v", "b", radials[720..800].to_vec())
            .unwrap()
            .expect("the cut ended");
        assert!(update.complete);
        assert_eq!(update.sweep.rays.len(), 700);
        assert_eq!(update.sweep.rows(), 701, "the missing rays leave a gap");
        assert!(
            assembler
                .feed(false, "v", "c", radials[800..900].to_vec())
                .unwrap()
                .is_none()
        );
    }

    /// A Start chunk begins with the Archive II header: the whole fixture
    /// file is one, and decodes to every radial of the volume.
    #[test]
    fn a_start_chunk_decodes_from_its_header() {
        let archive = fs::read(FIXTURE).unwrap();
        let file = File::new(archive).decompress().unwrap();
        let start = Chunk::new(file.data().to_vec()).unwrap();
        assert!(matches!(start, Chunk::Start(_)));
        let radials = radials_of(&start).unwrap();
        assert_eq!(radials.len(), 8280);
        assert_eq!(radials[0].elevation_number(), 1);
        assert_eq!(radials[0].radial_status(), RadialStatus::ScanStart);
        assert_eq!(radials[719].radial_status(), RadialStatus::ElevationEnd);
        assert_eq!(radials[720].elevation_number(), 2);
        assert!(Chunk::new(b"not a chunk".to_vec()).is_err());
    }
}
