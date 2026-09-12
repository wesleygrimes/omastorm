use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

/// The archived volume the daemons under test start on (`OMASTORM_ARCHIVE`),
/// extracted once by `scripts/extract-fixtures.sh`.
const ARCHIVE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../data/raw/KTLX20130520_201643_V06.gz"
);
const WIND_OBS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/wind-obs-fixture.txt");
const HRRR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/hrrr-fixture.json");
fn engine_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_omastorm-engine"));
    cmd.env("OMASTORM_ARCHIVE", ARCHIVE)
        .env("OMASTORM_WIND_OBS", WIND_OBS)
        .env("OMASTORM_HRRR", HRRR);
    cmd
}
/// How long a daemon may take to decode the archive and listen, and how long
/// a reply may take. Both are far above the usual fraction of a second, so a
/// busy machine gets a slow test rather than a failed one; they only bound
/// how long a genuinely hung daemon holds the suite.
const STARTUP: Duration = Duration::from_secs(30);
const REPLY: Duration = Duration::from_secs(10);
/// One daemon at a time. Each test starts its own; run at once they compete
/// for the CPU with each other and with whatever else the machine is doing,
/// and the suite has missed a reply (`WouldBlock`) or a launch under that
/// load. Serial, each daemon's timing is its own.
static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    // A failed test poisons the lock; the next test is still its own.
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}
/// A fresh scratch `XDG_RUNTIME_DIR` under `target/` for this test. An
/// interrupted run leaves its tree behind, and a later process with the same
/// PID would otherwise find a socket file no daemon listens on.
fn scratch_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../target/t-{name}-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    // Remove engine/.. before adding the socket suffix: Unix socket paths
    // have a small byte limit, especially in nested PR worktrees.
    fs::canonicalize(root).unwrap()
}
/// Connect once the daemon under `root` listens, within `STARTUP`; `alive`
/// fails early when the daemon has already exited.
fn await_daemon(root: &Path, mut alive: impl FnMut() -> bool) -> BufReader<UnixStream> {
    let deadline = Instant::now() + STARTUP;
    loop {
        if let Ok(stream) = UnixStream::connect(root.join("omastorm/engine.sock")) {
            stream.set_read_timeout(Some(REPLY)).unwrap();
            return BufReader::new(stream);
        }
        assert!(alive(), "engine exited");
        assert!(Instant::now() < deadline, "engine startup timed out");
        thread::sleep(Duration::from_millis(10));
    }
}
struct Engine {
    child: Child,
    root: PathBuf,
}
impl Engine {
    fn start() -> Self {
        let root = scratch_root("engine");
        let child = engine_cmd()
            .env("XDG_RUNTIME_DIR", &root)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut engine = Self { child, root };
        await_daemon(&engine.root, || engine.child.try_wait().unwrap().is_none());
        engine
    }
    fn connect(&self) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(self.root.join("omastorm/engine.sock")).unwrap();
        stream.set_read_timeout(Some(REPLY)).unwrap();
        BufReader::new(stream)
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn read(client: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn send(client: &mut BufReader<UnixStream>, command: Value) {
    writeln!(client.get_mut(), "{command}").unwrap();
}
fn state(client: &mut BufReader<UnixStream>, predicate: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..8 {
        let value = read(client);
        if value["type"] == "state" && predicate(&value) {
            return value;
        }
    }
    panic!("expected state not received");
}
#[test]
fn fixture_transport_and_shared_commands() {
    let _serial = serial();
    let engine = Engine::start();
    let mut first = engine.connect();
    let hello = read(&mut first);
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["v"], 1);
    assert_eq!(hello["build"].as_str().unwrap().len(), 16);
    let sites = hello["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 163);
    let mut ids = std::collections::HashSet::new();
    for site in sites {
        assert!(ids.insert(site["id"].as_str().unwrap()));
        assert!((-90.0..=90.0).contains(&site["lat"].as_f64().unwrap()));
        assert!((-180.0..=180.0).contains(&site["lon"].as_f64().unwrap()));
        assert!(site["altM"].as_f64().unwrap() > -500.0);
    }
    for id in ["KTLX", "PABC", "PHKI", "PGUA", "TJUA", "RKJK", "LPLA"] {
        assert!(ids.contains(id));
    }
    let initial = read(&mut first);
    assert_eq!(initial["frame"]["scanTime"], "2013-05-20T20:16:43Z");
    assert_eq!(initial["source"], "archived");
    // The timeline lists what `seek` accepts: here the one archived frame.
    assert_eq!(
        initial["timeline"],
        json!([{"id": initial["frame"]["id"], "scanTime": "2013-05-20T20:16:43Z", "status": "complete"}])
    );
    assert_eq!(initial["playing"], false);
    assert!(initial["connection"]["ageSeconds"].as_u64().unwrap() > 400_000_000);
    assert!(initial.to_string().len() < 2500);
    assert!(initial["frame"].get("values").is_none());
    // The frame is the lowest sweep decoded from the Level II fixture: a polar
    // texture with an azimuth lookup and gate geometry, and nothing else that
    // describes radar placement.
    let frame = &initial["frame"];
    assert!(frame.get("grid").is_none());
    assert!(frame.get("sweep").is_none());
    assert_eq!(frame["rays"], 720);
    assert_eq!(frame["gates"], 1832);
    assert_eq!(frame["firstGateM"], 2125);
    assert_eq!(frame["gateSpacingM"], 250);
    // The moment's encoding, so a client can place a dBZ floor in code units.
    assert_eq!(frame["scale"], 2.0);
    assert_eq!(frame["offset"], 66.0);
    assert_eq!(frame["elevationDeg"], 0.48);
    for (field, width, height) in [("texture", 1832, 720), ("azimuthLut", 3600, 1)] {
        let path = frame[field].as_str().unwrap();
        assert!(
            path.starts_with("tex/") && path.contains("-r"),
            "{field}: {path}"
        );
        let bytes = fs::read(engine.root.join("omastorm").join(path)).unwrap();
        let info = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!((info.width, info.height), (width, height), "{field}");
        assert_eq!(info.color_type, png::ColorType::Rgba, "{field}");
        assert_eq!(info.bit_depth, png::BitDepth::Eight, "{field}");
    }
    let mut second = engine.connect();
    assert_eq!(read(&mut second)["type"], "hello");
    read(&mut second);
    // Fragmented NDJSON and multiple commands per write both work.
    first
        .get_mut()
        .write_all(b"{\"type\":\"lock\",\"enabled\":")
        .unwrap();
    first
        .get_mut()
        .write_all(b"true}\n{\"type\":\"follow\",\"enabled\":false}\n")
        .unwrap();
    for client in [&mut first, &mut second] {
        state(client, |s| s["site"]["locked"] == true);
        state(client, |s| s["site"]["follow"] == false);
    }
    // A station outside the hello table cannot be selected (a table station
    // would go live and reach the network, which no test does). The command
    // is answered only to its sender; shared state is neither changed nor
    // re-broadcast.
    send(&mut second, json!({"type":"select_site","id":"XXXX"}));
    let e = read(&mut second);
    assert_eq!(e["type"], "error");
    assert_eq!(e["v"], 1);
    assert_eq!(e["command"], "select_site");
    assert!(e["message"].as_str().unwrap().contains("XXXX"));
    first
        .get_mut()
        .write_all(b"not json\n{\"type\":\"future_command\"}\n")
        .unwrap();
    send(&mut first, json!({"type":"follow","enabled":false}));
    // A frame outside the timeline cannot be sought; the answer goes to the
    // sender alone. Stepping past the only frame, seeking the frame already
    // shown, and playing a one-frame timeline change nothing.
    send(&mut first, json!({"type":"seek","id":"KTLX-nowhere"}));
    let e = read(&mut first);
    assert_eq!(e["type"], "error");
    assert_eq!(e["command"], "seek");
    assert!(e["message"].as_str().unwrap().contains("timeline"));
    send(&mut first, json!({"type":"step","delta":1}));
    send(&mut first, json!({"type":"step","delta":-1}));
    send(
        &mut first,
        json!({"type":"seek","id":initial["frame"]["id"]}),
    );
    send(&mut first, json!({"type":"play"}));
    // Neither the rejection nor a command that changes nothing reaches the
    // other client: the next message each receives is the following change,
    // and state carries no error field.
    send(&mut first, json!({"type":"lock","enabled":true}));
    send(&mut first, json!({"type":"follow","enabled":true}));
    for client in [&mut first, &mut second] {
        let s = read(client);
        assert_eq!(s["type"], "state");
        assert_eq!(s["site"]["follow"], true);
        assert!(s.get("error").is_none());
    }
    // A known command with a mistyped field is rejected to its sender alone.
    send(&mut first, json!({"type":"follow","enabled":"yes"}));
    let e = read(&mut first);
    assert_eq!(e["type"], "error");
    assert_eq!(e["command"], "follow");
    assert!(e["message"].as_str().unwrap().contains("follow"));
    // A pan that settles on the fixture's own station changes nothing, and a
    // centre off the globe is rejected to its sender; neither is broadcast.
    send(
        &mut first,
        json!({"type":"view_center","lat":35.4681,"lon":-97.3326}),
    );
    send(
        &mut first,
        json!({"type":"view_center","lat":95.0,"lon":-97.0}),
    );
    let e = read(&mut first);
    assert_eq!(e["type"], "error");
    assert_eq!(e["command"], "view_center");
    assert!(e["message"].as_str().unwrap().contains("lat"));
    // Place search is a reply to its sender: gazetteer places, not a
    // state change, and a bad origin is rejected without a broadcast.
    send(
        &mut first,
        json!({"type":"search_places","query":"oklahoma","lat":35.47,"lon":-97.52}),
    );
    let places = read(&mut first);
    assert_eq!(places["type"], "places");
    assert_eq!(places["v"], 1);
    assert_eq!(places["query"], "oklahoma");
    let results = places["results"].as_array().unwrap();
    assert_eq!(results[0]["name"], "Oklahoma City");
    assert_eq!(results[0]["region"], "Oklahoma");
    assert_eq!(results[0]["country"], "US");
    assert!(results.len() <= 8);
    send(
        &mut second,
        json!({"type":"search_places","query":"norman"}),
    );
    let places = read(&mut second);
    assert_eq!(places["results"][0]["name"], "Norman");
    send(
        &mut first,
        json!({"type":"search_places","query":"x","lat":95.0,"lon":0.0}),
    );
    let e = read(&mut first);
    assert_eq!(e["type"], "error");
    assert_eq!(e["command"], "search_places");
    assert!(e["message"].as_str().unwrap().contains("lat"));
    // Still locked from above, a settle far from the station hands off to
    // nothing (the nearest there would go live and reach the network); the
    // other client's next state is the release.
    send(
        &mut first,
        json!({"type":"view_center","lat":40.0,"lon":-105.0}),
    );
    send(&mut first, json!({"type":"lock","enabled":false}));
    let s = read(&mut second);
    assert_eq!(s["type"], "state");
    assert_eq!(s["site"]["locked"], false);
    assert_eq!(s["site"]["follow"], true);
    assert_eq!(s["site"]["id"], "KTLX");
    assert_eq!(s["source"], "archived");
    assert!(
        engine_cmd()
            .env("OMASTORM_ARCHIVE", ARCHIVE)
            .arg("ensure")
            .env("XDG_RUNTIME_DIR", &engine.root)
            .status()
            .unwrap()
            .success()
    );
    let duplicate = engine_cmd()
        .env("OMASTORM_ARCHIVE", ARCHIVE)
        .env("XDG_RUNTIME_DIR", &engine.root)
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    let mut third = engine.connect();
    assert_eq!(read(&mut third)["type"], "hello");
    assert_eq!(read(&mut third)["site"]["locked"], false);
}
#[test]
fn crash_recovery_and_immutable_revisions() {
    let _serial = serial();
    let mut engine = Engine::start();
    let mut client = engine.connect();
    read(&mut client);
    let old = read(&mut client)["frame"]["texture"]
        .as_str()
        .unwrap()
        .to_owned();
    engine.child.kill().unwrap();
    engine.child.wait().unwrap();
    let old_path = engine.root.join("omastorm").join(&old);
    let restarted = Instant::now();
    engine.child = engine_cmd()
        .env("OMASTORM_ARCHIVE", ARCHIVE)
        .env("XDG_RUNTIME_DIR", &engine.root)
        .spawn()
        .unwrap();
    let mut client = await_daemon(&engine.root, || engine.child.try_wait().unwrap().is_none());
    read(&mut client);
    let new = read(&mut client)["frame"]["texture"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(old, new);
    // The file's timestamps are ancient by now; only observation time counts.
    fs::File::open(&old_path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)),
        )
        .unwrap();
    assert!(old_path.exists(), "old revisions retained for 30 seconds");
    thread::sleep(Duration::from_secs(20));
    assert!(
        old_path.exists(),
        "a file served just before a crash survives the restart grace period"
    );
    let deadline = restarted + Duration::from_secs(45);
    while old_path.exists() {
        assert!(
            Instant::now() < deadline,
            "unreferenced revision was not retired"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.root.join("omastorm").join(new).exists());
}
#[test]
fn oversized_client_is_disconnected_without_harming_server() {
    let _serial = serial();
    let engine = Engine::start();
    let mut bad = engine.connect();
    read(&mut bad);
    read(&mut bad);
    let _ = bad.get_mut().write_all(&vec![b'x'; 20_000]);
    let mut line = String::new();
    assert!(matches!(bad.read_line(&mut line), Ok(0) | Err(_)));
    let mut good = engine.connect();
    assert_eq!(read(&mut good)["type"], "hello");
    assert_eq!(read(&mut good)["type"], "state");
}
#[test]
fn launcher_replaces_a_daemon_of_another_build() {
    let _serial = serial();
    let root = scratch_root("launcher");
    let dir = root.join("omastorm");
    fs::create_dir_all(&dir).unwrap();
    let listener = UnixListener::bind(dir.join("engine.sock")).unwrap();
    // Stand in for a daemon left over from an earlier build: a real process
    // that the launcher must end, whose PID the socket reports. It answers the
    // launcher's handshake once, then stops listening as a dead daemon would.
    let mut stale_process = Command::new("sleep").arg("30").spawn().unwrap();
    let stale_pid = stale_process.id();
    let stale = thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let mut stream = stream.unwrap();
            let hello = json!({"type":"hello","v":1,"pid":stale_pid,"build":"stale"});
            writeln!(stream, "{hello}").unwrap();
        }
    });
    let engine = |mode: &str| {
        engine_cmd()
            .env("OMASTORM_ARCHIVE", ARCHIVE)
            .arg(mode)
            .env("XDG_RUNTIME_DIR", &root)
            .output()
            .unwrap()
    };
    // One launch of this build ends the stale daemon, says so, and starts
    // its own; no manual `stop` is needed after a rebuild.
    let started = engine("ensure");
    stale.join().unwrap();
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(started.status.success(), "{stderr}");
    assert!(stderr.contains(&format!("PID {stale_pid}")), "{stderr}");
    assert!(stderr.contains("another build"), "{stderr}");
    let status = stale_process.wait().unwrap();
    assert!(
        !status.success(),
        "the stale process was terminated: {status}"
    );
    let stream = UnixStream::connect(dir.join("engine.sock")).unwrap();
    stream.set_read_timeout(Some(REPLY)).unwrap();
    let mut client = BufReader::new(stream);
    let hello = read(&mut client);
    assert_eq!(hello["type"], "hello");
    assert_ne!(hello["build"], "stale");
    let pid = hello["pid"].as_u64().unwrap();
    drop(client);

    // `stop` also ends a real daemon and waits until the socket is closed.
    let stopped = engine("stop");
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&stopped.stdout).trim(),
        format!("Stopped engine (PID {pid})")
    );
    assert!(UnixStream::connect(dir.join("engine.sock")).is_err());
    let _ = fs::remove_dir_all(&root);
}
#[test]
fn stop_with_no_daemon_is_quiet_and_leaves_nothing_behind() {
    let root = scratch_root("stop");
    let output = engine_cmd()
        .env("OMASTORM_ARCHIVE", ARCHIVE)
        .arg("stop")
        .env("XDG_RUNTIME_DIR", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(
        !root.join("omastorm").exists(),
        "stop created the runtime directory"
    );
    let _ = fs::remove_dir_all(&root);
}
#[test]
fn tiles_needed_is_answered_tile_by_tile_to_the_sender() {
    let _serial = serial();
    let engine = Engine::start();
    let mut asker = engine.connect();
    read(&mut asker);
    read(&mut asker);
    let mut bystander = engine.connect();
    read(&mut bystander);
    read(&mut bystander);
    // The four z5 tiles around KTLX: answered centre-out with `ne` masks.
    send(
        &mut asker,
        json!({"type":"tiles_needed","z":5,"x0":7,"y0":12,"x1":8,"y1":13}),
    );
    let mut paths = std::collections::HashMap::new();
    for _ in 0..4 {
        let tile = read(&mut asker);
        assert_eq!(tile["type"], "tile_ready", "{tile}");
        assert_eq!(tile["v"], 1);
        assert_eq!(tile["set"], "ne");
        assert_eq!(tile["z"], 5);
        let (x, y) = (tile["x"].as_u64().unwrap(), tile["y"].as_u64().unwrap());
        assert!((7..=8).contains(&x) && (12..=13).contains(&y), "{tile}");
        let path = tile["path"].as_str().unwrap().to_owned();
        assert!(
            path.starts_with(&format!("tiles/ne/5/{x}/{y}-")) && path.ends_with(".png"),
            "{path}"
        );
        assert_eq!(path.matches('/').count(), 4);
        let bytes = fs::read(engine.root.join("omastorm").join(&path)).unwrap();
        let info = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap()
            .info()
            .clone();
        assert_eq!((info.width, info.height), (512, 512));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert!(tile["labels"].is_array());
        for label in tile["labels"].as_array().unwrap() {
            assert!(label["name"].is_string() && label["class"].is_string());
            assert!(label["lat"].is_number() && label["lon"].is_number());
            assert!(label["rank"].is_number());
        }
        assert!(paths.insert((x, y), path).is_none(), "each tile once");
    }
    // Asking again is answered at once under the same names; the other
    // client hears nothing, and state is untouched.
    send(
        &mut asker,
        json!({"type":"tiles_needed","z":5,"x0":7,"y0":12,"x1":8,"y1":13}),
    );
    for _ in 0..4 {
        let tile = read(&mut asker);
        assert_eq!(tile["type"], "tile_ready");
        let key = (tile["x"].as_u64().unwrap(), tile["y"].as_u64().unwrap());
        assert_eq!(paths[&key], tile["path"].as_str().unwrap());
    }
    // Too many tiles, a bad rectangle, and a missing field are rejected to
    // the sender alone.
    for (command, word) in [
        (
            json!({"type":"tiles_needed","z":6,"x0":0,"y0":0,"x1":8,"y1":7}),
            "72 tiles",
        ),
        (
            json!({"type":"tiles_needed","z":3,"x0":5,"y0":0,"x1":2,"y1":0}),
            "x1",
        ),
        (
            json!({"type":"tiles_needed","z":2,"x0":0,"y0":0,"x1":4,"y1":0}),
            "run 0 to 3",
        ),
        (
            json!({"type":"tiles_needed","z":2,"x0":0,"y0":0}),
            "missing",
        ),
    ] {
        send(&mut asker, command);
        let e = read(&mut asker);
        assert_eq!(e["type"], "error", "{e}");
        assert_eq!(e["command"], "tiles_needed");
        assert!(e["message"].as_str().unwrap().contains(word), "{e}");
    }
    send(&mut bystander, json!({"type":"lock","enabled":true}));
    let s = read(&mut bystander);
    assert_eq!(s["type"], "state");
    assert!(s.get("basemap").is_none() || s["basemap"].is_object());
    let s = read(&mut asker);
    assert_eq!(s["type"], "state", "{s}");
}

/// Without `OMASTORM_ARCHIVE` the daemon starts lean: no station, no frame
/// to draw, an empty timeline, `loading` until a client selects a site.
#[test]
fn a_lean_start_has_no_frame_until_a_site_is_selected() {
    let _serial = serial();
    let root = scratch_root("lean");
    let mut child = engine_cmd()
        .env("XDG_RUNTIME_DIR", &root)
        .env_remove("OMASTORM_ARCHIVE")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut client = await_daemon(&root, || child.try_wait().unwrap().is_none());
    let hello = read(&mut client);
    assert_eq!(hello["type"], "hello");
    let initial = read(&mut client);
    assert_eq!(initial["type"], "state");
    assert_eq!(initial["source"], "live");
    assert_eq!(initial["site"]["id"], "");
    assert_eq!(initial["connection"]["status"], "loading");
    assert_eq!(initial["timeline"], json!([]));
    assert_eq!(initial["frame"]["id"], "-loading");
    assert_eq!(initial["frame"]["scanTime"], "");
    assert_eq!(initial["frame"]["rays"], 1);
    assert_eq!(initial["frame"]["gates"], 1);
    assert!(
        root.join("omastorm")
            .join(initial["frame"]["texture"].as_str().unwrap())
            .is_file()
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn launcher_retries_a_slow_hello_within_its_startup_budget() {
    let _serial = serial();
    let engine = Engine::start();
    let hello = read(&mut engine.connect());
    let root = scratch_root("hello");
    let dir = root.join("omastorm");
    fs::create_dir_all(&dir).unwrap();
    let lock = fs::File::create(dir.join("engine.lock")).unwrap();
    lock.try_lock().unwrap();
    let listener = UnixListener::bind(dir.join("engine.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        for attempt in 0..2 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "launcher did not retry hello");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            if attempt == 0 {
                thread::sleep(Duration::from_millis(350));
                let _ = writeln!(stream, "{hello}"); // The first probe has timed out.
            } else {
                writeln!(stream, "{hello}").unwrap();
            }
        }
    });
    let started = engine_cmd()
        .arg("ensure")
        .env("XDG_RUNTIME_DIR", &root)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    drop(lock);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn set_product_velocity_and_wind_layers() {
    let _serial = serial();
    let engine = Engine::start();
    let mut client = engine.connect();
    assert_eq!(read(&mut client)["type"], "hello");
    let initial = read(&mut client);
    assert_eq!(initial["frame"]["product"], "REF");
    send(
        &mut client,
        json!({"type":"set_product","product":"VEL","elevationIndex":0}),
    );
    let vel = read(&mut client);
    assert_eq!(vel["type"], "state");
    assert_eq!(vel["frame"]["product"], "VEL");
    assert_eq!(vel["frame"]["units"], "m/s");
    assert_eq!(vel["frame"]["productName"], "Velocity");
    assert!(vel["frame"]["rays"].as_u64().unwrap() > 0);
    send(
        &mut client,
        json!({"type":"set_product","product":"SW","elevationIndex":0}),
    );
    let e = read(&mut client);
    assert_eq!(e["type"], "error");
    assert_eq!(e["command"], "set_product");
    send(
        &mut client,
        json!({"type":"set_product","product":"REF","elevationIndex":0}),
    );
    let back = read(&mut client);
    assert_eq!(back["frame"]["product"], "REF");
    send(&mut client, json!({"type":"lock","enabled":true}));
    assert_eq!(read(&mut client)["site"]["locked"], true);
    send(&mut client, json!({"type":"set_wind_forecast","hour":6}));
    let loading = read(&mut client);
    assert_eq!(loading["windField"]["forecastHour"], 6);
    send(
        &mut client,
        json!({"type":"wind_needed","lat":40.25,"lon":-73.16}),
    );
    let deadline = Instant::now() + REPLY;
    let mut saw_obs = false;
    let mut saw_field = false;
    while Instant::now() < deadline && (!saw_obs || !saw_field) {
        let msg = read(&mut client);
        if msg["type"] != "state" {
            continue;
        }
        if msg["windObs"].as_array().is_some_and(|a| !a.is_empty()) {
            saw_obs = true;
            assert_eq!(msg["windObs"][0]["network"], "NDBC");
        }
        if msg["windField"]["status"] == "ok" {
            saw_field = true;
            assert_eq!(msg["windField"]["source"], "HRRR");
            assert!(
                msg["windField"]["texture"]
                    .as_str()
                    .unwrap()
                    .starts_with("tex/")
            );
        }
    }
    assert!(saw_obs, "expected NDBC observations near the view");
    assert!(saw_field, "expected HRRR fixture field");
}
