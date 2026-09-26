use super::*;
use crate::source::{GridMessage, SourceRegistry};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
}
fn key(stamp: NaiveDateTime) -> String {
    format!(
        "{}MRMS_{PRODUCT}_{}.grib2.gz",
        prefix(stamp.date()),
        stamp.format("%Y%m%d-%H%M%S")
    )
}
fn stamp(hour: u32, minute: u32, second: u32) -> NaiveDateTime {
    day().and_hms_opt(hour, minute, second).unwrap()
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn page(keys: &[String], token: Option<&str>) -> Vec<u8> {
    let mut text = format!(
        "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>{}</IsTruncated>",
        token.is_some()
    );
    for key in keys {
        text.push_str(&format!(
            "<Contents><Key>{}</Key><Size>123</Size></Contents>",
            escape(key)
        ));
    }
    if let Some(token) = token {
        text.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            escape(token)
        ));
    }
    text.push_str("</ListBucketResult>");
    text.into_bytes()
}

fn decoded(_: Vec<u8>, stamp: NaiveDateTime) -> Result<Decoded, String> {
    let (mut frame, png) = Mrms::new().loading_placeholder().unwrap();
    frame.id = format!("{ID}-{}", stamp.format("%Y%m%dT%H%M%SZ"));
    frame.scan_time = stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    frame.status = FrameStatus::Complete;
    Ok((frame, png, stamp.and_utc().timestamp_millis()))
}

#[test]
fn registry_exposes_manual_only_mrms_without_changing_follow_or_map_clip() {
    use crate::protocol::{AdapterTarget, GeoPoint, Selection};
    let registry = SourceRegistry::compiled(); // no runtime or network needed
    let sources = registry.hello_sources();
    let mrms = sources.iter().find(|s| s.id == ID).unwrap();
    assert_eq!(mrms.family, Family::Grid);
    assert_eq!(mrms.kind, Kind::Mosaic);
    assert_eq!(mrms.attribution, "NOAA/NSSL MRMS");
    assert!(mrms.name.contains("Contiguous U.S."));
    assert_eq!(mrms.coverage, Some(registry.mrms.coverage.clone()));
    assert!(registry.polls(ID));
    assert_eq!(registry.history_policy(ID), Some(HISTORY));
    assert_eq!(registry.history_policy("opera").unwrap().max_frames, 12);
    assert_eq!(registry.history_policy("opera").unwrap().window_ms, None);
    assert!(
        registry
            .candidates()
            .iter()
            .all(|c| c.selection.source_id != ID)
    );
    let held = Selection {
        source_id: ID.into(),
        target: AdapterTarget::Mosaic,
    };
    for center in [
        GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        GeoPoint {
            lat: 54.9,
            lon: -60.1,
        },
        GeoPoint {
            lat: 20.1,
            lon: -129.9,
        },
        GeoPoint {
            lat: 51.5,
            lon: -0.1,
        },
    ] {
        let selected = registry.covering_selection(center, Some(&held));
        assert_ne!(selected.as_ref().map(|s| s.source_id.as_str()), Some(ID));
    }
    assert_eq!(
        registry
            .covering_selection(
                GeoPoint {
                    lat: 51.5,
                    lon: -0.1
                },
                None
            )
            .unwrap()
            .source_id,
        "opera"
    );
    assert_eq!(
        registry
            .covering_selection(
                GeoPoint {
                    lat: 35.33,
                    lon: -97.28
                },
                None
            )
            .unwrap()
            .source_id,
        "nexrad"
    );
    assert!(
        registry
            .covering_selection(
                GeoPoint {
                    lat: 20.1,
                    lon: -129.9
                },
                Some(&held)
            )
            .is_none()
    );
    for lat in [20.0, 37.5, 55.0] {
        for lon in [-130.0, -95.0, -60.0] {
            assert!(crate::envelope::in_live_envelope(lon, lat));
            assert!(
                crate::envelope::nexrad_network(lon, lat),
                "MRMS does not expand the existing clip"
            );
        }
    }
    let (placeholder, texture) = registry.mrms.loading_placeholder().unwrap();
    assert_eq!(placeholder.product_name, "QC Base Reflectivity");
    assert_eq!(placeholder.units, "dBZ");
    assert!(placeholder.scan_time.is_empty());
    assert!(!texture.is_empty());
}

