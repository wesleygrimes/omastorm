//! The per-station frame ring buffer (DESIGN.md, frame storage): a SQLite
//! catalog of station, time, elevation, product, and provenance under
//! `$XDG_CACHE_HOME/omastorm/frames/`, with the sweep and lookup PNGs as
//! files beside it. Storage is a catalog, not a transport: the UI never reads
//! it. The engine writes each complete live frame here, keeps the newest
//! `RING` per station, and on a station switch republishes the newest stored
//! frame to the runtime directory so something real shows while the first
//! live sweep loads; the timeline session reads the rest.

use crate::protocol::Frame;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Frames kept per station: about five hours of volume scans.
pub const RING: usize = 60;

pub struct Catalog {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

/// One catalogued frame as the timeline lists it.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub id: String,
    pub scan_time: String,
    /// `scanTime` in milliseconds since the epoch.
    pub start_ms: i64,
}

/// A stored frame with its texture bytes, ready to republish.
pub struct Stored {
    /// The frame as broadcast, with empty texture paths.
    pub frame: Frame,
    /// `scanTime` in milliseconds since the epoch, for `ageSeconds`.
    pub start_ms: i64,
    pub texture: Vec<u8>,
    pub azimuth_lut: Vec<u8>,
}

fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(format!("frame catalog: {e}"))
}

