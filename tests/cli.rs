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
