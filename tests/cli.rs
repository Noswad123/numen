#![cfg(unix)]

use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(6);

struct ChildGuard(Child);

impl ChildGuard {
    fn wait_until(&mut self, deadline: Instant) -> ExitStatus {
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child exceeded its deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }

    // Call only after bounded wait: these pipes then have no live writer.
    fn output(&mut self) -> (String, String) {
        let mut stdout = String::new();
        let mut stderr = String::new();
        self.0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        if let Some(mut pipe) = self.0.stderr.take() {
            pipe.read_to_string(&mut stderr).unwrap();
        }
        (stdout, stderr)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new().prefix("numen-").tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("s");
    assert!(socket.as_os_str().as_bytes().len() <= 100);
    (dir, socket)
}

fn command(socket: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_numen"));
    command
        .arg("--socket")
        .arg(socket)
        .current_dir(socket.parent().unwrap())
        .env("RUST_LOG", "numen=info,wgpu=warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn ready(child: &mut ChildGuard, socket: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "listener exited before readiness"
        );
        match UnixStream::connect(socket) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(300)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(300)))
                    .unwrap();
                return stream;
            }
            Err(error) => {
                assert!(Instant::now() < deadline, "listener was not ready: {error}");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn acknowledge(stream: &mut BufReader<UnixStream>, request: Value) {
    writeln!(stream.get_mut(), "{request}").unwrap();
    let mut line = String::new();
    assert!(stream.read_line(&mut line).unwrap() > 0, "missing ACK");
    let ack: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ack["version"], 1, "{ack}");
    assert_eq!(ack["ok"], true, "{ack}");
}

fn control(socket: &Path, args: &[&str]) -> Value {
    let mut child = ChildGuard(command(socket).args(args).spawn().unwrap());
    let status = child.wait_until(Instant::now() + LIMIT);
    let (stdout, stderr) = child.output();
    assert!(status.success(), "{args:?}: {stderr}");
    serde_json::from_str(&stdout).expect("control must emit exactly one JSON status")
}

#[test]
fn cli_focus_selects_conversations_and_retains_disconnected_identity() {
    let (_dir, socket) = fixture();
    let mut listener = ChildGuard(
        command(&socket)
            .args(["listen", "--exit-after", "3"])
            .spawn()
            .unwrap(),
    );
    let mut producer = BufReader::new(ready(&mut listener, &socket));
    let deadline = Instant::now() + LIMIT;
    // Reverse snapshot order: initial focus is lexical, not attention-ranked.
    acknowledge(
        &mut producer,
        json!({"version":1,"type":"source.register","source":"djinn","instance":"one",
        "activities":[
            {"activity":"conv-b","sequence":1,"state":"waiting","attention":true},
            {"activity":"conv-a","sequence":1,"state":"working"}
        ]}),
    );
    let focus_a = json!({"source":"djinn","instance":"one","activity":"conv-a"});
    let focus_b = json!({"source":"djinn","instance":"one","activity":"conv-b"});
    let activities = json!([
        {"source":"djinn","instance":"one","activity":"conv-a",
            "state":"working","attention":false,"reason":null},
        {"source":"djinn","instance":"one","activity":"conv-b",
            "state":"waiting","attention":true,"reason":null}
    ]);
    let initial = control(&socket, &["status"]);
    assert_eq!(
        initial,
        json!({"mode":"shared-focused","focus":focus_a,"focus_available":true,
            "activities":activities,"offset":0,"total":2,"next_offset":null})
    );
    acknowledge(
        &mut producer,
        json!({"version":1,"type":"activity.state","activity":"conv-b",
            "sequence":2,"state":"waiting","attention":true}),
    );
    assert_eq!(control(&socket, &["status"]), initial);

    for args in [
        vec!["focus", "--source", "djinn", "--activity", "conv-b"],
        vec![
            "focus",
            "--source",
            "djinn",
            "--activity",
            "conv-b",
            "--instance",
            "one",
        ],
    ] {
        let selected = control(&socket, &args);
        let mut expected = initial.clone();
        expected["focus"] = focus_b.clone();
        assert_eq!(selected, expected);
        assert_eq!(control(&socket, &["status"]), selected);
    }
    assert_eq!(control(&socket, &["focus", "--next"]), initial);
    let mut page = initial.clone();
    page["offset"] = json!(1);
    page["activities"] = json!([activities[1]]);
    assert_eq!(control(&socket, &["status", "--offset", "1"]), page);

    let mut other = BufReader::new(ready(&mut listener, &socket));
    acknowledge(
        &mut other,
        json!({"version":1,"type":"source.register","source":"other","instance":"two",
            "activities":[{"activity":"task","sequence":1,"state":"working"}]}),
    );
    drop(producer);
    // EOF retirement can occur on the next service tick, after a status query.
    let disconnected = loop {
        assert!(Instant::now() < deadline, "producer was not retired");
        let status = control(&socket, &["status"]);
        if status["total"] == 1 {
            break status;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        disconnected,
        json!({"mode":"shared-focused","focus":focus_a,"focus_available":false,
            "activities":[{"source":"other","instance":"two","activity":"task",
                "state":"working","attention":false,"reason":null}],
            "offset":0,"total":1,"next_offset":null})
    );
    assert_eq!(control(&socket, &["status"]), disconnected);
    let status = listener.wait_until(deadline);
    let (_, stderr) = listener.output();
    assert!(status.success(), "{stderr}");
    assert!(!socket.exists(), "listener left its socket behind");
}

#[test]
fn focus_validates_target_groups_before_connecting() {
    let (_dir, socket) = fixture();
    for args in [
        vec!["focus"],
        vec!["focus", "--source", "djinn"],
        vec!["focus", "--activity", "conv-b"],
        vec!["focus", "--instance", "one"],
        vec![
            "focus",
            "--next",
            "--activity",
            "conv-b",
            "--source",
            "djinn",
        ],
        vec!["focus", "--next", "--instance", "one"],
        vec!["focus", "--source", "bad/id", "--activity", "conv-b"],
    ] {
        let mut child = ChildGuard(command(&socket).args(&args).spawn().unwrap());
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.contains("error:"), "{args:?}: {stderr}");
        assert!(stderr.contains("--help"), "{args:?}: {stderr}");
    }
    assert!(!socket.exists());
}

#[test]
fn listener_observes_publisher_and_disconnect_then_cleans_socket() {
    let (_dir, socket) = fixture();
    let started = Instant::now();
    let mut listener = ChildGuard(
        command(&socket)
            .args(["listen", "--exit-after", "1.5"])
            .spawn()
            .unwrap(),
    );
    drop(ready(&mut listener, &socket));
    let mut publisher = ChildGuard(
        command(&socket)
            .args([
                "publish",
                "--state",
                "working",
                "--duration",
                "0.1",
                "--source",
                "build",
                "--summary",
                "Building the conversation summary",
            ])
            .spawn()
            .unwrap(),
    );
    let status = publisher.wait_until(started + LIMIT);
    let (stdout, stderr) = publisher.output();
    assert!(status.success(), "{stderr}");
    assert!(stdout.is_empty(), "publisher polluted stdout: {stdout}");
    let status = listener.wait_until(started + LIMIT);
    let (stdout, stderr) = listener.output();
    assert!(status.success(), "{stderr}");
    let views: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let working = views
        .iter()
        .position(|view| view["base"]["state"] == "working" && view["base"]["source"] == "build")
        .expect("missing working presentation");
    assert_eq!(
        views[working]["base"]["summary"],
        "Building the conversation summary"
    );
    let mut following = views[working]["following"].clone();
    assert_eq!(following["summary"], "Building the conversation summary");
    assert_eq!(following["available"], true);
    following["available"] = json!(false);
    assert!(
        views[working + 1..]
            .iter()
            .any(|view| *view == json!({"base":null,"cue":null,"following":following})),
        "missing unavailable focused presentation after publisher disconnect: {stdout}"
    );
    assert!(!socket.exists(), "listener left its socket behind");
}

