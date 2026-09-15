//! The archive volume header names the live slot.
//!
//! List the last finished file in the archive bucket, read its 24-byte
//! header for the 1–999 volume number, then list that slot plus the next
//! two in the chunk bucket. Name timestamps still decide within a slot:
//! leftover keys in a reused folder lose to a generation newer than the
//! archive file. The join does not judge freshness.

use chrono::{NaiveDate, NaiveDateTime, Utc};
use nexrad_data::aws::realtime::{ChunkIdentifier, VolumeIndex, list_chunks_in_volume};
use nexrad_data::result::Result;
use nexrad_data::volume::File;
use std::future::Future;
use xml::reader::{EventReader, XmlEvent};

pub const LIST_LIMIT: usize = 1000;
const ARCHIVE_HOST: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";
const CHUNK_HOST: &str = "https://unidata-nexrad-level2-chunks.s3.amazonaws.com";

/// A finished volume in the archive bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveVolume {
    pub key: String,
    pub stamp: NaiveDateTime,
}

/// The live slot named by an archive header, with that generation's chunks.
#[derive(Debug, Clone)]
pub struct Join {
    pub volume: VolumeIndex,
    pub stamp: NaiveDateTime,
    pub ids: Vec<ChunkIdentifier>,
    pub archived: ArchiveVolume,
    /// The header's 1–999 slot, so the joined line can print +1 / +2 / +3.
    pub header_slot: VolumeIndex,
}

/// The newest dated generation in one directory, with its chunks in order.
/// `accept` lets backfill exclude generations newer than the active scan and
/// lets a live transition exclude leftover chunks from a previous rotation.
pub fn generation(
    ids: Vec<ChunkIdentifier>,
    accept: impl Fn(NaiveDateTime) -> bool,
) -> Option<(NaiveDateTime, Vec<ChunkIdentifier>)> {
    let stamp = ids
        .iter()
        .map(|id| *id.date_time_prefix())
        .filter(|stamp| accept(*stamp))
        .max()?;
    let mut selected: Vec<_> = ids
        .into_iter()
        .filter(|id| *id.date_time_prefix() == stamp)
        .collect();
    selected.sort_by_key(ChunkIdentifier::sequence);
    Some((stamp, selected))
}

pub async fn list(site: &str, volume: VolumeIndex) -> Result<Vec<ChunkIdentifier>> {
    list_chunks_in_volume(site, volume, LIST_LIMIT).await
}

fn archive_stamp(site: &str, filename: &str) -> Option<NaiveDateTime> {
    let rest = filename.strip_prefix(site)?;
    let rest = rest.strip_suffix("_V06")?;
    NaiveDateTime::parse_from_str(rest, "%Y%m%d_%H%M%S").ok()
}

fn parse_archive_listing(
    body: &str,
    site: &str,
) -> std::result::Result<Vec<ArchiveVolume>, String> {
    let mut volumes = Vec::new();
    let mut in_key = false;
    let mut key = None::<String>;
    let mut truncated = None::<String>;
    let mut reading_truncated = false;
    for event in EventReader::new(body.as_bytes()) {
        match event.map_err(|e| format!("reading archive listing: {e}"))? {
            XmlEvent::StartElement { name, .. } => match name.local_name.as_str() {
                "Key" => {
                    in_key = true;
                    key = Some(String::new());
                }
                "IsTruncated" => {
                    truncated = Some(String::new());
                    reading_truncated = true;
                }
                _ => {}
            },
            XmlEvent::Characters(value) => {
                if let Some(key) = key.as_mut()
                    && in_key
                {
                    key.push_str(&value);
                } else if reading_truncated {
                    truncated.as_mut().unwrap().push_str(&value);
                }
            }
            XmlEvent::EndElement { name } => match name.local_name.as_str() {
                "Key" => {
                    in_key = false;
                    if let Some(value) = key.take() {
                        let filename = value.rsplit('/').next().unwrap_or(&value);
                        if let Some(stamp) = archive_stamp(site, filename) {
                            volumes.push(ArchiveVolume { key: value, stamp });
                        }
                    }
                }
                "IsTruncated" => reading_truncated = false,
                _ => {}
            },
            _ => {}
        }
    }
    if truncated.as_deref() != Some("false") {
        return Err("archive listing is truncated or missing IsTruncated".into());
    }
    volumes.sort_by_key(|volume| volume.stamp);
    Ok(volumes)
}

/// Today's (or a given day's) finished `_V06` volumes, oldest first.
pub async fn archive_volumes(
    site: &str,
    day: NaiveDate,
) -> std::result::Result<Vec<ArchiveVolume>, String> {
    let prefix = format!("{}/{}/", day.format("%Y/%m/%d"), site);
    let url = format!("{ARCHIVE_HOST}?list-type=2&prefix={prefix}&max-keys=1000");
    let body = reqwest::get(url)
        .await
        .map_err(|e| format!("listing archive volumes: {e}"))?
        .error_for_status()
        .map_err(|e| format!("listing archive volumes: {e}"))?
        .text()
        .await
        .map_err(|e| format!("reading archive listing: {e}"))?;
    parse_archive_listing(&body, site)
}