impl Catalog {
    /// Open or create the catalog in `dir`.
    pub fn open(dir: PathBuf) -> io::Result<Catalog> {
        fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("catalog.sqlite")).map_err(sql)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS frames (
                 id TEXT PRIMARY KEY,
                 site TEXT NOT NULL,
                 product TEXT NOT NULL,
                 elevation_deg REAL NOT NULL,
                 start_ms INTEGER NOT NULL,
                 scan_time TEXT NOT NULL,
                 sweep_end TEXT NOT NULL,
                 provenance TEXT NOT NULL,
                 stored_ms INTEGER NOT NULL,
                 frame TEXT NOT NULL,
                 texture TEXT NOT NULL,
                 azimuth_lut TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS frames_site_time ON frames (site, start_ms);",
        )
        .map_err(sql)?;
        Ok(Catalog {
            conn: Mutex::new(conn),
            dir,
        })
    }

    /// Record a complete frame and its textures, then drop the station's
    /// frames past the ring, files included. Replaces a frame of the same id.
    pub fn store(
        &self,
        site: &str,
        frame: &Frame,
        start_ms: i64,
        texture: &[u8],
        azimuth_lut: &[u8],
        provenance: &str,
    ) -> io::Result<()> {
        let mut record = frame.clone();
        record.texture.clear();
        record.azimuth_lut.clear();
        let texture_path = format!("{site}/{}-sweep.png", frame.id);
        let lut_path = format!("{site}/{}-azlut.png", frame.id);
        write(&self.dir.join(&texture_path), texture)?;
        write(&self.dir.join(&lut_path), azimuth_lut)?;
        let stored_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO frames (id, site, product, elevation_deg, start_ms, scan_time,
                 sweep_end, provenance, stored_ms, frame, texture, azimuth_lut)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                frame.id,
                site,
                frame.product,
                frame.elevation_deg,
                start_ms,
                frame.scan_time,
                frame.sweep_end,
                provenance,
                stored_ms,
                serde_json::to_string(&record)?,
                texture_path,
                lut_path,
            ],
        )
        .map_err(sql)?;
        // The ring: everything past the newest RING for this station and product.
        let expired: Vec<(String, String, String)> = conn
            .prepare(
                "SELECT id, texture, azimuth_lut FROM frames WHERE site = ?1 AND product = ?2
                 ORDER BY start_ms DESC, id DESC LIMIT -1 OFFSET ?3",
            )
            .map_err(sql)?
            .query_map(params![site, frame.product, RING as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        for (id, texture, lut) in expired {
            conn.execute("DELETE FROM frames WHERE id = ?1", params![id])
                .map_err(sql)?;
            for path in [texture, lut] {
                match fs::remove_file(self.dir.join(path)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    /// The station's frames of `product`, oldest first: the ring's contents,
    /// at most `RING`, as `state.timeline` lists them.
    pub fn list(&self, site: &str, product: &str) -> io::Result<Vec<Entry>> {
        let mut entries: Vec<Entry> = self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, scan_time, start_ms FROM frames WHERE site = ?1 AND product = ?2
                 ORDER BY start_ms DESC, id DESC LIMIT ?3",
            )
            .map_err(sql)?
            .query_map(params![site, product, RING as i64], |row| {
                Ok(Entry {
                    id: row.get(0)?,
                    scan_time: row.get(1)?,
                    start_ms: row.get(2)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        entries.reverse();
        Ok(entries)
    }

    /// The stored frame `id` with its textures, if the ring still has it.
    pub fn load(&self, id: &str) -> io::Result<Option<Stored>> {
        let row = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT frame, start_ms, texture, azimuth_lut FROM frames WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(sql)?;
        self.read(row)
    }

    fn read(&self, row: Option<(String, i64, String, String)>) -> io::Result<Option<Stored>> {
        let Some((frame, start_ms, texture, lut)) = row else {
            return Ok(None);
        };
        Ok(Some(Stored {
            frame: serde_json::from_str(&frame)?,
            start_ms,
            texture: fs::read(self.dir.join(texture))?,
            azimuth_lut: fs::read(self.dir.join(lut))?,
        }))
    }

    /// How many frames the station has.
    #[cfg(test)]
    pub fn count(&self, site: &str) -> io::Result<usize> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE site = ?1 AND product = ?2",
                params![site, "REF"],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(sql)
    }
}

/// Write by temp-and-rename so a crash leaves no half file under a real name.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FrameStatus, Geometry};

    fn frame(site: &str, minute: u32) -> Frame {
        Frame {
            id: format!("{site}-20260906T12{minute:02}00Z-e0"),
            product: "REF".into(),
            product_name: "Reflectivity".into(),
            units: "dBZ".into(),
            elevation_deg: 0.5,
            scan_time: format!("2026-09-06T12:{minute:02}:00Z"),
            sweep_end: format!("2026-09-06T12:{minute:02}:20Z"),
            status: FrameStatus::Complete,
            texture: "tex/runtime-path.png".into(),
            azimuth_lut: "tex/runtime-lut.png".into(),
            rays: 720,
            gates: 1832,
            first_gate_m: 2125,
            gate_spacing_m: 250,
            scale: 2.0,
            offset: 66.0,
            site: Geometry {
                lat: 35.0,
                lon: -97.0,
                alt_m: 380.0,
            },
            palette: vec!["#000000".into()],
            bounds: vec![0, 10],
        }
    }

    #[test]
    fn the_ring_keeps_the_newest_frames_and_their_files() {
        let dir = std::env::temp_dir().join(format!("omastorm-catalog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let catalog = Catalog::open(dir.clone()).unwrap();
        assert!(catalog.list("KTLX", "REF").unwrap().is_empty());
        for minute in 0..(RING as u32 + 3) {
            let f = frame("KTLX", minute);
            catalog
                .store(
                    "KTLX",
                    &f,
                    1_757_160_000_000 + i64::from(minute) * 60_000,
                    &[minute as u8; 16],
                    &[1, 2, 3],
                    "unidata-nexrad-level2-chunks/KTLX/001/a..b",
                )
                .unwrap();
        }
        // Another station's ring is its own.
        catalog
            .store("KAMX", &frame("KAMX", 5), 1, &[9], &[9], "p")
            .unwrap();
        assert_eq!(catalog.count("KTLX").unwrap(), RING);
        assert_eq!(catalog.count("KAMX").unwrap(), 1);
        let listed = catalog.list("KTLX", "REF").unwrap();
        let newest = catalog.load(&listed[RING - 1].id).unwrap().unwrap();
        assert_eq!(newest.frame.id, frame("KTLX", RING as u32 + 2).id);
        // Runtime paths are not stored; the caller republishes.
        assert_eq!(newest.frame.texture, "");
        assert_eq!(newest.frame.azimuth_lut, "");
        assert_eq!(newest.texture, [(RING as u8 + 2); 16]);
        assert_eq!(newest.azimuth_lut, [1, 2, 3]);
        assert_eq!(
            newest.start_ms,
            1_757_160_000_000 + (RING as i64 + 2) * 60_000
        );
        let files: Vec<_> = fs::read_dir(dir.join("KTLX"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            files.len(),
            RING * 2,
            "one sweep and one lookup per kept frame"
        );
        assert!(
            !files
                .iter()
                .any(|f| f.contains("T120000Z") || f.contains("T120200Z"))
        );
        assert!(files.iter().any(|f| f.contains("T120300Z")));
        // The listing is the ring oldest first; a frame loads by id until it
        // falls off the ring.
        let listed = catalog.list("KTLX", "REF").unwrap();
        assert_eq!(listed.len(), RING);
        assert_eq!(listed[0].id, frame("KTLX", 3).id);
        assert_eq!(listed[0].scan_time, "2026-09-06T12:03:00Z");
        assert_eq!(listed[0].start_ms, 1_757_160_000_000 + 3 * 60_000);
        assert_eq!(listed[RING - 1].id, newest.frame.id);
        assert!(listed.windows(2).all(|w| w[0].start_ms < w[1].start_ms));
        let loaded = catalog.load(&listed[1].id).unwrap().unwrap();
        assert_eq!(loaded.frame.id, frame("KTLX", 4).id);
        assert_eq!(loaded.texture, [4; 16]);
        assert!(catalog.load(&frame("KTLX", 1).id).unwrap().is_none());
        assert_eq!(catalog.list("KAMX", "REF").unwrap().len(), 1);
        assert!(catalog.list("KOUN", "REF").unwrap().is_empty());
        // Reopening sees the same rows.
        drop(catalog);
        let again = Catalog::open(dir.clone()).unwrap();
        assert_eq!(again.count("KTLX").unwrap(), RING);
        let _ = fs::remove_dir_all(&dir);
    }
}