#[test]
fn unavailable_publisher_fails_promptly_without_stdout() {
    let (_dir, socket) = fixture();
    let started = Instant::now();
    let mut publisher = ChildGuard(
        command(&socket)
            .args([
                "publish",
                "--state",
                "working",
                "--duration",
                "0.1",
                "--source",
                "build",
            ])
            .spawn()
            .unwrap(),
    );
    let status = publisher.wait_until(started + LIMIT);
    assert!(started.elapsed() < LIMIT);
    let (stdout, stderr) = publisher.output();
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty(), "failure must be diagnosed on stderr");
    assert!(!socket.exists());
}

#[test]
fn demo_publishes_ordered_protocol_states_and_disconnects() {
    let (_dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + LIMIT;
    // Exercise the global socket option after the subcommand as well.
    let mut publisher = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_numen"))
            .args([
                "publish",
                "--demo",
                "--summary",
                "Reviewing protocol changes",
                "--duration",
                "0.1",
                "--socket",
            ])
            .arg(&socket)
            .current_dir(socket.parent().unwrap())
            .env("RUST_LOG", "numen=info,wgpu=warn")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stream = loop {
        match server.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "publisher never connected");
                assert!(
                    publisher.0.try_wait().unwrap().is_none(),
                    "publisher exited before connecting"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    // macOS may inherit O_NONBLOCK from the accepting listener.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut requests = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "publisher failed to disconnect");
        reader
            .get_ref()
            .set_read_timeout(Some(remaining.min(Duration::from_millis(500))))
            .unwrap();
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["version"], 1);
        requests.push(request);
        reader
            .get_mut()
            .write_all(b"{\"version\":1,\"ok\":true}\n")
            .unwrap();
    }
    let status = publisher.wait_until(deadline);
    let (stdout, stderr) = publisher.output();
    assert!(status.success(), "{stderr}");
    assert!(stdout.is_empty());
    assert_eq!(requests[0]["type"], "source.register");
    assert_eq!(requests[0]["activities"], json!([]));
    let states: Vec<&Value> = requests
        .iter()
        .skip(1)
        .filter(|request| {
            assert!(
                request["type"] == "activity.state"
                    || request["type"] == "source.heartbeat"
                    || request["type"] == "activity.remove"
            );
            request["type"] == "activity.state"
        })
        .collect();
    let expected = [
        "working",
        "waiting",
        "succeeded",
        "working",
        "failed",
        "cancelled",
        "idle",
    ];
    assert_eq!(states.len(), expected.len());
    for (index, (state, expected)) in states.iter().zip(expected).enumerate() {
        assert_eq!(state["state"], expected);
        assert_eq!(state["sequence"], index as u64 + 1);
        assert_eq!(state["attention"], expected == "waiting");
        assert_eq!(state["summary"], "Reviewing protocol changes");
    }
    assert_eq!(requests.last().unwrap()["type"], "activity.remove");
    assert_eq!(requests.last().unwrap()["sequence"], 8);
}

#[test]
fn publish_validates_flag_group_and_attention_before_connecting() {
    let (_dir, socket) = fixture();
    for (args, diagnostic) in [
        (vec!["publish"], "required"),
        (
            vec!["publish", "--demo", "--state", "working"],
            "cannot be used",
        ),
        (vec!["publish", "--demo", "--attention"], "cannot be used"),
        (
            vec!["publish", "--state", "working", "--attention"],
            "attention is allowed only for waiting",
        ),
        (
            vec!["publish", "--state", "working", "--summary", "   "],
            "summary",
        ),
        (
            vec!["publish", "--state", "working", "--summary", "bad\nsummary"],
            "summary",
        ),
    ] {
        let mut child = ChildGuard(command(&socket).args(&args).spawn().unwrap());
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert!(!status.success(), "accepted {args:?}");
        assert!(stdout.is_empty());
        assert!(stderr.contains(diagnostic), "{args:?}: {stderr}");
    }
}

