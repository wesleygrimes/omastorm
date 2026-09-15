//! Live Level II (DESIGN.md, live sweeps as built): the real-time chunk bucket polled with
//! dated chunk listings, one task per selected station
//! (DESIGN.md, engine runtime). Each chunk's radials feed an `Assembler`
//! that keeps the lowest cut of the current volume; every chunk that grows
//! or completes it is reported as an `Event::Sweep`, which `main.rs` turns
//! into a republished texture and a `state` broadcast, so the sweep paints
//! chunk by chunk as the antenna turns.
//!
//! Joining a volume in progress: the archive header names the slot. The
//! poller lists the slots after that header, takes the first with names
//! newer than the archive file, and replays the Start and first
//! lowest-cut chunks once, so the last complete lowest sweep shows within
//! seconds of selecting a station and the next volume paints live. Every
//! network call sits under a `tokio::time::timeout`, since the client sets
//! none. The bucket is public and needs no credentials; nothing here runs
//! until a client selects a station, so launch still fetches nothing.

use crate::{live_index, sweep::Sweep};
use chrono::{NaiveDateTime, SecondsFormat, TimeDelta, Utc};
use nexrad_data::aws::realtime::{
    Chunk, ChunkIdentifier, ChunkType, DownloadedChunk, VolumeIndex, download_chunk,
};
use nexrad_data::result::Error;
use nexrad_data::volume::Record;
use nexrad_model::data::{Radial, RadialStatus};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::{
    sync::mpsc::Sender,
    time::{Instant, error::Elapsed, sleep, timeout, timeout_at},
};

/// NOAA's real-time bucket, named in frame provenance (`nexrad-data` owns
/// the address).
pub const BUCKET: &str = "unidata-nexrad-level2-chunks";
/// One archive listing, one 24-byte header range-get, and up to three slot listings.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Between empty polls. Chunks land every 4–12 s.
const IDLE: Duration = Duration::from_secs(2);
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
/// network errors, and requests still in flight: a volume may be abandoned
/// before its final chunk arrives, or a listing may lag the live rotation.
/// Restart discovery instead of waiting until the UI goes UNAVAILABLE.
const QUIET_RESTART: Duration = Duration::from_secs(90);
/// Chunks replayed from a volume's start to cover its lowest cut. The lowest
/// cut of a super-resolution volume spans about four chunks.
const LOW_CUT_REPLAY: usize = 12;
/// Volumes before the current one fetched on joining a station, newest
/// first, so a fresh station has a loop to play rather than one frame. The
/// fetch starts after `BACKFILL_DELAY`, so a hand-off passed while panning
/// costs nothing, and ends with the poller. Prior volumes come from the
/// day's archive listing (and yesterday's if today is short); each file's
/// header names its chunk-bucket slot. A volume's lowest cut is within
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

/// Download one dated chunk; caller bounds the whole operation with a timeout.
async fn fetch(site: &str, id: &ChunkIdentifier) -> Result<DownloadedChunk, Error> {
    let (identifier, chunk) = download_chunk(site, id).await?;
    Ok(DownloadedChunk {
        identifier,
        chunk,
        attempts: 1,
    })
}

/// Replay the Start and the first low-cut chunks, then the latest chunk if
/// joining later in the volume. The listing has already been narrowed to
/// one dated generation, so old keys cannot leak into a new sweep.
async fn replay(site: &str, ids: &[ChunkIdentifier]) -> Vec<DownloadedChunk> {
    let Some(newest) = ids.last() else {
        return Vec::new();
    };
    let mut chunks = Vec::new();
    for id in ids
        .iter()
        .filter(|id| id.sequence() <= LOW_CUT_REPLAY || id.name() == newest.name())
    {
        match timeout(CALL_TIMEOUT, fetch(site, id)).await {
            Ok(Ok(chunk)) => chunks.push(chunk),
            Ok(Err(e)) => live_log(site, format_args!("replaying {}: {e}", id.name())),
            Err(_) => live_log(site, format_args!("replaying {} timed out", id.name())),
        }
    }
    chunks
}

/// How many slots the live volume sits after the archive header (1–3, wrapping 999→1).
fn slot_offset(from: VolumeIndex, to: VolumeIndex) -> usize {
    let a = from.as_number();
    let b = to.as_number();
    if b >= a { b - a } else { b + 999 - a }
}

fn catalogued_near(cached: &[i64], stamp: NaiveDateTime) -> bool {
    let ms = stamp.and_utc().timestamp_millis();
    cached.iter().any(|&t| (t - ms).abs() <= 2_000)
}

