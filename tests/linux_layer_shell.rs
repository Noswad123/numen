#![cfg(target_os = "linux")]

use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
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

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("numen-layer-")
        .tempdir()
        .unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn logs(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).unwrap()).into_owned()
}

fn command(root: &Path, socket: &Path) -> Command {
    assert!(socket.as_os_str().as_bytes().len() <= 100);
    let mut command = Command::new(env!("CARGO_BIN_EXE_numen"));
    command
        .arg("--socket")
        .arg(socket)
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("RUST_LOG", "numen=debug,wgpu=warn")
        .env("NO_COLOR", "1")
        .env_remove("ARCANA_PUBLIC_NUMEN")
        .env_remove("DISPLAY")
        .env_remove("WINIT_UNIX_BACKEND")
        .stdin(Stdio::null());
    // Preserve the real runtime directory and Wayland authentication for smoke tests.
    command
}

fn spawn(command: &mut Command, log: &Path) -> ChildGuard {
    let output = File::create(log).unwrap();
    ChildGuard(
        command
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    )
}

fn no_side_effects(root: &Path) {
    for path in [
        "missing-socket-parent",
        "config",
        "cache",
        "data",
        "missing-runtime",
    ] {
        assert!(
            !root.join(path).exists(),
            "unexpected filesystem write: {path}"
        );
    }
}

fn headless(root: &Path) -> Command {
    let mut command = command(root, &root.join("missing-socket-parent/s"));
    command
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env("XDG_RUNTIME_DIR", root.join("missing-runtime"));
    command
}

#[test]
fn forced_unavailable_layer_shell_fails_before_socket_or_preferences() {
    let dir = fixture();
    let log = dir.path().join("unavailable.log");
    let mut app = spawn(
        headless(dir.path()).args([
            "run",
            "--backend",
            "layer-shell",
            "--empty",
            "--no-hotkey",
            "--exit-after=60",
        ]),
        &log,
    );
    let started = Instant::now();
    assert!(!app.wait(&log).success(), "{}", logs(&log));
    assert!(started.elapsed() < Duration::from_secs(5), "{}", logs(&log));
    let text = logs(&log);
    assert!(
        text.contains("connect to existing Wayland display"),
        "{text}"
    );
    assert!(!text.contains("Local activity listener ready"), "{text}");
    assert!(!text.contains("backend=Winit"), "{text}");
    no_side_effects(dir.path());
}

#[test]
fn forced_layer_shell_rejects_window_modes_before_preflight_or_side_effects() {
    for option in ["--focus", "--normal-window-level"] {
        let dir = fixture();
        let log = dir.path().join("incompatible.log");
        let mut app = spawn(
            headless(dir.path()).args(["run", "--backend", "layer-shell", option]),
            &log,
        );
        assert!(!app.wait(&log).success(), "{option}: {}", logs(&log));
        let text = logs(&log);
        assert!(
            text.contains("layer-shell has no normal-window/focus mode"),
            "{text}"
        );
        assert!(text.contains("--backend winit"), "{text}");
        assert!(
            !text.contains("connect to existing Wayland display"),
            "{text}"
        );
        assert!(!text.contains("Local activity listener ready"), "{text}");
        no_side_effects(dir.path());
    }
}