/// The last finished archive file: today in UTC, or yesterday if today is empty.
pub async fn last_archived<F, Fut>(
    now: NaiveDateTime,
    list: F,
) -> std::result::Result<Option<ArchiveVolume>, String>
where
    F: Fn(NaiveDate) -> Fut,
    Fut: Future<Output = std::result::Result<Vec<ArchiveVolume>, String>>,
{
    let today = now.date();
    let mut volumes = list(today).await?;
    if volumes.is_empty() {
        let yesterday = today
            .pred_opt()
            .ok_or_else(|| "archive day has no yesterday".to_string())?;
        volumes = list(yesterday).await?;
    }
    volumes.sort_by_key(|volume| volume.stamp);
    Ok(volumes.pop())
}

fn slot_from_header(bytes: &[u8]) -> std::result::Result<VolumeIndex, String> {
    if bytes.len() < 24 {
        return Err("archive header is shorter than 24 bytes".into());
    }
    let file = File::new(bytes[..24].to_vec());
    let header = file
        .header()
        .ok_or_else(|| "archive header could not be parsed".to_string())?;
    let ext = header
        .extension_number()
        .ok_or_else(|| "archive header has no volume number".to_string())?;
    let n: usize = ext
        .parse()
        .map_err(|_| format!("archive header volume number {ext:?} is not digits"))?;
    if !(1..=999).contains(&n) {
        return Err(format!("archive header volume number {n} is out of range"));
    }
    Ok(VolumeIndex::new(n))
}