#[test]
fn discovery_merges_midnight_pages_sorts_and_deduplicates() {
    let yesterday = day().pred_opt().unwrap();
    let before = yesterday.and_hms_opt(23, 58, 1).unwrap();
    let newest = stamp(0, 20, 16);
    let cutoff = newest - chrono::Duration::hours(1);
    let calls = Mutex::new(Vec::new());
    let list = |date, token: Option<String>| {
        calls.lock().unwrap().push((date, token.clone()));
        let bytes = if date == day() && token.is_none() {
            page(&[key(newest), key(stamp(0, 0, 10))], Some("a+b/&="))
        } else if date == day() {
            assert_eq!(token.as_deref(), Some("a+b/&="));
            page(
                &[
                    key(newest),
                    key(stamp(0, 2, 5)),
                    key(stamp(0, 2, 5)).replace(PRODUCT, "MergedReflectivityQCComposite_00.50"),
                    key(before),
                ],
                None,
            )
        } else {
            page(
                &[
                    key(before),
                    key(cutoff),
                    key(cutoff + chrono::Duration::seconds(1)),
                ],
                None,
            )
        };
        async { Ok(bytes) }
    };
    let found = runtime().block_on(discover(day(), &list)).unwrap();
    assert_eq!(
        found.iter().map(|o| o.stamp).collect::<Vec<_>>(),
        vec![
            cutoff,
            cutoff + chrono::Duration::seconds(1),
            before,
            stamp(0, 0, 10),
            stamp(0, 2, 5),
            newest
        ]
    );
    assert_eq!(calls.lock().unwrap().len(), 3);
    let url = listing_url(HOST, day(), Some("a+b/&=")).unwrap();
    assert_eq!(
        url.query_pairs()
            .find(|(k, _)| k == "continuation-token")
            .unwrap()
            .1,
        "a+b/&="
    );
}

#[test]
fn malformed_incomplete_and_oversized_listings_fail_explicitly() {
    for xml in [
        "",
        "<Error/>",
        "<ListBucketResult/>",
        "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>",
        "<ListBucketResult><IsTruncated>wat</IsTruncated></ListBucketResult>",
        "<ListBucketResult><IsTruncated>false</IsTruncated><IsTruncated>false</IsTruncated></ListBucketResult>",
    ] {
        assert!(parse_page(xml.as_bytes(), day()).is_err(), "{xml}");
    }
    assert!(parse_page(&vec![b' '; LISTING_MAX + 1], day()).is_err());
    assert!(parse_page(&page(&vec![key(stamp(1, 0, 0)); 1001], None), day()).is_err());
    assert!(parse_page(&page(&[], Some(&"x".repeat(4097))), day()).is_err());
    let mut cut = page(&[], None);
    cut.pop();
    assert!(parse_page(&cut, day()).is_err());
    for raw in [
        key(stamp(1, 0, 0)).replace("010000", "250000"),
        key(stamp(1, 0, 0)).replace(".grib2.gz", ".png"),
        format!("https://evil.invalid/{}", key(stamp(1, 0, 0))),
    ] {
        assert!(object(&raw, day()).is_none());
    }
    let count = AtomicUsize::new(0);
    let error = runtime()
        .block_on(discover(day(), &|_, _| {
            let next = count.fetch_add(1, Ordering::SeqCst).to_string();
            async move { Ok(page(&[], Some(&next))) }
        }))
        .unwrap_err();
    assert!(error.contains("incomplete discovery"));
    assert_eq!(count.load(Ordering::SeqCst), PAGES_MAX);
    assert!(
        runtime()
            .block_on(discover(day(), &|_, _| async {
                Ok(page(&[], Some("repeat")))
            }))
            .is_err()
    );
    assert!(
        runtime()
            .block_on(discover(day(), &|_, _| async { Err("unreachable".into()) }))
            .is_err()
    );
    assert!(
        runtime()
            .block_on(discover(day(), &|_, _| async { Ok(page(&[], None)) }))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn bad_newest_falls_back_but_attempts_duplicates_and_clock_rollback_are_bounded() {
    let objects: Vec<_> = (0..5)
        .map(|m| object(&key(stamp(12, m, 7)), day()).unwrap())
        .collect();
    runtime().block_on(async {
        let calls = Mutex::new(Vec::new());
        let get = |key: String| {
            calls.lock().unwrap().push(key.clone());
            async move {
                if key.contains("120407") {
                    Err("missing object".into())
                } else {
                    Ok(key.into_bytes())
                }
            }
        };
        let decode = Arc::new(|bytes: Vec<u8>, stamp| {
            if String::from_utf8(bytes).unwrap().contains("120307") {
                Err("corrupt GRIB".into())
            } else {
                decoded(vec![], stamp)
            }
        });
        let work = Arc::new(Semaphore::new(1));
        let loaded = newest_valid(&objects, None, &get, Arc::clone(&decode), Arc::clone(&work))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.2, stamp(12, 2, 7).and_utc().timestamp_millis());
        assert_eq!(calls.lock().unwrap().len(), 3);
        calls.lock().unwrap().clear();
        let no_get = |_: String| async { panic!("known and older frames must not be downloaded") };
        assert!(
            newest_valid(
                &objects,
                Some(stamp(12, 4, 7).and_utc().timestamp_millis()),
                &no_get,
                Arc::clone(&decode),
                Arc::clone(&work)
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            newest_valid(
                &objects,
                Some(stamp(13, 0, 0).and_utc().timestamp_millis()),
                &no_get,
                Arc::clone(&decode),
                Arc::clone(&work)
            )
            .await
            .unwrap()
            .is_none()
        );
        let calls = AtomicUsize::new(0);
        let fail = |_: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err("offline".into()) }
        };
        assert!(
            newest_valid(&objects, None, &fail, decode, work)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), FALLBACK_MAX);
    });
}