#[test]
fn backend_help_accepts_winit_and_validates_values_without_side_effects() {
    let dir = fixture();
    let log = dir.path().join("help.log");
    let mut app = spawn(
        headless(dir.path()).args(["run", "--backend", "winit", "--help"]),
        &log,
    );
    assert!(app.wait(&log).success(), "{}", logs(&log));
    let text = logs(&log);
    for expected in ["--backend", "auto", "winit", "layer-shell"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let mut app = spawn(
        headless(dir.path()).args(["run", "--backend", "not-a-backend"]),
        &log,
    );
    assert!(!app.wait(&log).success(), "{}", logs(&log));
    let text = logs(&log);
    assert!(text.contains("invalid value 'not-a-backend'"), "{text}");
    assert!(
        text.contains("winit") && text.contains("layer-shell"),
        "{text}"
    );
    no_side_effects(dir.path());
}

fn control(root: &Path, socket: &Path, args: &[&str]) -> Value {
    let stdout = root.join("client.out");
    let stderr = root.join("client.err");
    // All child output uses private files, never potentially blocking pipes.
    let mut child = ChildGuard(
        command(root, socket)
            .args(args)
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    assert!(child.wait(&stderr).success(), "{args:?}: {}", logs(&stderr));
    serde_json::from_str(&logs(&stdout)).expect("one JSON control response")
}

fn observed(child: &mut ChildGuard, log: &Path, from: usize, needles: &[&str]) -> (usize, String) {
    let deadline = Instant::now() + LIMIT;
    loop {
        let text = logs(log);
        assert!(
            !text.contains(" ERROR ") && !text.contains("panicked at"),
            "{text}"
        );
        let mut end = from;
        for line in text[from..].split_inclusive('\n') {
            end += line.len();
            if line.ends_with('\n') && needles.iter().all(|s| line.contains(s)) {
                return (end, line.to_owned());
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

fn numeric_field(line: &str, name: &str) -> u32 {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(name))
        .unwrap_or_else(|| panic!("missing {name}: {line}"))
        .parse()
        .unwrap()
}

fn logical_size(line: &str) -> (u32, u32) {
    (
        numeric_field(line, "logical_width="),
        numeric_field(line, "logical_height="),
    )
}

fn bubble_dimensions(line: &str) -> (u32, u32) {
    let size = logical_size(line);
    // Default scene scale is 1.5: preferred 384x144, readability floor 160x80.
    let preferred = (576, 216);
    assert!(size.0 >= 240 && size.0 <= preferred.0, "{line}");
    assert!(size.1 >= 120 && size.1 <= preferred.1, "{line}");
    if line.contains("reflowed=true") {
        assert_ne!(size, preferred, "{line}");
    } else {
        assert!(line.contains("reflowed=false"), "{line}");
        assert_eq!(size, preferred, "unreflowed bubble resized: {line}");
    }
    assert!(line.contains("input_empty=true"), "{line}");
    size
}

/// cargo test --test linux_layer_shell -- --ignored --nocapture
#[test]
#[ignore = "requires a running Wayland desktop with layer-shell support"]
fn auto_native_lifecycle_concurrent_presets_settings_and_transient_bubbles() {
    let dir = fixture();
    let root = dir.path();
    let socket: PathBuf = root.join("s");
    let log = root.join("layer-shell.log");
    let mut app = spawn(
        command(root, &socket).args([
            "run",
            "--empty",
            "--no-preferences",
            "--no-hotkey",
            "--bubble-duration=2",
            "--exit-after=60",
        ]),
        &log,
    );
    observed(&mut app, &log, 0, &["Local activity listener ready"]);
    observed(
        &mut app,
        &log,
        0,
        &["Desktop backend selected backend=LayerShell"],
    );
    assert_eq!(control(root, &socket, &["service-info"])["graphical"], true);
    assert_eq!(control(root, &socket, &["familiars"])["total"], 0);
    let response = control(root, &socket, &["presets"]);
    let presets: Vec<_> = response["presets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(!presets.is_empty());
    // Keep every preset alive concurrently; each must reach a native commit.
    let mut initial_sizes = BTreeMap::new();
    for id in &presets {
        let start = logs(&log).len();
        control(root, &socket, &["open", "--id", id, "--preset", id]);
        let familiar = format!("familiar={id} ");
        observed(&mut app, &log, start, &["Familiar ready", &familiar]);
        let (_, line) = observed(
            &mut app,
            &log,
            start,
            &["Layer sprite presented", &familiar],
        );
        let sprite = logical_size(&line);
        // Initial creation must render pixels; later commits may retain this count.
        assert!(numeric_field(&line, "input_rectangles=") > 0, "{line}");
        assert!(numeric_field(&line, "raster_scale=") >= 1, "{line}");
        let (_, line) = observed(
            &mut app,
            &log,
            start,
            &["Layer bubble presented", &familiar, "input_empty=true"],
        );
        let bubble = bubble_dimensions(&line);
        assert!(bubble.0 > 0 && bubble.1 > 0, "{line}");
        assert_ne!(
            bubble, sprite,
            "bubble must have its own logical dimensions"
        );
        initial_sizes.insert(id, (sprite, bubble));
    }
    assert_eq!(
        control(root, &socket, &["familiars"])["total"],
        presets.len()
    );
    for id in &presets {
        let familiar = format!("familiar={id} ");
        let start = logs(&log).len();
        control(
            root,
            &socket,
            &[
                "settings",
                "--familiar",
                id,
                "--sprite-scale=0.75",
                "--font-size=16",
                "--bubble-duration=2",
            ],
        );
        // A frame queued before the settings ACK can still carry the old size.
        let (initial, _) = initial_sizes[id];
        let width = format!("logical_width={} ", initial.0 * 3 / 2);
        let height = format!("logical_height={} ", initial.1 * 3 / 2);
        let (applied_end, applied) = observed(
            &mut app,
            &log,
            start,
            &["Layer sprite presented", &familiar, &width, &height],
        );
        let size = logical_size(&applied);
        assert!(size.0 > 0 && size.1 > 0, "{applied}");
        // Resizing also requires a fresh sprite raster and shaped input region.
        assert!(
            numeric_field(&applied, "input_rectangles=") > 0,
            "{applied}"
        );
        assert!(numeric_field(&applied, "raster_scale=") >= 1, "{applied}");
        let (_, line) = observed(
            &mut app,
            &log,
            applied_end,
            &["Layer bubble presented", &familiar, "input_empty=true"],
        );
        // Settings may reduce available space by enlarging the sprite. Reflow is
        // bounded above; reveal and expiry below must retain this applied size.
        let bubble_size = bubble_dimensions(&line);
        assert_ne!(bubble_size, size);
        let mut offset = 0;
        let entry = loop {
            let registry = control(
                root,
                &socket,
                &["familiars", "--offset", &offset.to_string()],
            );
            if let Some(entry) = registry["familiars"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["id"] == id.as_str())
            {
                break entry.clone();
            }
            let next = registry["next_offset"]
                .as_u64()
                .unwrap_or_else(|| panic!("missing {id}: {registry}"));
            assert!(next > offset);
            offset = next;
        };
        if id == "random" {
            assert_ne!(entry["preset"], "random");
            assert!(
                presets
                    .iter()
                    .any(|preset| entry["preset"] == preset.as_str())
            );
        } else {
            assert_eq!(entry["preset"], id.as_str());
        }
        assert_eq!(
            entry["settings"],
            json!({
                "sprite_scale": 0.75, "font_size": 16.0, "bubble_duration": 2.0,
            })
        );
        for _ in 0..2 {
            let start = logs(&log).len();
            control(root, &socket, &["bubbles", "--familiar", id]);
            let (shown, line) = observed(
                &mut app,
                &log,
                start,
                &["Layer sprite presented", &familiar, "bubble_visible=true"],
            );
            assert_eq!(logical_size(&line), size, "reveal resized sprite: {line}");
            let (_, line) = observed(
                &mut app,
                &log,
                start,
                &[
                    "Layer bubble presented",
                    &familiar,
                    " visible=true ",
                    "requested_visible=true",
                    "suppressed=false",
                    "input_empty=true",
                ],
            );
            assert!(
                ["Bottom", "Top", "Left", "Right"]
                    .iter()
                    .any(|edge| line.contains(&format!("tail_edge=Some({edge})"))),
                "visible bubble must have a directional tail: {line}"
            );
            eprintln!("native visible commit: {}", line.trim());
            assert_eq!(
                bubble_dimensions(&line),
                bubble_size,
                "reveal resized bubble: {line}"
            );
            let (_, line) = observed(
                &mut app,
                &log,
                shown,
                &["Layer sprite presented", &familiar, "bubble_visible=false"],
            );
            assert_eq!(logical_size(&line), size, "expiry resized sprite: {line}");
            let (_, line) = observed(
                &mut app,
                &log,
                shown,
                &[
                    "Layer bubble presented",
                    &familiar,
                    " visible=false ",
                    "requested_visible=false",
                    "suppressed=false",
                    "input_empty=true",
                ],
            );
            assert_eq!(
                bubble_dimensions(&line),
                bubble_size,
                "expiry resized bubble: {line}"
            );
        }
    }
    for id in &presets {
        control(root, &socket, &["close", "--familiar", id]);
    }
    assert_eq!(control(root, &socket, &["familiars"])["total"], 0);
    assert!(
        app.0.try_wait().unwrap().is_none(),
        "last close stopped service: {}",
        logs(&log)
    );
    let start = logs(&log).len();
    control(
        root,
        &socket,
        &["open", "--id", "reopened", "--preset", &presets[0]],
    );
    observed(
        &mut app,
        &log,
        start,
        &["Familiar ready", "familiar=reopened "],
    );
    observed(
        &mut app,
        &log,
        start,
        &["Layer sprite presented", "familiar=reopened "],
    );
    observed(
        &mut app,
        &log,
        start,
        &[
            "Layer bubble presented",
            "familiar=reopened ",
            "input_empty=true",
        ],
    );
    control(root, &socket, &["quit"]);
    assert!(app.wait(&log).success(), "{}", logs(&log));
    assert!(!socket.exists(), "private socket was not cleaned up");
    no_side_effects(root);
    assert!(!logs(&log).contains("backend=Winit"), "{}", logs(&log));
}
