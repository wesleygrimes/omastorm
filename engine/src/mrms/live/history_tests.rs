use super::*;

fn ms(time: NaiveDateTime) -> i64 {
    time.and_utc().timestamp_millis()
}

fn objects(times: impl IntoIterator<Item = NaiveDateTime>) -> Vec<Object> {
    let mut objects: Vec<_> = times
        .into_iter()
        .map(|t| object(&key(t), t.date()).unwrap())
        .collect();
    objects.sort_by_key(|o| o.stamp);
    objects
}

#[test]
fn candidates_use_the_accepted_hour_and_count_retained_but_unlisted_frames() {
    let newest = stamp(1, 0, 7);
    let known = BTreeSet::from([ms(newest)]);
    let sparse = objects((0..30).map(|i| newest - chrono::Duration::minutes(i * 10)));
    assert_eq!(history_candidates(&sparse, &known).len(), 5);
    assert!(
        history_candidates(&sparse, &known)
            .iter()
            .all(|o| o.stamp > newest - chrono::Duration::hours(1))
    );
    let fast = objects((0..90).map(|i| newest - chrono::Duration::seconds(i * 30)));
    let recent = history_candidates(&fast, &known);
    assert_eq!(recent.len(), 29);
    assert_eq!(
        recent.first().unwrap().stamp,
        newest - chrono::Duration::seconds(30)
    );
    assert_eq!(
        recent.last().unwrap().stamp,
        newest - chrono::Duration::seconds(29 * 30)
    );
    let full: BTreeSet<_> = (0..30)
        .map(|i| ms(newest - chrono::Duration::seconds(i)))
        .collect();
    assert!(history_candidates(&sparse, &full).is_empty());
    // Corrupt newer objects do not advance the accepted window or exclude its tail.
    let fallback = objects([
        newest + chrono::Duration::minutes(4),
        newest,
        newest - chrono::Duration::minutes(59),
    ]);
    assert_eq!(history_candidates(&fallback, &known).len(), 1);
}

#[test]
fn an_hour_crosses_midnight_newest_first_and_restart_skips_retained_downloads() {
    runtime().block_on(async {
        tokio::time::pause();
        let newest = Utc::now().date_naive().and_hms_opt(0, 31, 7).unwrap();
        let times: Vec<_> = (0..30)
            .map(|i| newest - chrono::Duration::minutes(2 * i))
            .collect();
        let retained: Vec<_> = times.iter().map(|&t| ms(t)).collect();
        let keys: Arc<Vec<_>> = Arc::new(times.into_iter().map(key).collect());
        let gets = Arc::new(Mutex::new(Vec::new()));
        for initial in [vec![], retained.clone()] {
            let (tx, mut rx) = mpsc::channel(1);
            let keys = Arc::clone(&keys);
            let calls = Arc::clone(&gets);
            let task = tokio::spawn(poll_loop(
                GridSender::new(tx, 9),
                initial.clone(),
                move |day, _| {
                    let keys = keys
                        .iter()
                        .filter(|k| k.starts_with(&prefix(day)))
                        .cloned()
                        .collect::<Vec<_>>();
                    async move { Ok(page(&keys, None)) }
                },
                move |key| {
                    calls.lock().unwrap().push(key);
                    async { Ok(vec![]) }
                },
                Arc::new(decoded),
                Arc::new(Semaphore::new(1)),
                POLL_INTERVAL,
            ));
            if initial.is_empty() {
                for (i, expected) in retained.iter().enumerate() {
                    let message = rx.recv().await.unwrap();
                    let actual = match message.event {
                        GridEvent::Frame { start_ms, .. } if i == 0 => start_ms,
                        GridEvent::Backfill { start_ms, .. } if i > 0 => start_ms,
                        _ => panic!("expected newest first, then history"),
                    };
                    assert_eq!(actual, *expected);
                    message.published.unwrap().send(true).unwrap();
                }
            }
            let known = rx.recv().await.unwrap();
            assert!(matches!(known.event, GridEvent::Online { .. }));
            assert_eq!(gets.lock().unwrap().len(), 30);
            task.abort();
            let _ = task.await;
        }
    });
}