#[test]
fn listener_deadline_survives_sustained_traffic_with_unread_stdout() {
    let (_dir, socket) = fixture();
    let mut listener = ChildGuard(
        command(&socket)
            .args(["listen", "--exit-after", "1.5"])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stream = ready(&mut listener, &socket);
    // Budget the service's own exit separately from cold binary startup.
    let ready_at = Instant::now();
    let exit_limit = Duration::from_secs(3);
    let register = json!({"version":1,"type":"source.register",
        "source":"s".repeat(96),"instance":"i".repeat(96),"activities":[]});
    writeln!(stream, "{register}").unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ok"], true);
    stream = reader.into_inner();
    stream.set_nonblocking(true).unwrap();
    let traffic_deadline = Instant::now() + Duration::from_secs(1);
    let mut pending = Vec::new();
    let mut offset = 0;
    let mut sequence = 0_u64;
    let mut acknowledged = 0_u64;
    let mut replies = Vec::new();
    let mut bytes = [0_u8; 4096];
    while Instant::now() < traffic_deadline {
        if offset == pending.len() && sequence - acknowledged < 32 {
            pending.clear();
            offset = 0;
            for _ in 0..8 {
                sequence += 1;
                let update = json!({"version":1,"type":"activity.state",
                    "activity":"a".repeat(96),"sequence":sequence,"state":"working",
                    "reason":format!("{sequence:0128}")});
                writeln!(pending, "{update}").unwrap();
            }
        }
        if offset < pending.len() {
            match stream.write(&pending[offset..]) {
                Ok(0) => panic!("producer socket closed"),
                Ok(n) => offset += n,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => panic!("write traffic: {error}"),
            }
        }
        match stream.read(&mut bytes) {
            Ok(0) => panic!("listener exited before its deadline"),
            Ok(n) => {
                replies.extend_from_slice(&bytes[..n]);
                while let Some(end) = replies.iter().position(|byte| *byte == b'\n') {
                    let ack: Value = serde_json::from_slice(&replies[..end]).unwrap();
                    assert_eq!(ack["version"], 1);
                    assert_eq!(ack["ok"], true, "{ack}");
                    acknowledged += 1;
                    replies.drain(..=end);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("drain ACKs: {error}"),
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        acknowledged >= 64,
        "insufficient sustained traffic: {acknowledged} ACKs"
    );
    // Keep both the producer and the unread stdout pipe open until exit.
    let status = listener.wait_until(ready_at + exit_limit);
    assert!(status.success());
    assert!(ready_at.elapsed() < exit_limit);
    assert!(!socket.exists());
    let (stdout, _) = listener.output();
    assert!(!stdout.is_empty());
}

#[test]
fn presets_are_available_without_a_service() {
    let (_dir, socket) = fixture();
    assert_eq!(
        control(&socket, &["presets"]),
        json!({"presets":[{"id":"djinn","name":"Djinn","capabilities":["open"]}]})
    );
    assert!(!socket.exists());
}

#[test]
fn headless_discovery_reports_live_targets_but_no_lifecycle_capabilities() {
    let (_dir, socket) = fixture();
    let mut listener = ChildGuard(
        command(&socket)
            .args(["listen", "--exit-after", "3"])
            .spawn()
            .unwrap(),
    );
    let mut producer = BufReader::new(ready(&mut listener, &socket));
    let deadline = Instant::now() + LIMIT;
    acknowledge(
        &mut producer,
        json!({"version":1,"type":"source.register","source":"djinn","instance":"one",
        "activities":[
            {"activity":"a","sequence":1,"state":"working"},
            {"activity":"b","sequence":1,"state":"waiting","attention":true}
        ]}),
    );
    let attachment = json!({"source":"djinn","instance":"one","activity":"a"});
    let familiars = control(&socket, &["familiars"]);
    assert_eq!(
        familiars,
        json!({"familiars":[{"id":"main","preset":"djinn","attachment":attachment,
            "available":true,"capabilities":[],"settings":{"sprite_scale":0.5,"font_size":12.0,"bubble_duration":5.0}}],"offset":0,"total":1,"next_offset":null})
    );
    assert_eq!(
        control(&socket, &["familiars", "--offset", "1"]),
        json!({"familiars":[],"offset":1,"total":1,"next_offset":null})
    );
    let targets = json!([
        {"source":"djinn","instance":"one","activity":"a","state":"working",
            "attention":false,"reason":null,"claimed_by":"main","eligible":false},
        {"source":"djinn","instance":"one","activity":"b","state":"waiting",
            "attention":true,"reason":null,"claimed_by":null,"eligible":true}
    ]);
    assert_eq!(
        control(&socket, &["targets"]),
        json!({"targets":targets,"offset":0,"total":2,"next_offset":null})
    );
    let mut own_targets = targets.clone();
    own_targets[0]["eligible"] = json!(true);
    assert_eq!(
        control(&socket, &["targets", "--familiar", "main"]),
        json!({"targets":own_targets,"offset":0,"total":2,"next_offset":null})
    );
    assert_eq!(
        control(&socket, &["targets", "--familiar", "main", "--offset", "1"]),
        json!({"targets":[targets[1]],"offset":1,"total":2,"next_offset":null})
    );
    let mut explicit_main = control(&socket, &["status", "--familiar", "main"]);
    assert_eq!(
        explicit_main.as_object_mut().unwrap().remove("familiar_id"),
        Some(json!("main"))
    );
    assert_eq!(explicit_main, control(&socket, &["status"]));
    let initial_status = explicit_main;

    for args in [
        vec!["open", "--source", "djinn", "--activity", "b"],
        vec![
            "attach",
            "--familiar",
            "main",
            "--source",
            "djinn",
            "--activity",
            "b",
        ],
        vec!["close", "--familiar", "main"],
    ] {
        let mut child = ChildGuard(command(&socket).args(&args).spawn().unwrap());
        let status = child.wait_until(deadline);
        let (stdout, stderr) = child.output();
        assert!(
            !status.success(),
            "accepted headless lifecycle command {args:?}"
        );
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(
            stderr.contains("no graphical familiar"),
            "{args:?}: {stderr}"
        );
    }
    let mut motion = ChildGuard(
        command(&socket)
            .args(["go-to-mouse", "--familiar", "main"])
            .spawn()
            .unwrap(),
    );
    let status = motion.wait_until(deadline);
    let (stdout, stderr) = motion.output();
    assert!(!status.success(), "accepted headless motion command");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("go-to-mouse requires graphical navigation support"),
        "{stderr}"
    );
    let mut settings = ChildGuard(
        command(&socket)
            .args(["settings", "--familiar", "main", "--font-size", "16"])
            .spawn()
            .unwrap(),
    );
    let status = settings.wait_until(deadline);
    let (stdout, stderr) = settings.output();
    assert!(!status.success(), "accepted headless settings command");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("settings requires live graphical settings support"),
        "{stderr}"
    );
    assert_eq!(control(&socket, &["familiars"]), familiars);
    assert_eq!(control(&socket, &["status"]), initial_status);
    assert_eq!(control(&socket, &["status"])["focus"], attachment);
    let status = listener.wait_until(deadline);
    let (_, stderr) = listener.output();
    assert!(status.success(), "{stderr}");
    assert!(!socket.exists());
}

// Keep lifecycle serialization independent of a graphical server and its callbacks.
fn mock_control(args: &[&str], input: Option<&Value>, expected: Value, data: Value) {
    let (_dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + LIMIT;
    let mut child = ChildGuard(
        command(&socket)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    if let Some(input) = input {
        child
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
    } else {
        drop(child.0.stdin.take());
    }
    let stream = loop {
        match server.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "{args:?}: client never connected"
                );
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "{args:?}: client exited"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    // macOS may inherit O_NONBLOCK from the accepting listener.
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0, "missing request");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        expected,
        "{args:?}"
    );
    writeln!(
        reader.get_mut(),
        "{}",
        json!({"version":1,"ok":true,"data":data})
    )
    .unwrap();
    let status = child.wait_until(deadline);
    let (stdout, stderr) = child.output();
    assert!(status.success(), "{args:?}: {stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap(),
        data,
        "{args:?}"
    );
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "extra request: {line}"
    );
    assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
fn lifecycle_commands_serialize_each_selection_and_print_ack_data() {
    let open = json!([
        {"id":"first","preset":"djinn","source":"djinn","instance":"one","activity":"a"},
        {"source":"build","activity":"b"}
    ]);
    mock_control(
        &["open", "--batch", "-"],
        Some(&open),
        json!({"version":1,"type":"familiar.open","items":open}),
        json!({"opened":["first","generated"]}),
    );
    let attach = json!([
        {"id":"first","source":"build","instance":"two","activity":"b"},
        {"id":"second","source":"djinn","activity":"a"}
    ]);
    mock_control(
        &["attach", "--batch", "-"],
        Some(&attach),
        json!({"version":1,"type":"familiar.attach","items":attach}),
        json!({"attached":["first","second"]}),
    );
    for (args, item) in [
        (
            vec!["open", "--source", "djinn", "--activity", "a"],
            json!({"id":null,"preset":"djinn","source":"djinn","instance":null,"activity":"a"}),
        ),
        (
            vec![
                "open",
                "--source",
                "build",
                "--instance",
                "two",
                "--activity",
                "b",
                "--id",
                "second",
                "--preset",
                "djinn",
            ],
            json!({"id":"second","preset":"djinn","source":"build","instance":"two","activity":"b"}),
        ),
        (
            vec![
                "attach",
                "--familiar",
                "second",
                "--source",
                "djinn",
                "--activity",
                "a",
            ],
            json!({"id":"second","source":"djinn","instance":null,"activity":"a"}),
        ),
        (
            vec![
                "attach",
                "--familiar",
                "first",
                "--source",
                "build",
                "--instance",
                "two",
                "--activity",
                "b",
            ],
            json!({"id":"first","source":"build","instance":"two","activity":"b"}),
        ),
    ] {
        let kind = format!("familiar.{}", args[0]);
        let data = if args[0] == "open" {
            json!({"opened":["second"]})
        } else {
            json!({"attached":[item["id"]]})
        };
        mock_control(
            &args,
            None,
            json!({"version":1,"type":kind,"items":[item]}),
            data,
        );
    }
    mock_control(
        &["close", "--familiar", "first", "second"],
        None,
        json!({"version":1,"type":"familiar.close","ids":["first","second"]}),
        json!({"closed":["first","second"]}),
    );
}

#[test]
fn targeted_controls_serialize_familiar_ids_and_print_ack_data() {
    mock_control(
        &["go-to-mouse", "--familiar", "main", "second"],
        None,
        json!({"version":1,"type":"familiar.go-to-mouse","ids":["main","second"]}),
        json!({"moving":["main","second"]}),
    );
    for (args, expected) in [
        (
            vec!["bubbles", "--familiar", "first", "second"],
            json!({"version":1,"type":"companion.reveal","familiars":["first","second"]}),
        ),
        (
            vec!["status", "--familiar", "second", "--offset", "4"],
            json!({"version":1,"type":"companion.status","familiar":"second","offset":4}),
        ),
        (
            vec!["focus", "--next", "--familiar", "second"],
            json!({"version":1,"type":"companion.next","familiar":"second"}),
        ),
        (
            vec![
                "focus",
                "--familiar",
                "second",
                "--source",
                "djinn",
                "--activity",
                "a",
            ],
            json!({"version":1,"type":"companion.focus","familiar":"second","source":"djinn","instance":null,"activity":"a"}),
        ),
        (
            vec![
                "focus",
                "--familiar",
                "first",
                "--source",
                "build",
                "--instance",
                "two",
                "--activity",
                "b",
            ],
            json!({"version":1,"type":"companion.focus","familiar":"first","source":"build","instance":"two","activity":"b"}),
        ),
    ] {
        mock_control(
            &args,
            None,
            expected,
            json!({"focus":{"source":"build","instance":"two","activity":"b"},"activities":[]}),
        );
    }
}

#[test]
fn settings_serializes_only_explicit_fields_in_one_atomic_request() {
    for (flags, patch) in [
        (vec!["--sprite-scale", "0.25"], json!({"sprite_scale":0.25})),
        (vec!["--font-size", "16"], json!({"font_size":16.0})),
        (
            vec!["--bubble-duration", "2"],
            json!({"bubble_duration":2.0}),
        ),
        (
            vec![
                "--sprite-scale",
                "2",
                "--font-size",
                "32",
                "--bubble-duration",
                "30",
            ],
            json!({"sprite_scale":2.0,"font_size":32.0,"bubble_duration":30.0}),
        ),
        (
            vec!["--font-size", "8", "--bubble-duration", "0.25"],
            json!({"font_size":8.0,"bubble_duration":0.25}),
        ),
    ] {
        let mut args = vec!["settings", "--familiar", "first", "second"];
        args.extend(flags);
        mock_control(
            &args,
            None,
            json!({"version":1,"type":"familiar.settings","ids":["first","second"],"patch":patch}),
            json!({"updated":["second","first"]}),
        );
    }
}

#[test]
fn settings_rejects_invalid_options_before_connecting() {
    let (dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let mut cases = vec![
        vec!["settings".to_owned()],
        vec!["settings".into(), "--font-size".into(), "16".into()],
        vec!["settings".into(), "--familiar".into(), "a".into()],
    ];
    for ids in [
        vec!["".into()],
        vec!["bad/id".into()],
        vec!["a".into(), "a".into()],
        (0..17).map(|n| format!("f{n}")).collect(),
    ] {
        let mut args = vec![
            "settings".into(),
            "--font-size".into(),
            "16".into(),
            "--familiar".into(),
        ];
        args.extend(ids);
        cases.push(args);
    }
    for (flag, values) in [
        (
            "--sprite-scale",
            vec!["0.24", "2.01", "0", "NaN", "inf", "-inf"],
        ),
        ("--font-size", vec!["7.9", "32.1", "0", "NaN", "Inf", "-1"]),
        (
            "--bubble-duration",
            vec!["0.24", "30.01", "0", "NaN", "inf", "-1"],
        ),
        ("--config", vec!["unknown.json"]),
        ("--unknown", vec!["1"]),
    ] {
        for value in values {
            cases.push(
                ["settings", "--familiar", "a", flag, value]
                    .map(str::to_owned)
                    .to_vec(),
            );
        }
    }
    for args in cases {
        let mut child = ChildGuard(command(&socket).args(&args).spawn().unwrap());
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert!(!status.success(), "accepted {args:?}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(!stderr.is_empty(), "{args:?}");
        assert!(
            matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
            "{args:?}: connected before validation"
        );
    }
    assert!(!dir.path().join("service.log").exists());
}

#[test]
fn invalid_batches_and_selections_are_rejected_before_connecting() {
    let (_dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    for (args, input, diagnostic) in [
        (
            vec!["open", "--batch", "-"],
            Some(json!([])),
            "expected 1..16 items",
        ),
        (
            vec!["attach", "--batch", "-"],
            Some(json!([])),
            "expected 1..16 familiar IDs",
        ),
        (
            vec!["attach", "--batch", "-"],
            Some(json!([
                {"id":"same","source":"djinn","activity":"a"},
                {"id":"same","source":"build","activity":"b"}
            ])),
            "duplicate familiar ID",
        ),
        (
            vec!["close", "--familiar", "same", "same"],
            None,
            "duplicate familiar ID",
        ),
        (
            vec!["bubbles", "--familiar", "same", "same"],
            None,
            "duplicate familiar ID",
        ),
        (vec!["go-to-mouse"], None, "required"),
        (
            vec!["go-to-mouse", "--familiar"],
            None,
            "a value is required",
        ),
        (vec!["go-to-mouse", "--familiar", ""], None, "identifier"),
        (
            vec!["go-to-mouse", "--familiar", "same", "same"],
            None,
            "duplicate familiar ID",
        ),
        (
            vec![
                "go-to-mouse",
                "--familiar",
                "a",
                "b",
                "c",
                "d",
                "e",
                "f",
                "g",
                "h",
                "i",
                "j",
                "k",
                "l",
                "m",
                "n",
                "o",
                "p",
                "q",
            ],
            None,
            "expected 1..16 familiar IDs",
        ),
        (
            vec!["open", "--batch", "-"],
            Some(json!([
                {"id":"bad/id","source":"djinn","activity":"a"}
            ])),
            "identifier",
        ),
        (
            vec!["open", "--batch", "-"],
            Some(json!([
                {"source":"bad/id","activity":"a"}
            ])),
            "identifier",
        ),
        (
            vec!["attach", "--batch", "-"],
            Some(json!([
                {"id":"main","source":"djinn","instance":"bad/id","activity":"a"}
            ])),
            "identifier",
        ),
        (
            vec!["attach", "--batch", "-"],
            Some(json!([
                {"id":"bad/id","source":"djinn","activity":"a"}
            ])),
            "identifier",
        ),
        (
            vec!["attach", "--batch", "-"],
            Some(json!([
                {"id":"main","source":"djinn","activity":"bad/id"}
            ])),
            "identifier",
        ),
    ] {
        let mut child = ChildGuard(
            command(&socket)
                .args(&args)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        if let Some(input) = input {
            child
                .0
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
        } else {
            drop(child.0.stdin.take());
        }
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert!(!status.success(), "accepted {args:?}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.contains(diagnostic), "{args:?}: {stderr}");
        assert!(
            matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
            "{args:?}: validation connected to the service"
        );
    }
}

#[test]
fn wisp_action_prints_absolute_argv_without_writing_home_or_catalog() {
    let (dir, socket) = fixture();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let sentinel = home.join("keep");
    fs::write(&sentinel, b"untouched").unwrap();
    let catalog = dir.path().join("actions.toml");
    for explicit_file in [false, true] {
        let mut cmd = command(&socket);
        cmd.arg("wisp-action")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("PATH", &home);
        if explicit_file {
            cmd.arg("--file").arg(&catalog);
        }
        let mut child = ChildGuard(cmd.spawn().unwrap());
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert!(status.success(), "{stderr}");
        let value: toml::Value = toml::from_str(&stdout).unwrap();
        let actions = value["action"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert_eq!(action["id"].as_str(), Some("numen"));
        assert_eq!(action["title"].as_str(), Some("Numen"));
        assert_eq!(action["kind"].as_str(), Some("summon"));
        assert!(action.get("key").is_none());
        let argv: Vec<_> = action["command"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part.as_str().unwrap())
            .collect();
        assert_eq!(argv.len(), 4);
        assert!(Path::new(argv[0]).is_absolute());
        assert_eq!(
            fs::canonicalize(argv[0]).unwrap(),
            fs::canonicalize(env!("CARGO_BIN_EXE_numen")).unwrap()
        );
        assert_eq!(&argv[1..], &["--socket", socket.to_str().unwrap(), "menu"]);
        assert!(!catalog.exists());
        assert!(!socket.exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn wisp_action_install_preserves_backup_is_idempotent_and_refuses_conflicts() {
    let (dir, socket) = fixture();
    let home = dir.path().join("unused-home");
    let catalog = dir.path().join("actions.toml");
    let original = b"# user comments\n[[action]]\nid = 'other' # retain me\ncommand = ['hello']";
    fs::write(&catalog, original).unwrap();
    let install = || {
        let mut child = ChildGuard(
            command(&socket)
                .args(["wisp-action", "--install", "--file"])
                .arg(&catalog)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("PATH", &home)
                .spawn()
                .unwrap(),
        );
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        (status, stdout, stderr)
    };
    let (status, stdout, stderr) = install();
    assert!(status.success(), "{stderr}");
    let backup = PathBuf::from(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("Snapshot backup: "))
            .expect("missing backup path"),
    );
    assert_eq!(backup.parent(), Some(dir.path()));
    assert_eq!(fs::read(&backup).unwrap(), original);
    let live_backup = PathBuf::from(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("Live inode backup (retains late editor writes): "))
            .expect("missing live inode backup path"),
    );
    assert_eq!(live_backup.parent(), Some(dir.path()));
    assert_eq!(fs::read(&live_backup).unwrap(), original);
    let installed = fs::read(&catalog).unwrap();
    assert!(installed.starts_with(original));
    let value: toml::Value = toml::from_str(std::str::from_utf8(&installed).unwrap()).unwrap();
    let actions = value["action"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["id"].as_str(), Some("other"));
    assert_eq!(actions[1]["id"].as_str(), Some("numen"));
    let entries = fs::read_dir(dir.path()).unwrap().count();
    let (status, stdout, stderr) = install();
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("already installed"), "{stdout}");
    assert_eq!(fs::read(&catalog).unwrap(), installed);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), entries);
    let conflict = b"# custom Numen\n[[action]]\nid = 'numen'\ncommand = ['different']\n";
    fs::write(&catalog, conflict).unwrap();
    let (status, _, stderr) = install();
    assert!(!status.success());
    assert!(stderr.contains("different numen action"), "{stderr}");
    assert_eq!(fs::read(&catalog).unwrap(), conflict);
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), entries);
    assert!(!home.exists());
    assert!(!socket.exists());
}

fn pty() -> (fs::File, fs::File) {
    use std::os::fd::FromRawFd;
    let mut master = -1;
    let mut slave = -1;
    // Both descriptors become owned Files immediately after successful allocation.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", io::Error::last_os_error());
    unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) }
}

#[test]
fn menu_requires_both_ttys_before_probing_or_bootstrapping() {
    let (dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let missing_parent = dir.path().join("missing");
    let absent_socket = missing_parent.join("s");
    let (_master, slave) = pty();
    for path in [&socket, &absent_socket] {
        for (tty_stdin, tty_stderr) in [(false, false), (true, false), (false, true)] {
            let mut cmd = command(path);
            // The absent socket's parent must not become the child's working directory.
            cmd.current_dir(dir.path())
                .args(["menu", "--fzf"])
                .arg(dir.path().join("missing-fzf"))
                .stdin(if tty_stdin {
                    Stdio::from(slave.try_clone().unwrap())
                } else {
                    Stdio::null()
                });
            if tty_stderr {
                cmd.stderr(Stdio::from(slave.try_clone().unwrap()));
            }
            let mut child = ChildGuard(cmd.spawn().unwrap());
            let status = child.wait_until(Instant::now() + LIMIT);
            let (stdout, stderr) = child.output();
            assert!(!status.success());
            assert!(stdout.is_empty(), "{stdout}");
            if !tty_stderr {
                assert!(stderr.contains("interactive TTY"), "{stderr}");
                assert!(stderr.contains("Wisp run/summon, not exec"), "{stderr}");
            }
            assert!(
                matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
            );
            assert!(!missing_parent.exists());
            assert!(!dir.path().join("service.log").exists());
        }
    }
}

#[test]
fn menu_help_lists_flags_without_a_tty_or_service() {
    let (_dir, socket) = fixture();
    let mut child = ChildGuard(
        command(&socket)
            .args(["menu", "--help"])
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let status = child.wait_until(Instant::now() + LIMIT);
    let (stdout, stderr) = child.output();
    assert!(status.success(), "{stderr}");
    for flag in ["--no-start", "--fzf", "--help"] {
        assert!(stdout.contains(flag), "missing {flag}: {stdout}");
    }
    assert!(!stderr.contains("interactive TTY"), "{stderr}");
    assert!(!socket.exists());
}

#[test]
fn service_info_reports_actual_headless_listener_pid_and_capabilities() {
    let (_dir, socket) = fixture();
    let mut listener = ChildGuard(
        command(&socket)
            .args(["listen", "--exit-after", "1.5"])
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + LIMIT;
    drop(ready(&mut listener, &socket));
    let mut info = control(&socket, &["service-info"]);
    let instance = info
        .as_object_mut()
        .unwrap()
        .remove("service_instance")
        .unwrap();
    assert!(
        instance
            .as_str()
            .is_some_and(|instance| !instance.is_empty())
    );
    assert_eq!(
        control(&socket, &["service-info"])["service_instance"],
        instance
    );
    assert_eq!(
        info.as_object_mut().unwrap().remove("settings"),
        Some(json!(false))
    );
    assert_eq!(
        info.as_object_mut().unwrap().remove("familiar_count"),
        Some(json!(1))
    );
    assert_eq!(
        info.as_object_mut().unwrap().remove("source_count"),
        Some(json!(0))
    );
    let diagnostics = info.as_object_mut().unwrap().remove("diagnostics").unwrap();
    assert_eq!(diagnostics["version"], 1);
    assert_eq!(diagnostics["build"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        diagnostics["hotkeys"]["option_i"],
        json!({"state":"unavailable","reason":"headless"})
    );
    assert_eq!(
        diagnostics["hotkeys"]["option_m"],
        diagnostics["hotkeys"]["option_i"]
    );
    let doctor = control(&socket, &["doctor", "--json"]);
    assert_eq!(doctor["service_state"], "reachable");
    assert_eq!(doctor["service"]["pid"], listener.0.id());
    assert_eq!(doctor["service"]["diagnostics"], diagnostics);
    assert_eq!(doctor["client"]["build"], diagnostics["build"]);
    assert_eq!(doctor["build_comparison"], "match");
    assert_eq!(
        info,
        json!({"api_version":1,"pid":listener.0.id(),"graphical":false,
            "lifecycle":false,"navigation":false})
    );
    let status = listener.wait_until(deadline);
    let (_, stderr) = listener.output();
    assert!(status.success(), "{stderr}");
    assert!(!socket.exists());
}

#[test]
fn doctor_offline_does_not_create_socket_parent_or_logs() {
    let (dir, socket) = fixture();
    let leaf = dir.path().join("missing");
    for path in [&socket, &leaf.join("s")] {
        for json in [false, true] {
            let mut cmd = command(path);
            cmd.current_dir(dir.path()).arg("doctor");
            if json {
                cmd.arg("--json");
            }
            let mut child = ChildGuard(cmd.spawn().unwrap());
            let status = child.wait_until(Instant::now() + LIMIT);
            let (stdout, stderr) = child.output();
            assert!(status.success(), "{stderr}");
            assert!(stderr.is_empty(), "{stderr}");
            if json {
                let report: Value = serde_json::from_str(&stdout).unwrap();
                assert_eq!(report["service_state"], "not_running");
                assert_eq!(report["service"], Value::Null);
                assert_eq!(report["build_comparison"], "unknown");
                assert!(Path::new(report["client"]["executable"].as_str().unwrap()).is_absolute());
            } else {
                assert!(stdout.contains("Service: not_running"));
                assert!(stdout.contains("Comparison: unknown"));
            }
            assert!(!leaf.exists());
            assert!(!socket.exists());
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
    // A refused, private stale socket is informational, but must not be removed.
    drop(UnixListener::bind(&socket).unwrap());
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        control(&socket, &["doctor", "--json"])["service_state"],
        "not_running"
    );
    assert!(socket.exists());
}

#[test]
fn doctor_default_socket_resolution_does_not_create_runtime_leaf() {
    let (dir, _) = fixture();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_numen"))
            .args(["doctor", "--json"])
            .current_dir(dir.path())
            .env("XDG_RUNTIME_DIR", dir.path())
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("PATH", dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let status = child.wait_until(Instant::now() + LIMIT);
    let (stdout, stderr) = child.output();
    assert!(status.success(), "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["service_state"], "not_running");
    assert_eq!(
        Path::new(report["socket"].as_str().unwrap()),
        dir.path().join("numen/events.sock")
    );
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

fn doctor_mock(reply: Option<&str>, human: bool) -> (ExitStatus, String, String) {
    let (_dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let mut cmd = command(&socket);
    cmd.arg("doctor");
    if !human {
        cmd.arg("--json");
    }
    let mut child = ChildGuard(cmd.spawn().unwrap());
    let deadline = Instant::now() + LIMIT;
    let stream = loop {
        match server.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "doctor never connected");
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "doctor exited before request"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"version":1,"type":"companion.service"})
    );
    if let Some(reply) = reply {
        writeln!(reader.get_mut(), "{reply}").unwrap();
    }
    let status = child.wait_until(deadline);
    let (stdout, stderr) = child.output();
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "extra request: {line}"
    );
    assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    (status, stdout, stderr)
}

fn doctor_data(data: &Value) -> Value {
    let reply = json!({"version":1,"ok":true,"data":data}).to_string();
    let (status, stdout, stderr) = doctor_mock(Some(&reply), false);
    assert!(status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    serde_json::from_str(&stdout).unwrap()
}

fn legacy_service() -> Value {
    json!({"api_version":1,"pid":123,"service_instance":"mock-service",
        "graphical":true,"lifecycle":true,"navigation":true})
}

// Keep this as wire text: constructing a Value would erase duplicate-key evidence.
const DOCTOR_ACK: &str = r#"{"version":1,"ok":true,"data":{"api_version":1,"pid":123,"service_instance":"mock-service","graphical":true,"lifecycle":true,"navigation":true,"settings":true,"familiar_count":1,"source_count":0,"diagnostics":{"version":1,"executable":"/safe/path","build":{"version":"0.1.0","fingerprint":"unknown","source_fingerprint":"unknown","target":"target","profile":"profile"},"hotkeys":{"option_i":{"state":"registered","reason":null},"option_m":{"state":"registered","reason":null}}}}}"#;

#[test]
fn doctor_rejects_contradictory_shortcut_states_and_per_key_reasons() {
    for (key, state, reason) in [
        ("option_i", "registered", "registration_failed"),
        ("option_m", "registered", "registration_failed"),
        ("option_i", "disabled", "registration_pending"),
        ("option_m", "disabled", "no_bubble"),
        ("option_i", "unavailable", "pointer_initialization_failed"),
        ("option_i", "unsupported", "unsupported_mode"),
    ] {
        let reply = DOCTOR_ACK.replace(
            &format!(r#""{key}":{{"state":"registered","reason":null}}"#),
            &format!(r#""{key}":{{"state":"{state}","reason":"{reason}"}}"#),
        );
        assert_ne!(reply, DOCTOR_ACK);
        let (status, stdout, stderr) = doctor_mock(Some(&reply), false);
        assert_eq!(status.code(), Some(1), "{reply}: {stderr}");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            stderr.contains("invalid service shortcut metadata"),
            "{stderr}"
        );
    }
}

#[test]
fn doctor_rejects_recursive_duplicate_ack_keys_before_value_collapse() {
    for (needle, duplicate) in [
        (r#""pid":123"#, r#""pid":0,"pid":123"#),
        (
            r#""executable":"/safe/path""#,
            r#""executable":"/unsafe/\u202epath","executable":"/safe/path""#,
        ),
        (
            r#""fingerprint":"unknown""#,
            r#""fingerprint":"malicious","fingerprint":"unknown""#,
        ),
        (
            r#""source_fingerprint":"unknown""#,
            r#""source_fingerprint":"malicious","source_fingerprint":"unknown""#,
        ),
        (
            r#""state":"registered""#,
            r#""state":"disabled","state":"registered""#,
        ),
    ] {
        let reply = DOCTOR_ACK.replacen(needle, duplicate, 1);
        assert_ne!(reply, DOCTOR_ACK);
        let (status, stdout, stderr) = doctor_mock(Some(&reply), false);
        assert_eq!(status.code(), Some(1), "{reply}: {stderr}");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(!stderr.is_empty());
        assert!(!stderr.contains("malicious"));
    }
}

#[test]
fn doctor_accepts_bounded_256_byte_build_labels_but_rejects_unsafe_labels() {
    for field in ["version", "target", "profile"] {
        let original = match field {
            "version" => "0.1.0",
            "target" => "target",
            _ => "profile",
        };
        let needle = format!(r#""{field}":"{original}""#);
        let reply = DOCTOR_ACK.replace(&needle, &format!(r#""{field}":"{}""#, "a".repeat(256)));
        assert_ne!(reply, DOCTOR_ACK);
        let (status, stdout, stderr) = doctor_mock(Some(&reply), false);
        assert!(status.success(), "{stderr}");
        let report: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(report["service_state"], "reachable");
        assert_eq!(report["build_comparison"], "unknown");
        assert_eq!(
            report["service"]["diagnostics"]["build"][field],
            "a".repeat(256)
        );
        for value in [
            "a".repeat(257),
            r#"bad\nlabel"#.into(),
            r#"bad\u202elabel"#.into(),
        ] {
            let reply = DOCTOR_ACK.replace(&needle, &format!(r#""{field}":"{value}""#));
            let (status, stdout, stderr) = doctor_mock(Some(&reply), false);
            assert_eq!(status.code(), Some(1), "{reply}: {stderr}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(!stderr.is_empty());
            assert!(!stderr.contains("bad"));
        }
    }
}

#[test]
fn doctor_compares_embedded_metadata_not_pid_path_or_package_version() {
    let (_dir, socket) = fixture();
    let client = control(&socket, &["doctor", "--json"])["client"].clone();
    let mut data = legacy_service();
    data["settings"] = json!(true);
    data["familiar_count"] = json!(2);
    data["source_count"] = json!(3);
    data["diagnostics"] = json!({"version":1,"executable":client["executable"],"build":client["build"],
        "hotkeys":{"option_i":{"state":"registered","reason":null},
        "option_m":{"state":"unavailable","reason":"registration_failed"}}});
    let report = doctor_data(&data);
    assert_eq!(report["service_state"], "reachable");
    assert_eq!(report["build_comparison"], "match");
    let reply = json!({"version":1,"ok":true,"data":data}).to_string();
    let (status, stdout, stderr) = doctor_mock(Some(&reply), true);
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("Comparison: match (recorded build/input fingerprints)"));
    assert!(stdout.contains("Match does not guarantee identical executables"));
    assert!(stdout.contains("different is not proof of age"));
    assert_eq!(report["service"]["source_count"], 3);
    assert_eq!(
        report["service"]["diagnostics"]["hotkeys"]["option_m"]["reason"],
        "registration_failed"
    );
    // Same installed path and version are not enough when recorded build inputs differ.
    data["diagnostics"]["build"]["fingerprint"] =
        json!(format!("numen-build-v1-fnv1a128:{}", "a".repeat(32)));
    assert_eq!(doctor_data(&data)["build_comparison"], "different");
    data["diagnostics"]["build"]["fingerprint"] = json!("unknown");
    assert_eq!(doctor_data(&data)["build_comparison"], "unknown");
    let reply = json!({"version":1,"ok":true,"data":data}).to_string();
    let (status, stdout, stderr) = doctor_mock(Some(&reply), true);
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("Option+I: registered"));
    assert!(stdout.contains("Option+M: unavailable (reason: registration_failed)"));
    assert!(stdout.contains("Comparison: unknown"));
    assert!(!stdout.contains("Comparison: older"));
}

#[test]
fn doctor_legacy_and_future_diagnostics_remain_unknown() {
    let mut data = legacy_service();
    let legacy = doctor_data(&data);
    assert_eq!(legacy["service_state"], "legacy");
    assert_eq!(legacy["build_comparison"], "unknown");
    assert_eq!(legacy["service"]["settings"], Value::Null);
    assert_eq!(legacy["service"]["diagnostics"], Value::Null);
    data["diagnostics"] =
        json!({"version":2,"executable":"bad\npath","hotkeys":{"option_i":{"state":"registered"}}});
    let future = doctor_data(&data);
    assert_eq!(future["service_state"], "unsupported_diagnostics");
    assert_eq!(future["build_comparison"], "unknown");
    assert_eq!(future["service"]["diagnostics"], Value::Null);
    assert!(!future.to_string().contains("bad"));
    let reply = json!({"version":1,"ok":true,"data":legacy_service()}).to_string();
    let (status, stdout, stderr) = doctor_mock(Some(&reply), true);
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("Option+I: unknown"));
    assert!(stdout.contains("Option+M: unknown"));
    assert!(!stdout.contains("disabled"));
    assert!(stdout.contains("legacy, or unsupported diagnostics leave comparison unknown"));
}

#[test]
fn doctor_live_read_and_schema_failures_are_errors_not_offline() {
    let mut cases = vec![
        None,
        Some("not-json".to_owned()),
        Some(json!({"version":2,"ok":true,"data":legacy_service()}).to_string()),
        Some(json!({"version":1,"ok":false,"error":"private untrusted error"}).to_string()),
    ];
    for (field, value) in [
        ("api_version", json!(2)),
        ("pid", json!(0)),
        ("service_instance", json!("bad/id")),
        ("navigation", json!("true")),
        ("diagnostics", json!({"version":1})),
        ("diagnostics", json!(null)),
    ] {
        let mut data = legacy_service();
        data[field] = value;
        cases.push(Some(json!({"version":1,"ok":true,"data":data}).to_string()));
    }
    let (_dir, socket) = fixture();
    let build = control(&socket, &["doctor", "--json"])["client"]["build"].clone();
    for (field, value) in [
        ("executable", json!("bad\npath")),
        ("executable", json!("x".repeat(1025))),
        ("executable", json!("/path/\u{202e}bad")),
        (
            "build",
            json!({"version":"1","fingerprint":"malicious","source_fingerprint":"unknown","target":"t","profile":"p"}),
        ),
        (
            "hotkeys",
            json!({"option_i":{"state":"registered","reason":"arbitrary text"},"option_m":{"state":"registered"}}),
        ),
    ] {
        let mut data = legacy_service();
        data["settings"] = json!(false);
        data["familiar_count"] = json!(1);
        data["source_count"] = json!(0);
        data["diagnostics"] = json!({"version":1,"executable":"/safe/path","build":build,
            "hotkeys":{"option_i":{"state":"registered","reason":null},"option_m":{"state":"registered","reason":null}}});
        data["diagnostics"][field] = value;
        cases.push(Some(json!({"version":1,"ok":true,"data":data}).to_string()));
    }
    for reply in cases {
        let (status, stdout, stderr) = doctor_mock(reply.as_deref(), false);
        assert_eq!(status.code(), Some(1), "{reply:?}: {stderr}");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(!stderr.is_empty());
        assert!(!stderr.contains("private untrusted error"));
    }
}

#[test]
fn doctor_rejects_unsafe_socket_permissions_without_connecting() {
    let (dir, socket) = fixture();
    let server = UnixListener::bind(&socket).unwrap();
    server.set_nonblocking(true).unwrap();
    for (parent_mode, socket_mode) in [(0o700, 0o666), (0o755, 0o600)] {
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(parent_mode)).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(socket_mode)).unwrap();
        let mut child = ChildGuard(command(&socket).args(["doctor", "--json"]).spawn().unwrap());
        let status = child.wait_until(Instant::now() + LIMIT);
        let (stdout, stderr) = child.output();
        assert_eq!(status.code(), Some(1));
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
        assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        assert!(socket.exists());
        assert!(!dir.path().join("service.log").exists());
    }
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn menu_with_mock_fzf_and_pty_dispatches_info_without_a_gui() {
    let (dir, socket) = fixture();
    let fzf = dir.path().join("fake-fzf");
    // Consume every offered row, then return a real row with fzf's blank expect-key line.
    fs::write(&fzf, b"#!/bin/sh\nselected=''\nwhile IFS= read -r row; do\n  case \"$row\" in\n    \"familiar:main\t\"*|\"info\t\"*) selected=$row ;;\n  esac\ndone\n[ -n \"$selected\" ] || exit 1\nprintf '\\n%s\\n' \"$selected\"\n").unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o700)).unwrap();
    let server = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    server.set_nonblocking(true).unwrap();
    let (_master, slave) = pty();
    let deadline = Instant::now() + LIMIT;
    let mut child = ChildGuard(
        command(&socket)
            .args(["menu", "--no-start", "--fzf"])
            .arg(&fzf)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap(),
    );
    let mut dispatched = false;
    let mut discovered = false;
    while !dispatched {
        assert!(Instant::now() < deadline, "menu did not dispatch Info");
        let stream = match server.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "menu exited before Info"
                );
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) => panic!("accept: {error}"),
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0, "missing request");
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["version"], 1);
        let data = match request["type"].as_str().unwrap() {
            "companion.service" => {
                assert_eq!(request, json!({"version":1,"type":"companion.service"}));
                json!({"api_version":1,"pid":123,"graphical":true,
                    "lifecycle":true,"navigation":false,"service_instance":"mock-menu-service"})
            }
            "familiar.list" => {
                assert_eq!(
                    request,
                    json!({"version":1,"type":"familiar.list","offset":0,
                    "service_instance":"mock-menu-service"})
                );
                discovered = true;
                json!({"familiars":[{"id":"main","preset":"djinn","attachment":null,
                    "available":false,"summary":null,"capabilities":["info"]}],
                    "offset":0,"total":1,"next_offset":null})
            }
            "companion.reveal" => {
                assert!(discovered);
                assert_eq!(
                    request,
                    json!({"version":1,"type":"companion.reveal","familiars":["main"],
                        "service_instance":"mock-menu-service"})
                );
                dispatched = true;
                json!({"summary":"Info must not print this status data"})
            }
            _ => panic!("unexpected menu request: {request}"),
        };
        writeln!(
            reader.get_mut(),
            "{}",
            json!({"version":1,"ok":true,"data":data})
        )
        .unwrap();
        line.clear();
        assert_eq!(
            reader.read_line(&mut line).unwrap(),
            0,
            "extra request: {line}"
        );
    }
    let status = child.wait_until(deadline);
    let (stdout, _) = child.output();
    assert!(status.success(), "menu failed: {status}");
    assert_eq!(stdout.trim(), "Accepted info: main");
    assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert!(!dir.path().join("service.log").exists());
}

#[test]
fn menu_settings_with_mock_fzf_and_pty_commits_once_or_cancels_without_writes() {
    for cancel in [false, true] {
        let (dir, socket) = fixture();
        let fzf = dir.path().join("fake-fzf");
        // The value-stage cancellation is followed by Esc at fields, commands and families.
        let script = format!(
            r#"#!/bin/sh
n=0
[ ! -f '{state}' ] || read -r n < '{state}'
n=$((n + 1))
printf '%s\n' "$n" > '{state}'
case "$n" in
  1) key='familiar:main' ;;
  2) key='settings' ;;
  3) key='font_size' ;;
  4) key='value:2' ;;
  *) key='' ;;
esac
selected=''
while IFS= read -r row; do
  case "$row" in "$key"'\t'*) selected=$row ;; esac
done
[ '{cancel}' != true ] || [ "$n" -lt 4 ] || exit 130
[ -n "$selected" ] || exit 130
printf '\n%s\n' "$selected"
"#,
            state = dir.path().join("stage").display(),
            cancel = cancel
        )
        .replace("'\\t'", "'\t'");
        fs::write(&fzf, script).unwrap();
        fs::set_permissions(&fzf, fs::Permissions::from_mode(0o700)).unwrap();
        let server = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        server.set_nonblocking(true).unwrap();
        let (_master, slave) = pty();
        let mut child = ChildGuard(
            command(&socket)
                .args(["menu", "--no-start", "--fzf"])
                .arg(&fzf)
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave))
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + LIMIT;
        let mut queries = 0;
        let mut writes = 0;
        let status = loop {
            assert!(Instant::now() < deadline, "Settings menu exceeded deadline");
            let stream = match server.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.0.try_wait().unwrap() {
                        break status;
                    }
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("accept: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            let request: Value = serde_json::from_str(&line).unwrap();
            let data = match request["type"].as_str().unwrap() {
                "companion.service" => {
                    assert_eq!(request, json!({"version":1,"type":"companion.service"}));
                    json!({"api_version":1,"pid":123,"graphical":true,"lifecycle":true,
                        "navigation":false,"service_instance":"settings-generation"})
                }
                "familiar.list" => {
                    queries += 1;
                    assert_eq!(
                        request,
                        json!({"version":1,"type":"familiar.list","offset":0,
                        "service_instance":"settings-generation"})
                    );
                    json!({"familiars":[{"id":"main","preset":"djinn","attachment":null,
                        "available":false,"capabilities":["settings"],
                        "settings":{"sprite_scale":0.5,"font_size":12.0,"bubble_duration":5.0}}],
                        "offset":0,"total":1,"next_offset":null})
                }
                "familiar.settings" => {
                    assert!(!cancel, "cancel dispatched settings");
                    writes += 1;
                    assert_eq!(
                        request,
                        json!({"version":1,"type":"familiar.settings","ids":["main"],
                        "patch":{"font_size":16.0},"service_instance":"settings-generation"})
                    );
                    json!({"updated":["main"]})
                }
                _ => panic!("unexpected request: {request}"),
            };
            writeln!(
                reader.get_mut(),
                "{}",
                json!({"version":1,"ok":true,"data":data})
            )
            .unwrap();
            line.clear();
            assert_eq!(
                reader.read_line(&mut line).unwrap(),
                0,
                "extra request: {line}"
            );
        };
        let (stdout, _) = child.output();
        assert!(status.success(), "Settings menu failed: {status}");
        assert_eq!(queries, 2);
        assert_eq!(writes, usize::from(!cancel));
        assert_eq!(
            stdout.trim(),
            if cancel {
                ""
            } else {
                "Accepted settings: main"
            }
        );
        assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        assert!(!dir.path().join("service.log").exists());
    }
}
