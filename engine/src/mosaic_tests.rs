//! Exercise grid history through the actual shared engine state and commands.
use super::*;

struct Harness {
    shared: Arc<Mutex<Shared>>,
    dir: PathBuf,
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn harness(name: &str) -> Harness {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../target/test-mosaic-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let template = serde_json::from_str(include_str!("../data/product.json")).unwrap();
    let registry = source::SourceRegistry::compiled();
    let mut state = initial_state(
        None,
        protocol::Osm {
            status: protocol::OsmStatus::Unavailable,
            source: String::new(),
            version: String::new(),
            attribution: String::new(),
        },
        Mode::Live,
        ConnectionStatus::Loading,
        Some(Selection {
            source_id: mrms::ID.into(),
            target: AdapterTarget::Mosaic,
        }),
    );
    state.navigation.locked = true;
    let shared = Arc::new(Mutex::new(Shared {
        state,
        tiles: tiles::Store::open(&dir, "test").unwrap(),
        clients: vec![],
        next_client: 0,
        template,
        sites: registry.hello_sites(),
        registry,
        last_center: Some((34.0, -100.0)),
        mosaic: vec![],
        dir: dir.clone(),
        catalog: Arc::new(catalog::Catalog::open(dir.join("catalog")).unwrap()),
        live: None,
        last_live_restart: Instant::now(),
        events: mpsc::channel(1).0,
        grid_events: mpsc::channel(1).0,
        grid_session: 1,
        frame_ms: None,
        timeline: Timeline::default(),
        pending: None,
        wake: Arc::new(Notify::new()),
        last_broadcast: String::new(),
    }));
    Harness { shared, dir }
}
fn frame(ms: i64) -> (MosaicFrame, Vec<u8>) {
    let (mut frame, png) = mrms::Mrms::new().loading_placeholder().unwrap();
    frame.id = format!("{}-{}", mrms::ID, compact(ms));
    frame.scan_time = iso(ms);
    frame.status = FrameStatus::Complete;
    (frame, png)
}
fn add(shared: &mut Shared, ms: i64, live: bool) {
    let (frame, png) = frame(ms);
    shared.accept_mosaic(frame, png, ms, live).unwrap();
}
fn shown(shared: &Shared) -> &MosaicFrame {
    match shared.state.frame.as_ref().unwrap() {
        FrameWire::Mosaic(frame) => frame,
        _ => panic!("expected grid frame"),
    }
}
fn assert_files(shared: &Shared) {
    assert_eq!(shared.mosaic.len(), shared.timeline.len());
    for entry in &shared.timeline.stored {
        let frame = shared.mosaic.iter().find(|f| f.id == entry.id).unwrap();
        assert!(shared.dir.join(&frame.texture).is_file());
    }
}

#[test]
fn backfill_and_count_eviction_preserve_the_selected_frame_until_it_expires() {
    let h = harness("pin");
    let mut shared = h.shared.lock().unwrap();
    let newest = now_ms() / 1000 * 1000;
    add(&mut shared, newest, true);
    for i in 1..30 {
        add(&mut shared, newest - i * 120_000, false);
    }
    assert_eq!(shared.timeline.len(), 30);
    assert_eq!(shown(&shared).scan_time, iso(newest));
    assert_files(&shared);
    let chosen = frame(newest - 20 * 60_000).0.id;
    assert!(shared.apply(Command::Seek { id: chosen.clone() }).is_none());
    let path = shown(&shared).texture.clone();
    // Removing an older frame must not move this pin.
    add(&mut shared, newest + 120_000, true);
    assert_eq!(shown(&shared).id, chosen);
    assert_eq!(shown(&shared).texture, path);
    assert_eq!(shared.timeline.len(), 30);
    assert!(!shared.state.playing);
    assert!(shared.state.navigation.locked);
    assert_eq!(shared.last_center, Some((34.0, -100.0)));
    // Home pins the oldest; the next live frame evicts it and selects the new oldest.
    shared.apply(Command::Step { delta: -1000 });
    let oldest = shown(&shared).id.clone();
    add(&mut shared, newest + 240_000, true);
    assert_ne!(shown(&shared).id, oldest);
    assert_eq!(Some(shown(&shared).id.as_str()), shared.timeline.id_at(0));
    // Late expired and duplicate replies never publish more files or roll back age.
    let before = fs::read_dir(h.dir.join("tex")).unwrap().count();
    add(&mut shared, newest - 56 * 60_000, false); // exact one-hour cutoff
    add(&mut shared, newest - 120 * 60_000, true);
    add(&mut shared, newest, false);
    assert_eq!(fs::read_dir(h.dir.join("tex")).unwrap().count(), before);
    assert_eq!(shared.frame_ms, Some(newest + 240_000));
    assert_files(&shared);
}

#[test]
fn duration_and_count_are_independent_and_outages_do_not_erase_history() {
    let h = harness("window");
    let mut shared = h.shared.lock().unwrap();
    // Deliberately stale: wall clock must not evict this useful loop.
    let newest = now_ms() / 1000 * 1000 - 3 * 60 * 60_000;
    add(&mut shared, newest, true);
    for i in 1..30 {
        add(&mut shared, newest - i * 10 * 60_000, false);
    }
    assert_eq!(shared.timeline.len(), 6);
    assert_eq!(
        shared.state.connection.status,
        ConnectionStatus::Unavailable
    );
    let ids = shared.timeline.entries();
    report_grid(&mut shared, mrms::ID, "outage", ConnectionStatus::Offline);
    shared.snapshot();
    assert_eq!(shared.state.connection.status, ConnectionStatus::Offline);
    assert_eq!(shared.timeline.entries(), ids);
    assert_eq!(shared.frame_ms, Some(newest));
    // Resume on the same selection; the newly accepted observation expires the old loop.
    add(&mut shared, newest + 3 * 60 * 60_000, true);
    assert_eq!(shared.timeline.len(), 1);
    assert_eq!(shared.state.connection.status, ConnectionStatus::Ok);
    let newest = shared.frame_ms.unwrap();
    for i in 1..90 {
        add(&mut shared, newest - i * 30_000, false);
    }
    assert_eq!(shared.timeline.len(), 30);
    assert_eq!(shared.timeline.stored[0].start_ms, newest - 29 * 30_000);
    assert_eq!(shared.mosaic_source_id(), Some(mrms::ID));
    assert_files(&shared);
}

#[test]
fn playback_navigation_uses_published_paths_and_cleanup_keeps_only_references() {
    let h = harness("playback");
    {
        let mut shared = h.shared.lock().unwrap();
        let newest = now_ms() / 1000 * 1000;
        add(&mut shared, newest, true);
        add(&mut shared, newest - 120_000, false);
        shared.apply(Command::Step { delta: -1000 }); // Home
        let pinned = shown(&shared).id.clone();
        add(&mut shared, newest - 240_000, false);
        assert_eq!(shown(&shared).id, pinned);
        shared.apply(Command::Play);
        assert!(shared.state.playing);
        shared.tick();
        assert_eq!(shown(&shared).scan_time, iso(newest));
        shared.tick(); // wrap
        assert_eq!(shown(&shared).scan_time, iso(newest - 240_000));
        shared.apply(Command::Pause);
        assert!(!shared.state.playing);
        shared.tick();
        assert_eq!(shown(&shared).scan_time, iso(newest - 240_000));
        shared.apply(Command::Step { delta: 1000 }); // End
        assert!(shared.timeline.following());
        let paths: Vec<_> = shared.mosaic.iter().map(|f| f.texture.clone()).collect();
        for _ in 0..3 {
            shared.apply(Command::Play);
            for _ in 0..3 {
                shared.tick();
            }
        }
        assert_eq!(
            shared
                .mosaic
                .iter()
                .map(|f| f.texture.clone())
                .collect::<Vec<_>>(),
            paths
        );
        assert_files(&shared);
    }
    let mut retirement = Retirement::default();
    cleanup(&h.dir, &h.shared, &mut retirement).unwrap();
    assert!(retirement.unreferenced.is_empty());
    h.shared.lock().unwrap().clear_selection();
    cleanup(&h.dir, &h.shared, &mut retirement).unwrap();
    assert_eq!(retirement.unreferenced.len(), 3);
    for since in retirement.unreferenced.values_mut() {
        *since -= RETIRE_AFTER;
    }
    cleanup(&h.dir, &h.shared, &mut retirement).unwrap();
    assert_eq!(fs::read_dir(h.dir.join("tex")).unwrap().count(), 0);
}

#[test]
fn history_publication_failure_and_superseded_events_preserve_live_state() {
    let h = harness("events");
    let newest = now_ms() / 1000 * 1000;
    {
        let mut shared = h.shared.lock().unwrap();
        add(&mut shared, newest, true);
        // Fill the allowance with a sparse retired file, without allocating RAM.
        let file = fs::File::create(h.dir.join("tex/mosaic-mrms-conus-retired.png")).unwrap();
        file.set_len(1 << 30).unwrap();
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (tx, rx) = mpsc::channel(1);
            let task = tokio::spawn(mosaic_events(Arc::clone(&h.shared), rx));
            let (frame, texture) = frame(newest - 120_000);
            let ack = source::GridSender::new(tx.clone(), 1)
                .reserve()
                .await
                .unwrap()
                .send(source::GridEvent::Backfill {
                    source_id: mrms::ID.into(),
                    frame: Box::new(frame),
                    texture,
                    start_ms: newest - 120_000,
                });
            assert!(!ack.await.unwrap());
            {
                let shared = h.shared.lock().unwrap();
                assert_eq!(shared.state.connection.status, ConnectionStatus::Ok);
                assert_eq!(shared.timeline.len(), 1);
                assert_files(&shared);
            }
            h.shared.lock().unwrap().grid_session = 3;
            let stale = source::GridSender::new(tx.clone(), 1)
                .reserve()
                .await
                .unwrap()
                .send(source::GridEvent::Offline {
                    source_id: mrms::ID.into(),
                    reason: "superseded".into(),
                });
            assert!(stale.await.is_err());
            assert_eq!(
                h.shared.lock().unwrap().state.connection.status,
                ConnectionStatus::Ok
            );
            drop(tx);
            task.await.unwrap();
        });
}