#[test]
fn polling_reports_failure_empty_recovery_and_new_data_without_duplicate_frames() {
    runtime().block_on(async {
        let today = Utc::now().date_naive();
        let first = today.and_hms_opt(0, 1, 7).unwrap();
        let next = first + chrono::Duration::minutes(2);
        let replies = Arc::new(Mutex::new(VecDeque::from([
            Ok(page(&[key(first)], None)), Ok(page(&[], None)),
            Err("network error".into()),
            Ok(page(&[], None)), Ok(page(&[], None)),
            Ok(page(&[key(first)], None)), Ok(page(&[], None)),
            Ok(page(&[key(next)], None)), Ok(page(&[], None)),
        ])));
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn(poll_loop(GridSender::new(tx, 4), vec![],
            move |_, _| { let reply = replies.lock().unwrap().pop_front().expect("unexpected poll"); async { reply } },
            |_| async { Ok(vec![]) }, Arc::new(decoded), Arc::new(Semaphore::new(1)), Duration::from_millis(1)));
        let mut received = Vec::new();
        for _ in 0..5 {
            let mut message = timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
            if let Some(ack) = message.published.take() { ack.send(true).unwrap(); }
            received.push(message);
        }
        task.abort(); let _ = task.await;
        assert!(received.iter().all(|e| e.session == 4));
        assert!(matches!(received[0].event, GridEvent::Frame { start_ms, .. } if start_ms == first.and_utc().timestamp_millis()));
        assert!(matches!(received[1].event, GridEvent::Offline { .. }));
        assert!(matches!(received[2].event, GridEvent::Silent { .. }));
        assert!(matches!(received[3].event, GridEvent::Online { .. }));
        assert!(matches!(received[4].event, GridEvent::Frame { start_ms, .. } if start_ms == next.and_utc().timestamp_millis()));
    });
}