#[test]
fn slow_history_download_yields_to_live_poll_and_drops_its_pending_request() {
    runtime().block_on(async {
        tokio::time::pause();
        let today = Utc::now().date_naive();
        let newest = today.and_hms_opt(12, 0, 7).unwrap();
        let older = newest - chrono::Duration::minutes(2);
        let next = newest + chrono::Duration::minutes(2);
        let rounds = AtomicUsize::new(0);
        let gets = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::clone(&gets);
        let dropped = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::clone(&dropped);
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn(poll_loop(
            GridSender::new(tx, 1),
            vec![],
            move |day, _| {
                let keys = if day != today {
                    vec![]
                } else if rounds.fetch_add(1, Ordering::SeqCst) == 0 {
                    vec![key(older), key(newest)]
                } else {
                    vec![key(older), key(newest), key(next)]
                };
                async move { Ok(page(&keys, None)) }
            },
            move |requested| {
                calls.lock().unwrap().push(requested.clone());
                let cancelled = Arc::clone(&cancelled);
                async move {
                    if requested == key(older) {
                        let _guard = Guard(cancelled);
                        std::future::pending::<()>().await;
                    }
                    Ok(vec![])
                }
            },
            Arc::new(decoded),
            Arc::new(Semaphore::new(1)),
            POLL_INTERVAL,
        ));
        let first = rx.recv().await.unwrap();
        assert!(matches!(first.event, GridEvent::Frame { start_ms, .. } if start_ms == ms(newest)));
        first.published.unwrap().send(true).unwrap();
        // The fake clock advances to the deadline of the stalled historical GET.
        let second = rx.recv().await.unwrap();
        assert!(matches!(second.event, GridEvent::Frame { start_ms, .. } if start_ms == ms(next)));
        assert_eq!(
            *gets.lock().unwrap(),
            vec![key(newest), key(older), key(next)]
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
    });
}

#[test]
fn history_failures_are_isolated_and_unpublished_history_is_retried() {
    runtime().block_on(async {
        tokio::time::pause();
        let today = Utc::now().date_naive();
        let newest = today.and_hms_opt(12, 0, 7).unwrap();
        let bad = newest - chrono::Duration::minutes(2);
        let good = newest - chrono::Duration::minutes(4);
        let gets = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::clone(&gets);
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn(poll_loop(
            GridSender::new(tx, 1), vec![],
            move |day, _| async move { Ok(page(&if day == today { vec![key(good), key(bad), key(newest)] } else { vec![] }, None)) },
            move |requested| {
                calls.lock().unwrap().push(requested.clone());
                async move { if requested == key(bad) { Err("history missing".into()) } else { Ok(vec![]) } }
            },
            Arc::new(decoded), Arc::new(Semaphore::new(1)), POLL_INTERVAL,
        ));
        let first = rx.recv().await.unwrap();
        assert!(matches!(first.event, GridEvent::Frame { .. }));
        first.published.unwrap().send(true).unwrap();
        for persisted in [false, true] {
            let history = rx.recv().await.unwrap();
            assert!(matches!(history.event, GridEvent::Backfill { start_ms, .. } if start_ms == ms(good)));
            history.published.unwrap().send(persisted).unwrap();
            let live = rx.recv().await.unwrap();
            assert!(matches!(live.event, GridEvent::Online { .. }));
            live.published.unwrap().send(true).unwrap();
        }
        let next = rx.recv().await.unwrap();
        assert!(matches!(next.event, GridEvent::Online { .. }));
        assert_eq!(gets.lock().unwrap().iter().filter(|k| **k == key(newest)).count(), 1);
        assert_eq!(gets.lock().unwrap().iter().filter(|k| **k == key(good)).count(), 2);
        task.abort();
        let _ = task.await;
    });
}

#[test]
fn waiting_for_a_previous_decoder_consumes_the_history_deadline() {
    runtime().block_on(async {
        tokio::time::pause();
        let work = Arc::new(Semaphore::new(1));
        let previous_decode = work.acquire().await.unwrap();
        let obj = object(&key(stamp(12, 0, 7)), day()).unwrap();
        let result = load_object(
            &obj,
            &|_| async { panic!("no history GET after its deadline") },
            Arc::new(decoded),
            Arc::clone(&work),
            POLL_INTERVAL,
        )
        .await;
        assert!(result.unwrap_err().contains("timed out"));
        drop(previous_decode);
        assert_eq!(work.available_permits(), 1);
    });
}