/// The 1–999 slot stored in the first 24 bytes of an archive object.
pub async fn archived_slot(key: &str) -> std::result::Result<VolumeIndex, String> {
    let url = format!("{ARCHIVE_HOST}/{key}");
    let response = reqwest::Client::new()
        .get(&url)
        .header("Range", "bytes=0-23")
        .send()
        .await
        .map_err(|e| format!("reading archive header {key}: {e}"))?;
    let status = response.status();
    if status != reqwest::StatusCode::PARTIAL_CONTENT && status != reqwest::StatusCode::OK {
        return Err(format!("archive header {key}: {status}"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("reading archive header {key}: {e}"))?;
    slot_from_header(&bytes)
}

fn parse_chunk_keys(
    body: &str,
    site: &str,
    volume: VolumeIndex,
) -> std::result::Result<Vec<ChunkIdentifier>, String> {
    let mut ids = Vec::new();
    let mut in_key = false;
    let mut key = None::<String>;
    for event in EventReader::new(body.as_bytes()) {
        match event.map_err(|e| format!("reading chunk listing: {e}"))? {
            XmlEvent::StartElement { name, .. } if name.local_name == "Key" => {
                in_key = true;
                key = Some(String::new());
            }
            XmlEvent::Characters(value) if in_key => {
                if let Some(key) = key.as_mut() {
                    key.push_str(&value);
                }
            }
            XmlEvent::EndElement { name } if name.local_name == "Key" => {
                in_key = false;
                if let Some(value) = key.take() {
                    let name = value.rsplit('/').next().unwrap_or(&value);
                    if let Ok(id) =
                        ChunkIdentifier::from_name(site.to_owned(), volume, name.to_owned(), None)
                    {
                        ids.push(id);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ids)
}

/// Chunk keys in `volume` whose names sort after `after`'s clock.
pub async fn list_after(
    site: &str,
    volume: VolumeIndex,
    after: NaiveDateTime,
) -> std::result::Result<Vec<ChunkIdentifier>, String> {
    let prefix = format!("{}/{}/", site, volume.as_number());
    let start = format!("{prefix}{}", after.format("%Y%m%d-%H%M%S"));
    let url = format!(
        "{CHUNK_HOST}?list-type=2&prefix={prefix}&start-after={start}&max-keys={LIST_LIMIT}"
    );
    let body = reqwest::get(url)
        .await
        .map_err(|e| format!("listing volume {}: {e}", volume.as_number()))?
        .error_for_status()
        .map_err(|e| format!("listing volume {}: {e}", volume.as_number()))?
        .text()
        .await
        .map_err(|e| format!("reading volume {}: {e}", volume.as_number()))?;
    parse_chunk_keys(&body, site, volume)
}

/// The first candidate whose newest accepted generation is after `after`.
fn pick_join(
    candidates: Vec<(VolumeIndex, Vec<ChunkIdentifier>)>,
    after: NaiveDateTime,
) -> Option<(VolumeIndex, NaiveDateTime, Vec<ChunkIdentifier>)> {
    for (volume, ids) in candidates {
        if let Some((stamp, ids)) = generation(ids, |stamp| stamp > after) {
            return Some((volume, stamp, ids));
        }
    }
    None
}

/// The live generation: archive header, then the first later slot with newer names.
pub async fn latest(site: &str) -> std::result::Result<Option<Join>, String> {
    let Some(archived) =
        last_archived(Utc::now().naive_utc(), |day| archive_volumes(site, day)).await?
    else {
        return Ok(None);
    };
    let header_slot = archived_slot(&archived.key).await?;
    let mut slot = header_slot.next();
    for _ in 0..3 {
        let ids = list_after(site, slot, archived.stamp).await?;
        if let Some((volume, stamp, ids)) = pick_join(vec![(slot, ids)], archived.stamp) {
            return Ok(Some(Join {
                volume,
                stamp,
                ids,
                archived,
                header_slot,
            }));
        }
        slot = slot.next();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeDelta};
    use std::cell::Cell;
    use std::fs;

    fn stamp(day: u32, hour: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
    }

    fn id(volume: usize, day: u32, hour: u32, sequence: usize) -> ChunkIdentifier {
        let name = format!("202609{day:02}-{hour:02}0000-{sequence:03}-I");
        ChunkIdentifier::from_name("KJAX".into(), VolumeIndex::new(volume), name, None).unwrap()
    }

    fn chunk(volume: usize, name: &str) -> ChunkIdentifier {
        ChunkIdentifier::from_name("KFCX".into(), VolumeIndex::new(volume), name.into(), None)
            .unwrap()
    }

    fn archive(name: &str, stamp: NaiveDateTime) -> ArchiveVolume {
        ArchiveVolume {
            key: format!("2026/09/13/KFCX/{name}"),
            stamp,
        }
    }

    fn header_bytes(ext: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 24];
        bytes[..9].copy_from_slice(b"AR2V0006.");
        let n = ext.len().min(3);
        bytes[9..9 + n].copy_from_slice(&ext[..n]);
        bytes
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn mixed_volume_selects_only_its_newest_generation() {
        let mixed = vec![id(130, 9, 5, 1), id(130, 12, 17, 2), id(130, 12, 17, 1)];
        let selected = generation(mixed.clone(), |_| true).unwrap();
        assert_eq!(selected.0, stamp(12, 17));
        assert_eq!(
            selected
                .1
                .iter()
                .map(ChunkIdentifier::sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let backfill = generation(mixed, |time| time < stamp(12, 17)).unwrap();
        assert_eq!(backfill.0, stamp(9, 5));
        assert_eq!(backfill.1.len(), 1);
    }

    #[test]
    fn midnight_falls_back_to_yesterday() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 13).unwrap();
        let now = today.and_hms_opt(0, 30, 0).unwrap();
        let yesterday_first = archive(
            "KFCX20260912_220000_V06",
            today.pred_opt().unwrap().and_hms_opt(22, 0, 0).unwrap(),
        );
        let yesterday_last = archive(
            "KFCX20260912_233000_V06",
            today.pred_opt().unwrap().and_hms_opt(23, 30, 0).unwrap(),
        );
        let today_file = archive("KFCX20260913_001500_V06", now);

        let calls = Cell::new(0);
        let got = block_on(last_archived(now, |day| {
            calls.set(calls.get() + 1);
            let empty = day == today;
            let first = yesterday_first.clone();
            let last = yesterday_last.clone();
            async move {
                if empty {
                    Ok(vec![])
                } else {
                    Ok(vec![first, last])
                }
            }
        }))
        .unwrap();
        assert_eq!(got, Some(yesterday_last.clone()));
        assert_eq!(calls.get(), 2);

        let calls = Cell::new(0);
        let got = block_on(last_archived(now, |day| {
            calls.set(calls.get() + 1);
            assert_eq!(day, today, "files today must not ask for yesterday");
            let today_file = today_file.clone();
            async move { Ok(vec![today_file]) }
        }))
        .unwrap();
        assert_eq!(got, Some(today_file));
        assert_eq!(calls.get(), 1);

        let got = block_on(last_archived(now, |_| async { Ok(Vec::new()) })).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn archive_listing_keeps_only_volume_files() {
        let body = r#"<ListBucketResult><IsTruncated>false</IsTruncated>
            <Contents><Key>2026/09/13/KFCX/KFCX20260913_231906_V06</Key></Contents>
            <Contents><Key>2026/09/13/KFCX/KFCX20260913_231906_MDM</Key></Contents>
            <Contents><Key>2026/09/13/KFCX/KFCX20260913_120000_V06.gz</Key></Contents>
            <Contents><Key>2026/09/13/KJAX/KJAX20260913_231906_V06</Key></Contents>
            </ListBucketResult>"#;
        let volumes = parse_archive_listing(body, "KFCX").unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].key, "2026/09/13/KFCX/KFCX20260913_231906_V06");
        assert_eq!(
            volumes[0].stamp,
            NaiveDate::from_ymd_opt(2026, 9, 13)
                .unwrap()
                .and_hms_opt(23, 19, 6)
                .unwrap()
        );
        assert!(parse_archive_listing(&body.replace("false", "true"), "KFCX").is_err());
    }

    #[test]
    fn header_names_the_slot() {
        const FIXTURE: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/KTLX20130520_201643_V06.gz"
        );
        let raw = fs::read(FIXTURE).unwrap();
        let data = File::new(raw).decompress().unwrap().data().to_vec();
        assert_eq!(slot_from_header(&data[..24]).unwrap().as_number(), 939);

        assert!(slot_from_header(&[0u8; 10]).is_err());
        assert!(slot_from_header(&header_bytes(b"abc")).is_err());
        assert!(slot_from_header(&header_bytes(b"000")).is_err());
        // 1000 cannot fit in the 3-byte ASCII field; overlaying "1000"
        // still parses as 100. The 1..=999 guard is the 000 case above.
        let html = b"<!DOCTYPE html><html><bo";
        assert_eq!(html.len(), 24);
        assert!(slot_from_header(html).is_err());
    }

    #[test]
    fn join_skips_a_slot_of_leftovers() {
        let after = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(23, 19, 6)
            .unwrap();
        let leftover = chunk(412, "20260909-050000-001-S");
        let fresh = [
            chunk(413, "20260913-232324-001-S"),
            chunk(413, "20260913-232324-002-I"),
        ];
        let (volume, stamp, ids) = pick_join(
            vec![
                (VolumeIndex::new(412), vec![leftover]),
                (VolumeIndex::new(413), fresh.to_vec()),
            ],
            after,
        )
        .unwrap();
        assert_eq!(volume.as_number(), 413);
        assert_eq!(stamp, *fresh[0].date_time_prefix());
        assert_eq!(
            ids.iter()
                .map(ChunkIdentifier::sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn join_takes_the_first_slot_with_newer_names() {
        let after = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(23, 19, 6)
            .unwrap();
        let (volume, _, ids) = pick_join(
            vec![
                (
                    VolumeIndex::new(412),
                    vec![
                        chunk(412, "20260909-050000-001-S"),
                        chunk(412, "20260913-232324-001-S"),
                        chunk(412, "20260913-232324-002-I"),
                    ],
                ),
                (
                    VolumeIndex::new(413),
                    vec![chunk(413, "20260913-233000-001-S")],
                ),
            ],
            after,
        )
        .unwrap();
        assert_eq!(volume.as_number(), 412);
        assert_eq!(
            ids.iter().map(ChunkIdentifier::name).collect::<Vec<_>>(),
            vec!["20260913-232324-001-S", "20260913-232324-002-I"]
        );
    }

    #[test]
    fn join_wraps_from_999_to_1() {
        let after = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(23, 19, 6)
            .unwrap();
        assert_eq!(VolumeIndex::new(999).next().as_number(), 1);
        let (volume, _, _) = pick_join(
            vec![
                (VolumeIndex::new(1), vec![chunk(1, "20260913-232324-001-S")]),
                (VolumeIndex::new(2), vec![]),
                (VolumeIndex::new(3), vec![]),
            ],
            after,
        )
        .unwrap();
        assert_eq!(volume.as_number(), 1);
    }

    #[test]
    fn join_gives_up_after_three_slots() {
        let after = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(23, 19, 6)
            .unwrap();
        assert_eq!(
            pick_join(
                vec![
                    (
                        VolumeIndex::new(412),
                        vec![chunk(412, "20260909-050000-001-S")]
                    ),
                    (
                        VolumeIndex::new(413),
                        vec![chunk(413, "20260909-060000-001-S")]
                    ),
                    (VolumeIndex::new(414), vec![]),
                ],
                after,
            ),
            None
        );
    }

    #[test]
    fn join_does_not_judge_freshness() {
        let now = NaiveDate::from_ymd_opt(2026, 9, 13)
            .unwrap()
            .and_hms_opt(18, 0, 0)
            .unwrap();
        let archived = now - TimeDelta::hours(6);
        let stale = now - TimeDelta::hours(5);
        let name = format!("{}-001-S", stale.format("%Y%m%d-%H%M%S"));
        let (volume, stamp, _) = pick_join(
            vec![(VolumeIndex::new(412), vec![chunk(412, &name)])],
            archived,
        )
        .unwrap();
        assert_eq!(volume.as_number(), 412);
        assert_eq!(stamp, stale);
    }
}