/// Earlier finished volumes for backfill: at most `limit`, newest first,
/// none newer than the joined archive file. Yesterday is unused when today
/// already has more than `limit` files.
fn prior_volumes(
    today: Vec<live_index::ArchiveVolume>,
    yesterday: Vec<live_index::ArchiveVolume>,
    archived_stamp: NaiveDateTime,
    limit: usize,
) -> Vec<live_index::ArchiveVolume> {
    let mut vols = today;
    if vols.len() < limit + 1 {
        vols.extend(yesterday);
    }
    vols.retain(|v| v.stamp <= archived_stamp);
    vols.sort_by_key(|v| v.stamp);
    let start = vols.len().saturating_sub(limit);
    vols[start..].iter().rev().cloned().collect()
}

/// Fetch the lowest cut of the `BACKFILL_VOLUMES` volumes at or before the
/// joined archive file, newest first, and report each complete one as
/// `Event::Backfill`. A volume whose start time is already catalogued
/// (`cached`) is skipped; a volume with no Start chunk in the listing is
/// skipped; a listing or header failure ends the backfill.
async fn backfill(site: String, events: Sender<Event>, join: live_index::Join, cached: Vec<i64>) {
    sleep(BACKFILL_DELAY).await;
    let today = Utc::now().date_naive();
    let today_vols = match timeout(CALL_TIMEOUT, live_index::archive_volumes(&site, today)).await {
        Ok(Ok(vols)) => vols,
        Ok(Err(e)) => {
            live_log(&site, format_args!("backfill listing archive: {e}"));
            return;
        }
        Err(_) => {
            live_log(&site, "backfill listing archive timed out");
            return;
        }
    };
    let yesterday = if today_vols.len() < BACKFILL_VOLUMES + 1 {
        let Some(day) = today.pred_opt() else {
            live_log(&site, "backfill listing archive: no yesterday");
            return;
        };
        match timeout(CALL_TIMEOUT, live_index::archive_volumes(&site, day)).await {
            Ok(Ok(vols)) => vols,
            Ok(Err(e)) => {
                live_log(&site, format_args!("backfill listing archive: {e}"));
                return;
            }
            Err(_) => {
                live_log(&site, "backfill listing archive timed out");
                return;
            }
        }
    } else {
        Vec::new()
    };
    let entries = prior_volumes(today_vols, yesterday, join.archived.stamp, BACKFILL_VOLUMES);
    let mut fetched = 0;
    for entry in entries {
        if catalogued_near(&cached, entry.stamp) {
            continue;
        }
        let volume = match timeout(CALL_TIMEOUT, live_index::archived_slot(&entry.key)).await {
            Ok(Ok(volume)) => volume,
            Ok(Err(e)) => {
                live_log(&site, format_args!("backfill header {}: {e}", entry.key));
                return;
            }
            Err(_) => {
                live_log(
                    &site,
                    format_args!("backfill header {} timed out", entry.key),
                );
                return;
            }
        };
        let after = entry.stamp - TimeDelta::seconds(1);
        let ids = match timeout(CALL_TIMEOUT, live_index::list_after(&site, volume, after)).await {
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
        let Some((_, ids)) = live_index::generation(ids, |stamp| stamp == entry.stamp) else {
            continue;
        };
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

/// The exact generation and sequence being followed through the rotating ring.
struct Cursor {
    volume: VolumeIndex,
    stamp: NaiveDateTime,
    sequence: usize,
    ended: bool,
    pending: VecDeque<ChunkIdentifier>,
}

impl Cursor {
    fn new(volume: VolumeIndex, stamp: NaiveDateTime, newest: &ChunkIdentifier) -> Self {
        Self {
            volume,
            stamp,
            sequence: newest.sequence(),
            ended: newest.chunk_type() == ChunkType::End,
            pending: VecDeque::new(),
        }
    }

    fn queue_current(&mut self, ids: Vec<ChunkIdentifier>) {
        let mut fresh: Vec<_> = ids
            .into_iter()
            .filter(|id| *id.date_time_prefix() == self.stamp && id.sequence() > self.sequence)
            .collect();
        fresh.sort_by_key(ChunkIdentifier::sequence);
        self.pending.extend(fresh);
    }

    fn queue_next(&mut self, volume: VolumeIndex, ids: Vec<ChunkIdentifier>) {
        if let Some((stamp, ids)) = live_index::generation(ids, |stamp| stamp > self.stamp) {
            self.volume = volume;
            self.stamp = stamp;
            self.sequence = 0;
            self.ended = false;
            self.pending = ids.into();
        }
    }

    /// Return the next dated chunk. A reused next-volume directory may start
    /// with keys from days ago; only a generation newer than this cursor is
    /// eligible, and all its available chunks are queued in sequence order.
    async fn next(&mut self, site: &str) -> Result<Option<DownloadedChunk>, Error> {
        if self.pending.is_empty() {
            if !self.ended {
                let ids = live_index::list(site, self.volume).await?;
                self.queue_current(ids);
            }
            if self.pending.is_empty() {
                let next = self.volume.next();
                let ids = live_index::list(site, next).await?;
                self.queue_next(next, ids);
            }
        }
        let Some(id) = self.pending.front() else {
            return Ok(None);
        };
        let chunk = fetch(site, id).await?;
        self.sequence = id.sequence();
        self.ended = id.chunk_type() == ChunkType::End;
        self.pending.pop_front();
        Ok(Some(chunk))
    }
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
        let join = match timeout(START_TIMEOUT, live_index::latest(&site)).await {
            Ok(Ok(Some(join))) => join,
            Ok(Ok(None)) => {
                if events
                    .send(Event::Silent {
                        site: site.clone(),
                        reason: "no archived volume or no chunks newer than it for this station"
                            .into(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                sleep(back_off).await;
                back_off = (back_off * 2).min(MAX_BACK_OFF);
                continue;
            }
            Ok(Err(e)) => {
                if !offline(&events, &site, format!("finding the latest volume: {e}")).await {
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
        let mut assembler = Assembler::default();
        let Some(newest) = join.ids.last() else {
            continue;
        };
        let volume = join.volume;
        let stamp = join.stamp;
        let mut cursor = Cursor::new(volume, stamp, newest);
        let replay = replay(&site, &join.ids).await;
        let archive_name = join
            .archived
            .key
            .rsplit('/')
            .next()
            .unwrap_or(&join.archived.key);
        live_log(
            &site,
            format_args!(
                "joined volume {} at chunk {}, age {}s, +{} from archive {} slot {}, replaying {} chunks",
                volume.as_number(),
                newest.name(),
                (Utc::now().naive_utc() - stamp).num_seconds(),
                slot_offset(join.header_slot, volume),
                archive_name,
                join.header_slot.as_number(),
                replay.len()
            ),
        );
        if backfilling.is_none() {
            backfilling = Some(AbortOnDrop(tokio::spawn(backfill(
                site.clone(),
                events.clone(),
                join,
                known.clone(),
            ))));
        }
        {
            let replay_known = skip_known.then_some(known.as_slice());
            for chunk in &replay {
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
            let next = wait_for_progress(deadline, cursor, async |mut cursor| {
                let result = match timeout(CALL_TIMEOUT, cursor.next(&site)).await {
                    Ok(Ok(None)) => {
                        sleep(IDLE).await;
                        None
                    }
                    result => Some(result),
                };
                (cursor, result)
            })
            .await;
            let Ok((next_cursor, next)) = next else {
                live_log(
                    &site,
                    format_args!(
                        "no chunk progress for {}s; rediscovering latest volume",
                        QUIET_RESTART.as_secs()
                    ),
                );
                break;
            };
            cursor = next_cursor;
            match next {
                Ok(Ok(Some(chunk))) => {
                    failures = 0;
                    // Walking old generations is not progress: rediscover if
                    // no genuinely recent chunk arrives within the deadline.
                    if Utc::now().naive_utc() - *chunk.identifier.date_time_prefix()
                        < chrono::Duration::minutes(30)
                    {
                        deadline = Instant::now() + QUIET_RESTART;
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
    use chrono::NaiveDate;
    use nexrad_data::volume::File;
    use std::fs;

    fn chunk_id(volume: usize, name: &str) -> ChunkIdentifier {
        ChunkIdentifier::from_name("KJAX".into(), VolumeIndex::new(volume), name.into(), None)
            .unwrap()
    }

    fn archive_vol(name: &str, stamp: NaiveDateTime) -> live_index::ArchiveVolume {
        live_index::ArchiveVolume {
            key: format!("2026/09/13/KJAX/{name}"),
            stamp,
        }
    }

    #[test]
    fn joining_an_ended_volume_advances_on_the_next_poll() {
        let newest = chunk_id(130, "20260913-231906-055-E");
        let join = live_index::Join {
            volume: VolumeIndex::new(130),
            stamp: *newest.date_time_prefix(),
            ids: vec![newest.clone()],
            archived: archive_vol("KJAX20260913_231906_V06", *newest.date_time_prefix()),
            header_slot: VolumeIndex::new(129),
        };
        let mut cursor = Cursor::new(join.volume, join.stamp, join.ids.last().unwrap());
        assert!(cursor.ended);
        cursor.queue_current(join.ids.clone());
        assert!(cursor.pending.is_empty());
        cursor.queue_next(
            VolumeIndex::new(131),
            vec![
                chunk_id(131, "20260913-232324-001-S"),
                chunk_id(131, "20260913-232324-002-I"),
            ],
        );
        assert_eq!(cursor.volume.as_number(), 131);
        assert_eq!(cursor.sequence, 0);
        assert!(!cursor.ended);
        assert_eq!(
            cursor
                .pending
                .iter()
                .map(ChunkIdentifier::sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let mut leftover = Cursor::new(join.volume, join.stamp, join.ids.last().unwrap());
        leftover.queue_next(
            VolumeIndex::new(131),
            vec![chunk_id(131, "20260909-050000-001-S")],
        );
        assert_eq!(leftover.volume.as_number(), 130);
        assert!(leftover.pending.is_empty());
    }

    #[test]
    fn backfill_picks_prior_volumes_from_the_archive_day() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 13).unwrap();
        let archived_stamp = day.and_hms_opt(12, 0, 0).unwrap();
        let today: Vec<_> = (0..3)
            .map(|h| {
                let stamp = day.and_hms_opt(10 + h, 0, 0).unwrap();
                archive_vol(&format!("KJAX20260913_{:02}0000_V06", 10 + h), stamp)
            })
            .collect();
        let yesterday: Vec<_> = (0..20)
            .map(|i| {
                let stamp = day.pred_opt().unwrap().and_hms_opt(i, 0, 0).unwrap();
                archive_vol(&format!("KJAX20260912_{i:02}0000_V06"), stamp)
            })
            .collect();
        let picked = prior_volumes(today.clone(), yesterday.clone(), archived_stamp, 12);
        assert_eq!(picked.len(), 12);
        assert!(picked.windows(2).all(|w| w[0].stamp >= w[1].stamp));
        assert!(picked.iter().all(|v| v.stamp <= archived_stamp));
        assert_eq!(picked[0].stamp, archived_stamp);

        let many_today: Vec<_> = (0..30)
            .map(|i| {
                let hour = i / 2;
                let minute = (i % 2) * 30;
                let stamp = day.and_hms_opt(hour, minute, 0).unwrap();
                archive_vol(&format!("KJAX20260913_{hour:02}{minute:02}00_V06"), stamp)
            })
            .collect();
        let from_today = prior_volumes(many_today.clone(), yesterday, archived_stamp, 12);
        assert_eq!(from_today.len(), 12);
        assert!(
            from_today
                .iter()
                .all(|v| v.key.contains("20260913") && v.stamp <= archived_stamp)
        );
        assert!(!from_today.iter().any(|v| v.key.contains("20260912")));
    }

    #[test]
    fn backfill_skips_a_catalogued_volume() {
        let stamp = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let ms = stamp.and_utc().timestamp_millis();
        assert!(catalogued_near(&[ms + 1_000], stamp));
        assert!(!catalogued_near(&[ms + 3_000], stamp));
    }

    #[test]
    fn next_volume_ignores_leftover_chunks_and_keeps_the_new_sweep_in_order() {
        let old = chunk_id(129, "20260909-050000-055-E");
        let mut cursor = Cursor::new(VolumeIndex::new(129), *old.date_time_prefix(), &old);
        cursor.queue_next(
            VolumeIndex::new(130),
            vec![chunk_id(130, "20260908-050500-001-S")],
        );
        assert_eq!(cursor.volume.as_number(), 129);
        assert!(cursor.pending.is_empty());
        cursor.queue_next(
            VolumeIndex::new(130),
            vec![
                chunk_id(130, "20260909-050500-001-S"),
                chunk_id(130, "20260912-170153-002-I"),
                chunk_id(130, "20260912-170153-001-S"),
            ],
        );
        assert_eq!(cursor.volume.as_number(), 130);
        assert_eq!(
            cursor
                .pending
                .iter()
                .map(ChunkIdentifier::sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            cursor
                .pending
                .iter()
                .all(|id| id.name().starts_with("20260912"))
        );
    }

    #[test]
    fn current_volume_does_not_replay_old_or_already_seen_chunks() {
        let current = chunk_id(130, "20260912-170153-004-I");
        let mut cursor = Cursor::new(VolumeIndex::new(130), *current.date_time_prefix(), &current);
        cursor.queue_current(vec![
            chunk_id(130, "20260909-050500-055-E"),
            current,
            chunk_id(130, "20260912-170153-006-I"),
            chunk_id(130, "20260912-170153-005-I"),
        ]);
        assert_eq!(
            cursor
                .pending
                .iter()
                .map(ChunkIdentifier::sequence)
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
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
