#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(10);

struct ChildGuard(Child);

impl ChildGuard {
    fn wait(&mut self, log: &Path) -> ExitStatus {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child timed out: {}", logs(log));
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn logs(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).unwrap()).into_owned()
}

fn command(socket: &Path) -> Command {
    let root = socket.parent().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_numen"));
    command
        .arg("--socket")
        .arg(socket)
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("RUST_LOG", "numen=debug,wgpu=warn")
        .env("NO_COLOR", "1")
        .env_remove("ARCANA_PUBLIC_NUMEN")
        .stdin(Stdio::null());
    // Preserve runtime-dir and authentication: Wayland sockets live in the real
    // XDG_RUNTIME_DIR. Winit 0.30 selects via display variables, not this hint.
    match std::env::var("WINIT_UNIX_BACKEND").as_deref() {
        Ok("wayland") => {
            command.env_remove("DISPLAY");
        }
        Ok("x11") => {
            command
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET");
        }
        Err(std::env::VarError::NotPresent) => {}
        other => panic!("unsupported WINIT_UNIX_BACKEND: {other:?}"),
    }
    command
}

fn control(socket: &Path, args: &[&str]) -> Value {
    let root = socket.parent().unwrap();
    let stdout = root.join("client.out");
    let stderr = root.join("client.err");
    // Files have no bounded pipe buffer, even if the service/client is noisy.
    let mut child = ChildGuard(
        command(socket)
            .args(args)
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let status = child.wait(&stderr);
    assert!(status.success(), "{args:?}: {}", logs(&stderr));
    serde_json::from_str(&logs(&stdout)).expect("one JSON control response")
}

fn observed(child: &mut ChildGuard, log: &Path, from: usize, needles: &[&str]) -> usize {
    let deadline = Instant::now() + LIMIT;
    loop {
        let text = logs(log);
        let mut end = from;
        for line in text[from..].split_inclusive('\n') {
            end += line.len();
            if line.ends_with('\n') && needles.iter().all(|s| line.contains(s)) {
                return end;
            }
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "service exited: {text}"
        );
        assert!(Instant::now() < deadline, "missing {needles:?}: {text}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Opt in with WINIT_UNIX_BACKEND=wayland (or x11) cargo test
/// --test linux_desktop -- --ignored --nocapture.
#[test]
#[ignore = "requires a running Linux desktop and GPU"]
fn graphical_lifecycle_settings_and_transient_bubbles() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("s");
    assert!(socket.as_os_str().as_bytes().len() <= 100);
    let log = dir.path().join("desktop.log");
    let output = File::create(&log).unwrap();
    let mut app = ChildGuard(
        command(&socket)
            .args([
                "run",
                "--empty",
                "--no-preferences",
                "--no-hotkey",
                "--normal-window-level",
                "--bubble-duration=0.25",
                "--exit-after=90",
            ])
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    );
    observed(&mut app, &log, 0, &["Local activity listener ready"]);
    assert_eq!(control(&socket, &["service-info"])["graphical"], true);
    assert_eq!(control(&socket, &["familiars"])["total"], 0);
    let presets = control(&socket, &["presets"]);
    let backend = match std::env::var("WINIT_UNIX_BACKEND").as_deref() {
        Ok("wayland") => "backend=Wayland",
        Ok("x11") => "backend=X11",
        _ => "",
    };
    for id in ["amethyst", "ruby", "saphire", "emerald"] {
        let available = presets["presets"].as_array().unwrap();
        assert!(available.iter().any(|p| p["id"] == id));
        let start = logs(&log).len();
        // Closed IDs remain reserved for the service lifetime.
        control(&socket, &["open", "--id", id, "--preset", id]);
        observed(
            &mut app,
            &log,
            start,
            &["Graphics initialized", id, backend],
        );
        let ready = observed(&mut app, &log, start, &["Familiar ready", id]);
        observed(&mut app, &log, ready, &["Familiar frame presented", id]);
        let start = logs(&log).len();
        control(
            &socket,
            &[
                "settings",
                "--familiar",
                id,
                "--sprite-scale=0.75",
                "--font-size=16",
                "--bubble-duration=0.25",
            ],
        );
        let applied = observed(&mut app, &log, start, &["Familiar settings applied", id]);
        observed(
            &mut app,
            &log,
            applied,
            &[
                "Familiar frame presented",
                id,
                "sprite_scale=0.75",
                "font_size=16",
            ],
        );
        let familiar = &control(&socket, &["familiars"])["familiars"][0];
        assert_eq!(familiar["preset"], id);
        assert_eq!(
            familiar["settings"],
            json!({"sprite_scale":0.75,"font_size":16.0,"bubble_duration":0.25})
        );
        // Observe a full reveal/expiry cycle twice, not merely the reveal ACK.
        for _ in 0..2 {
            let start = logs(&log).len();
            control(&socket, &["bubbles", "--familiar", id]);
            let shown = observed(
                &mut app,
                &log,
                start,
                &["Bubble visibility changed", "visible=true", id],
            );
            observed(
                &mut app,
                &log,
                shown,
                &["Familiar frame presented", "bubble_visible=true", id],
            );
            let hidden = observed(
                &mut app,
                &log,
                shown,
                &["Bubble visibility changed", "visible=false", id],
            );
            observed(
                &mut app,
                &log,
                hidden,
                &["Familiar frame presented", "bubble_visible=false", id],
            );
        }
        let start = logs(&log).len();
        control(&socket, &["close", "--familiar", id]);
        observed(&mut app, &log, start, &["Familiar view closed", id]);
        assert_eq!(control(&socket, &["familiars"])["total"], 0);
        assert!(
            app.0.try_wait().unwrap().is_none(),
            "closing last view stopped service"
        );
    }
    let start = logs(&log).len();
    control(
        &socket,
        &["open", "--id", "reopened", "--preset", "amethyst"],
    );
    observed(
        &mut app,
        &log,
        start,
        &["Graphics initialized", "reopened", backend],
    );
    let ready = observed(&mut app, &log, start, &["Familiar ready", "reopened"]);
    observed(
        &mut app,
        &log,
        ready,
        &["Familiar frame presented", "reopened"],
    );
    control(&socket, &["quit"]);
    assert!(app.wait(&log).success(), "{}", logs(&log));
    assert!(!socket.exists(), "socket was not cleaned up");
    assert!(
        !dir.path().join("config").exists(),
        "preferences/config were written"
    );
}