#[test]
fn queue_backpressure_precedes_listing_and_abort_cancels_pending_http() {
    runtime().block_on(async {
        let (tx, mut rx) = mpsc::channel(1);
        let sender = GridSender::new(tx, 1);
        sender
            .send(GridEvent::Online {
                source_id: ID.into(),
            })
            .await
            .unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        struct DropCount(Arc<AtomicUsize>);
        impl Drop for DropCount {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let count = Arc::clone(&started);
        let cancelled = Arc::clone(&dropped);
        let task = tokio::spawn(poll_loop(
            sender,
            vec![],
            move |_, _| {
                let count = Arc::clone(&count);
                let cancelled = Arc::clone(&cancelled);
                async move {
                    let _guard = DropCount(cancelled);
                    count.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<Result<Vec<u8>, String>>().await
                }
            },
            |_| async { Ok(vec![]) },
            Arc::new(decoded),
            Arc::new(Semaphore::new(1)),
            Duration::from_millis(1),
        ));
        tokio::task::yield_now().await;
        assert_eq!(started.load(Ordering::SeqCst), 0);
        rx.recv().await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(started.load(Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_err());
    });
}

#[test]
fn failed_publication_retries_the_frame_before_marking_it_known() {
    runtime().block_on(async {
        let today = Utc::now().date_naive();
        let stamp = today.and_hms_opt(0, 1, 7).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn(poll_loop(
            GridSender::new(tx, 1),
            vec![],
            move |day, _| async move {
                Ok(page(
                    &if day == today {
                        vec![key(stamp)]
                    } else {
                        vec![]
                    },
                    None,
                ))
            },
            |_| async { Ok(vec![]) },
            Arc::new(decoded),
            Arc::new(Semaphore::new(1)),
            Duration::from_millis(1),
        ));
        for success in [false, true] {
            let message = timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(message.event, GridEvent::Frame { .. }));
            message.published.unwrap().send(success).unwrap();
        }
        let recovered = timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(recovered.event, GridEvent::Online { .. }));
        task.abort();
        let _ = task.await;
    });
}

#[test]
fn cancelled_blocking_decode_keeps_adapter_permit_until_it_really_finishes() {
    runtime().block_on(async {
        let work = Arc::new(Semaphore::new(1));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, gate) = std::sync::mpsc::channel();
        let gate = Mutex::new(gate);
        let signal = Arc::clone(&entered);
        let decode = Arc::new(move |bytes, stamp| {
            signal.notify_one();
            gate.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            decoded(bytes, stamp)
        });
        let objects = vec![object(&key(stamp(12, 0, 0)), day()).unwrap()];
        let first_work = Arc::clone(&work);
        let first_objects = objects.clone();
        let first = tokio::spawn(async move {
            newest_valid(
                &first_objects,
                None,
                &|_| async { Ok(vec![]) },
                decode,
                first_work,
            )
            .await
        });
        timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        first.abort();
        let _ = first.await;
        assert_eq!(work.available_permits(), 0);
        let gets = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&gets);
        let second = tokio::spawn(async move {
            newest_valid(
                &objects,
                None,
                &|_| {
                    count.fetch_add(1, Ordering::SeqCst);
                    async { Ok(vec![]) }
                },
                Arc::new(decoded),
                work,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(
            gets.load(Ordering::SeqCst),
            0,
            "new session must wait before downloading"
        );
        release.send(()).unwrap();
        assert!(
            timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_some()
        );
        assert_eq!(gets.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn all_grid_replies_reject_old_sessions_including_switch_away_and_back() {
    use crate::protocol::{AdapterTarget, Selection};
    let selected = Selection {
        source_id: ID.into(),
        target: AdapterTarget::Mosaic,
    };
    let other = Selection {
        source_id: "opera".into(),
        target: AdapterTarget::Mosaic,
    };
    let (frame, texture, start_ms) = decoded(vec![], stamp(12, 0, 0)).unwrap();
    for event in [
        GridEvent::Frame {
            source_id: ID.into(),
            frame: Box::new(frame.clone()),
            texture: texture.clone(),
            start_ms,
        },
        GridEvent::Backfill {
            source_id: ID.into(),
            frame: Box::new(frame),
            texture,
            start_ms,
        },
        GridEvent::Online {
            source_id: ID.into(),
        },
        GridEvent::Offline {
            source_id: ID.into(),
            reason: "late".into(),
        },
        GridEvent::Silent {
            source_id: ID.into(),
            reason: "late".into(),
        },
    ] {
        let reply = GridMessage {
            session: 1,
            event,
            published: None,
        };
        assert!(reply.matches(1, Some(&selected)));
        assert!(!reply.matches(2, Some(&other)));
        assert!(!reply.matches(3, Some(&selected)));
        assert!(!reply.matches(1, None));
    }
}

#[test]
fn http_transport_rejects_errors_and_overflow_without_content_length() {
    use std::io::{Read, Write};
    for response in [
        "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response.as_bytes());
        });
        assert!(runtime().block_on(fetch(url, 4)).is_err());
        server.join().unwrap();
    }
}

#[path = "history_tests.rs"]
mod history_tests;
