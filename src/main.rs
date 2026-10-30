mod activity;
mod animation;
mod assets;
mod bootstrap;
mod bubble;
mod build_info;
mod context_menu;
mod diagnostics;
mod display;
mod doctor;
mod hotkey;
mod ipc;
mod menu;
mod motion;
mod pointer;
mod preferences;
mod protocol;
mod typography;
mod visibility;
mod window;
mod wisp_action;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use animation::Animation;
use assets::{CELL_HEIGHT, CELL_WIDTH, DEFAULT_CHARACTER_ID, DEFAULT_CHARACTER_NAME, SpriteAtlas};
use protocol::ActivityState;

#[derive(Parser)]
#[command(version, about = "Numen — an independent desktop familiar")]
struct Cli {
    /// Override the Unix socket path (parent must be a private, user-owned directory).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the default character and listen for local activity events.
    #[command(about = format!("Show {DEFAULT_CHARACTER_NAME} and listen for local activity events."))]
    Run(RunOptions),
    /// Run the same event service without a window; print presentation changes as JSON.
    Listen(ListenOptions),
    /// Publish a diagnostic activity, keeping its connection alive for the selected duration.
    Publish(PublishOptions),
    /// Inspect the selected conversation and a page of available activities.
    Status(StatusOptions),
    /// Select a conversation for the shared familiar, or cycle to the next one.
    Focus(FocusOptions),
    /// Briefly reveal all enabled bubbles, or only the named familiars.
    Bubbles(BubbleOptions),
    /// List running familiar instances (including unavailable attachments).
    Familiars(PageOptions),
    /// List compatible live targets and their current familiar owners.
    Targets(TargetOptions),
    /// List available character presets without starting a service.
    Presets,
    /// Open an unattached familiar, or attach it to a distinct live source.
    Open(OpenOptions),
    /// Choose a distinct source for each selected familiar.
    Attach(AttachOptions),
    /// Remove selected familiars without stopping their source applications.
    Close(CloseOptions),
    /// Run selected familiars to the live mouse pointer and celebrate once on arrival.
    GoToMouse(CloseOptions),
    /// Atomically update presentation settings for explicitly selected familiars.
    Settings(SettingsOptions),
    /// Manage remembered appearance and manual placement without changing live attachments.
    Preferences {
        #[command(subcommand)]
        command: PreferenceCommand,
    },
    /// Familiar-first terminal menu for Wisp or an interactive terminal.
    Menu(menu::MenuOptions),
    /// Print a Wisp launcher action, or install it explicitly with a backup.
    WispAction(wisp_action::ActionOptions),
    /// Inspect the running service's version, PID and graphical capabilities.
    ServiceInfo,
    /// Report client/service build identity and shortcut readiness without changing state.
    Doctor {
        /// Emit a single machine-readable JSON report.
        #[arg(long)]
        json: bool,
    },
    /// Gracefully stop the graphical service and its familiars, not source applications.
    Quit,
    /// Validate the bundled atlas and print clip information without opening a window.
    CheckAssets,
    /// Print the license for the bundled fallback font.
    FontLicense,
}

#[derive(Args, Clone)]
pub struct RunOptions {
    /// Disable the event listener; use only local expression controls.
    #[arg(long)]
    manual: bool,
    /// Start the graphical service with no initial familiar (menu bootstrap).
    #[arg(long, conflicts_with_all = ["manual", "demo"])]
    empty: bool,
    /// Hide the conversation thought bubble (also useful for privacy).
    #[arg(long)]
    no_bubble: bool,
    /// Keep the bubble visible instead of using transient notifications.
    #[arg(long, conflicts_with = "no_bubble")]
    always_bubble: bool,
    /// Duration of a notification or on-demand reveal, in seconds.
    #[arg(long, default_value = "5", value_parser = parse_bubble_duration)]
    bubble_duration: Duration,
    /// Do not register macOS system-wide Option+I, Option+M, or Option+C shortcuts (focused-window controls remain).
    #[arg(long)]
    no_hotkey: bool,
    /// Disable all saved-preference reads and writes; keep runtime settings available.
    #[arg(long, conflicts_with = "preferences_file")]
    no_preferences: bool,
    /// Use an absolute preferences file instead of the XDG/HOME default (live mode only).
    #[arg(long, value_parser = parse_preferences_path)]
    preferences_file: Option<PathBuf>,
    /// Installed Regular font family; defaults to SauceCodePro Nerd Font when available.
    #[arg(long, conflicts_with = "font_file", value_parser = parse_font_family)]
    font_family: Option<String>,
    /// Load a specific TTF/OTF file instead of discovering an installed font family.
    #[arg(long, conflicts_with = "font_family")]
    font_file: Option<PathBuf>,
    /// Bubble font size in logical canvas units (scaled with the scene).
    #[arg(long, default_value = "12", value_parser = parse_font_size)]
    font_size: f32,
    /// Scale only Djinn's artwork, without shrinking the bubble or its text.
    #[arg(long, default_value = "0.5", value_parser = parse_sprite_scale)]
    sprite_scale: f32,
    /// Initial expression. Success, failure, and waving play once, then return to idle.
    #[arg(long, value_enum, default_value = "idle")]
    animation: Animation,
    /// Scale the whole scene; sprite size can be adjusted separately with --sprite-scale.
    #[arg(long, default_value = "1.5", value_parser = parse_scale)]
    scale: f64,
    /// Go-to-mouse movement speed in desktop points per second.
    #[arg(long, default_value = "360", value_parser = parse_move_speed)]
    move_speed: f64,
    /// Cycle through every expression automatically, three seconds per selection.
    #[arg(long, conflicts_with = "paused")]
    demo: bool,
    /// Start with animation paused (also useful for reduced motion).
    #[arg(long)]
    paused: bool,
    /// Show an opaque black background to diagnose transparency problems.
    #[arg(long)]
    opaque: bool,
    /// Use normal window stacking instead of requesting always-on-top.
    #[arg(long)]
    normal_window_level: bool,
    /// Explicitly request keyboard focus at startup; otherwise leave focus alone.
    #[arg(long)]
    focus: bool,
    /// Close automatically after this many seconds, for a bounded graphical smoke test.
    #[arg(long, value_parser = parse_seconds)]
    exit_after: Option<Duration>,
}

#[derive(Args)]
struct ListenOptions {
    /// Stop after this many seconds; otherwise run until interrupted.
    #[arg(long, value_parser = parse_seconds)]
    exit_after: Option<Duration>,
}

#[derive(Args)]
struct StatusOptions {
    /// Index of the first activity to list (four activities per page).
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, value_parser = parse_id)]
    familiar: Option<String>,
}

#[derive(Args)]
struct PageOptions {
    #[arg(long, default_value_t = 0)]
    offset: usize,
}

#[derive(Args)]
struct TargetOptions {
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Include this familiar's own target as eligible for reattachment.
    #[arg(long, value_parser = parse_id)]
    familiar: Option<String>,
}

#[derive(Args)]
struct BubbleOptions {
    /// Omit to reveal all; repeat to select individual familiars.
    #[arg(long, num_args = 1.., value_parser = parse_id)]
    familiar: Vec<String>,
}

#[derive(Args)]
struct CloseOptions {
    #[arg(long, required = true, num_args = 1.., value_parser = parse_id)]
    familiar: Vec<String>,
}

#[derive(Subcommand)]
enum PreferenceCommand {
    /// Forget saved appearance/placement for selected chats; leave current views unchanged.
    Forget(CloseOptions),
}

#[derive(Args)]
#[command(
    after_help = "Supply one or more setting flags. Unspecified settings and unselected familiars stay unchanged. With remembering enabled, edits are saved asynchronously for the attached chat."
)]
#[command(group(ArgGroup::new("patch").required(true).multiple(true)
    .args(["sprite_scale", "font_size", "bubble_duration"])))]
struct SettingsOptions {
    /// Explicit 1–16 unique existing familiar IDs; there is no implicit all.
    #[arg(long, required = true, num_args = 1..=16, value_parser = parse_id)]
    familiar: Vec<String>,
    /// Sprite-only scale, 0.25–2; independent of bubble text size.
    #[arg(long, value_parser = parse_sprite_scale)]
    sprite_scale: Option<f32>,
    /// Bubble font size, 8–32 logical canvas units.
    #[arg(long, value_parser = parse_font_size)]
    font_size: Option<f32>,
    /// Duration in seconds, 0.25–30; applies to future accepted reveals only.
    #[arg(long, value_parser = parse_bubble_duration)]
    bubble_duration: Option<Duration>,
}

impl SettingsOptions {
    fn request(self) -> Result<serde_json::Value> {
        protocol::validate_ids(&self.familiar)?;
        let patch = protocol::SettingsPatch {
            sprite_scale: self.sprite_scale.map(f64::from),
            font_size: self.font_size.map(f64::from),
            bubble_duration: self.bubble_duration.map(|value| value.as_secs_f64()),
        };
        let request = serde_json::json!({"version":1,"type":"familiar.settings",
            "ids":self.familiar,"patch":patch});
        protocol::Request::parse(&serde_json::to_vec(&request)?)?;
        Ok(request)
    }
}

fn parse_preferences_path(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("preferences file must be an absolute file path".into());
    }
    Ok(path)
}

#[derive(Args)]
struct AttachTarget {
    #[arg(long, required_unless_present = "batch", conflicts_with = "batch", value_parser = parse_id)]
    source: Option<String>,
    #[arg(long, conflicts_with = "batch", value_parser = parse_id)]
    instance: Option<String>,
    #[arg(long, required_unless_present = "batch", conflicts_with = "batch", value_parser = parse_id)]
    activity: Option<String>,
}

#[derive(Args)]
struct OpenTarget {
    #[arg(long, requires = "activity", conflicts_with = "batch", value_parser = parse_id)]
    source: Option<String>,
    #[arg(long, requires_all = ["source", "activity"], conflicts_with = "batch", value_parser = parse_id)]
    instance: Option<String>,
    #[arg(long, requires = "source", conflicts_with = "batch", value_parser = parse_id)]
    activity: Option<String>,
}

#[derive(Args)]
struct OpenOptions {
    /// Optional stable ID; otherwise generated by the running service.
    #[arg(long, conflicts_with = "batch", value_parser = parse_id)]
    id: Option<String>,
    #[arg(long, default_value = "djinn", conflicts_with = "batch", value_parser = parse_id)]
    preset: String,
    #[command(flatten)]
    target: OpenTarget,
    /// Read a JSON array of per-familiar requests from a file, or - for stdin.
    #[arg(long)]
    batch: Option<PathBuf>,
}

#[derive(Args)]
struct AttachOptions {
    #[arg(long, required_unless_present = "batch", conflicts_with = "batch", value_parser = parse_id)]
    familiar: Option<String>,
    #[command(flatten)]
    target: AttachTarget,
    /// Read a JSON array of per-familiar requests from a file, or - for stdin.
    #[arg(long)]
    batch: Option<PathBuf>,
}

fn read_batch(path: &std::path::Path) -> Result<Vec<serde_json::Value>> {
    let input: Box<dyn Read> = if path == std::path::Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(
            std::fs::File::open(path).with_context(|| format!("open batch {}", path.display()))?,
        )
    };
    let mut bytes = Vec::new();
    input
        .take(protocol::MAX_LINE as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= protocol::MAX_LINE,
        "batch input exceeds 8 KiB"
    );
    let value = ipc::parse_unique_json(&bytes).context("invalid batch JSON")?;
    serde_json::from_value(value).context("batch must be a JSON array of familiar requests")
}

#[derive(Args)]
#[command(group(ArgGroup::new("target").required(true).args(["next", "activity"])))]
struct FocusOptions {
    /// Cycle through available conversations in stable source/instance/activity order.
    #[arg(long)]
    next: bool,
    #[arg(long, requires = "activity", value_parser = parse_id)]
    source: Option<String>,
    /// Required if more than one producer instance reports this source/activity pair.
    #[arg(long, requires = "activity", value_parser = parse_id)]
    instance: Option<String>,
    #[arg(long, requires = "source", value_parser = parse_id)]
    activity: Option<String>,
    /// Familiar to retarget/cycle; defaults to the bootstrap familiar main.
    #[arg(long, value_parser = parse_id)]
    familiar: Option<String>,
}

fn print_control(path: PathBuf, request: serde_json::Value) -> Result<()> {
    protocol::Request::parse(&serde_json::to_vec(&request)?)?;
    let status = ipc::control(&path, request)?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

#[derive(Args)]
#[command(group(ArgGroup::new("expression").required(true).args(["state", "demo"])))]
struct PublishOptions {
    /// Semantic state, not an animation name.
    #[arg(long, value_enum)]
    state: Option<ActivityState>,
    /// Publish a sequence exercising working, attention, outcomes, cancellation, and idle.
    #[arg(long)]
    demo: bool,
    #[arg(long, default_value = "diagnostic", value_parser = parse_id)]
    source: String,
    #[arg(long, default_value = "task", value_parser = parse_id)]
    activity: String,
    /// Producer lifetime ID; defaults to a unique process/time-based ID.
    #[arg(long, value_parser = parse_id)]
    instance: Option<String>,
    /// Waiting requires user action, rather than an external dependency.
    #[arg(long, requires = "state", conflicts_with = "demo")]
    attention: bool,
    #[arg(long, conflicts_with = "demo")]
    reason: Option<String>,
    /// Short conversation summary supplied by the source (at most 160 UTF-8 bytes).
    #[arg(long, value_parser = parse_summary)]
    summary: Option<String>,
    /// Hold each state for this many seconds; send heartbeats while connected.
    #[arg(long, default_value = "3", value_parser = parse_seconds)]
    duration: Duration,
}

fn parse_id(value: &str) -> Result<String, String> {
    protocol::validate_id(value).map_err(|e| e.to_string())?;
    Ok(value.to_owned())
}

fn parse_summary(value: &str) -> Result<String, String> {
    protocol::validate_summary(value).map_err(|error| error.to_string())?;
    Ok(value.trim().to_owned())
}

fn parse_font_family(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("font family must not be empty".into())
    } else {
        Ok(value.trim().to_owned())
    }
}

fn parse_font_size(value: &str) -> Result<f32, String> {
    let size: f32 = value
        .parse()
        .map_err(|_| "expected a font size".to_owned())?;
    if size.is_finite() && (8.0..=32.0).contains(&size) {
        Ok(size)
    } else {
        Err("font size must be a finite number between 8 and 32".into())
    }
}

fn parse_sprite_scale(value: &str) -> Result<f32, String> {
    let scale: f32 = value
        .parse()
        .map_err(|_| "expected a sprite scale".to_owned())?;
    if scale.is_finite() && (0.25..=2.0).contains(&scale) {
        Ok(scale)
    } else {
        Err("sprite scale must be a finite number between 0.25 and 2".into())
    }
}

fn parse_bubble_duration(value: &str) -> Result<Duration, String> {
    let duration = parse_seconds(value)?;
    if (Duration::from_millis(250)..=Duration::from_secs(30)).contains(&duration) {
        Ok(duration)
    } else {
        Err("bubble duration must be between 0.25 and 30 seconds".into())
    }
}

fn parse_move_speed(value: &str) -> Result<f64, String> {
    let speed: f64 = value
        .parse()
        .map_err(|_| "expected a movement speed".to_owned())?;
    if speed.is_finite() && (40.0..=2000.0).contains(&speed) {
        Ok(speed)
    } else {
        Err("movement speed must be a finite number between 40 and 2000".into())
    }
}

fn socket_path(path: Option<PathBuf>) -> Result<PathBuf> {
    path.map_or_else(ipc::default_socket_path, Ok)
}

fn launch_default(path: PathBuf) -> Result<()> {
    let info = bootstrap::ensure_graphical(&path)?;
    anyhow::ensure!(
        info.unattached_open,
        "existing service does not support unattached familiars; update/restart Numen manually"
    );
    let data = ipc::control(
        &path,
        serde_json::json!({"version":1,"type":"familiar.open","items":[{}],
            "service_instance":info.service_instance}),
    )?;
    menu::validate_opened_ids(&data, 1)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}

fn listen(path: PathBuf, options: ListenOptions) -> Result<()> {
    // One latest-value slot, not a queue of stale diagnostic snapshots. A writer
    // may block on stdout, but neither the service nor the exit deadline does.
    let mailbox = Arc::new(display::Mailbox::default());
    let output_mailbox = mailbox.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let (status_tx, status_rx) = mpsc::sync_channel(1);
    let _writer = std::thread::Builder::new()
        .name("numen-output".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let stdout = io::stdout();
                let mut output = stdout.lock();
                while rx.recv().is_ok() {
                    if let Some(view) = output_mailbox.take() {
                        writeln!(output, "{}", serde_json::to_string(&view)?)?;
                        output.flush()?;
                    }
                }
                Ok(())
            })();
            let _ = status_tx.send(result);
        })?;
    // Intentionally do not join the output thread: an unread stdout pipe must
    // not defeat --exit-after. Process exit terminates any blocked writer.
    let _server = ipc::Server::start(&path, move |view| {
        if mailbox.post(view) {
            let _ = tx.try_send(());
        }
    })?;
    tracing::info!(socket = %path.display(), "Numen headless listener ready");
    let deadline = options.exit_after.map(|duration| Instant::now() + duration);
    loop {
        let wait = deadline.map_or(Duration::from_secs(1), |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(1))
        });
        if wait.is_zero() {
            return Ok(());
        }
        match status_rx.recv_timeout(wait) {
            Ok(result) => {
                result.context("diagnostic output failed")?;
                anyhow::bail!("diagnostic output worker stopped");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("event worker stopped"),
        }
    }
}

fn publish(path: PathBuf, options: PublishOptions) -> Result<()> {
    let steps = if options.demo {
        vec![
            (ActivityState::Working, false, None),
            (ActivityState::Waiting, true, Some("permission_required")),
            (ActivityState::Succeeded, false, None),
            (ActivityState::Working, false, None),
            (ActivityState::Failed, false, None),
            (ActivityState::Cancelled, false, None),
            (ActivityState::Idle, false, None),
        ]
    } else {
        vec![(
            options.state.context("--state or --demo is required")?,
            options.attention,
            options.reason.as_deref(),
        )]
    };
    for (state, attention, reason) in &steps {
        protocol::Activity {
            activity: options.activity.clone(),
            sequence: 1,
            state: *state,
            attention: *attention,
            reason: reason.map(str::to_owned),
            summary: options.summary.clone(),
        }
        .validate()?;
    }
    let instance = options.instance.unwrap_or(format!(
        "cli-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let mut publisher = ipc::Publisher::connect(&path, &options.source, &instance)?;
    let remove_sequence = steps.len() as u64 + 1;
    for (index, (state, attention, reason)) in steps.into_iter().enumerate() {
        if let Some(summary) = options.summary.as_deref() {
            publisher.update_with_summary(
                &options.activity,
                index as u64 + 1,
                state,
                attention,
                reason,
                Some(summary),
            )?;
        } else {
            publisher.update(
                &options.activity,
                index as u64 + 1,
                state,
                attention,
                reason,
            )?;
        }
        tracing::info!(source = %options.source, activity = %options.activity, state = state.label(), attention, "Activity acknowledged");
        let deadline = Instant::now() + options.duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(Duration::from_secs(1)));
            if Instant::now() < deadline {
                publisher.heartbeat()?;
            }
        }
    }
    publisher.remove(&options.activity, remove_sequence)?;
    Ok(())
}

fn parse_scale(value: &str) -> Result<f64, String> {
    let scale: f64 = value.parse().map_err(|_| "expected a number".to_owned())?;
    if scale.is_finite() && (0.5..=4.0).contains(&scale) {
        Ok(scale)
    } else {
        Err("scale must be a finite number between 0.5 and 4".to_owned())
    }
}

fn parse_seconds(value: &str) -> Result<Duration, String> {
    let seconds: f64 = value.parse().map_err(|_| "expected seconds".to_owned())?;
    if seconds.is_finite() && (0.1..=86400.0).contains(&seconds) {
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err("seconds must be a finite number between 0.1 and 86400".to_owned())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "numen=info,wgpu=warn".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let command = match cli.command {
        Some(command) => command,
        None => return launch_default(socket_path(cli.socket)?),
    };
    match command {
        Command::Run(options) => {
            let path = if options.manual || options.demo {
                None
            } else {
                Some(socket_path(cli.socket)?)
            };
            let atlas = SpriteAtlas::load()
                .with_context(|| format!("cannot load {DEFAULT_CHARACTER_NAME} runtime assets"))?;
            window::run(atlas, options, path.as_deref())
        }
        Command::Listen(options) => listen(socket_path(cli.socket)?, options),
        Command::Publish(options) => publish(socket_path(cli.socket)?, options),
        Command::Bubbles(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version": 1, "type": "companion.reveal",
                "familiars": if options.familiar.is_empty() { None } else { Some(options.familiar) },
            }),
        ),
        Command::Familiars(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version": 1, "type": "familiar.list", "offset": options.offset,
            }),
        ),
        Command::Targets(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version": 1, "type": "companion.targets", "offset": options.offset, "familiar": options.familiar,
            }),
        ),
        Command::Presets => {
            println!("{}", serde_json::to_string_pretty(&menu::presets())?);
            Ok(())
        }
        Command::Menu(options) => menu::run(&socket_path(cli.socket)?, options),
        Command::WispAction(options) => wisp_action::run(options, cli.socket.as_deref()),
        Command::ServiceInfo => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version":1,"type":"companion.service",
            }),
        ),
        Command::Doctor { json } => doctor::run(&socket_path(cli.socket)?, json),
        Command::Quit => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version":1,"type":"companion.quit",
            }),
        ),
        Command::Open(options) => {
            let items = if let Some(path) = options.batch {
                read_batch(&path)?
            } else {
                let mut item = serde_json::json!({"id": options.id, "preset": options.preset});
                if let Some(source) = options.target.source {
                    item["source"] = source.into();
                    item["instance"] = serde_json::json!(options.target.instance);
                    item["activity"] = serde_json::json!(options.target.activity);
                }
                vec![item]
            };
            let count = items.len();
            let request = serde_json::json!({"version":1,"type":"familiar.open","items":items});
            protocol::Request::parse(&serde_json::to_vec(&request)?)?;
            let data = ipc::control(&socket_path(cli.socket)?, request)?;
            menu::validate_opened_ids(&data, count)?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        Command::Attach(options) => {
            let items = if let Some(path) = options.batch {
                read_batch(&path)?
            } else {
                vec![
                    serde_json::json!({"id": options.familiar.context("--familiar is required")?,
                    "source": options.target.source.context("--source is required")?,
                    "instance": options.target.instance,
                    "activity": options.target.activity.context("--activity is required")?}),
                ]
            };
            print_control(
                socket_path(cli.socket)?,
                serde_json::json!({"version":1,"type":"familiar.attach","items":items}),
            )
        }
        Command::Close(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version":1,"type":"familiar.close","ids":options.familiar,
            }),
        ),
        Command::GoToMouse(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version":1,"type":"familiar.go-to-mouse","ids":options.familiar,
            }),
        ),
        Command::Settings(options) => {
            let request = options.request()?;
            let ids: Vec<String> = serde_json::from_value(request["ids"].clone())?;
            let path = socket_path(cli.socket)?;
            let data = ipc::control(&path, request)?;
            menu::validate_updated_ids(&data, &ids)?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        Command::Preferences { command } => match command {
            PreferenceCommand::Forget(options) => {
                protocol::validate_ids(&options.familiar)?;
                let request = serde_json::json!({"version":1,"type":"familiar.preferences.forget","ids":options.familiar});
                protocol::Request::parse(&serde_json::to_vec(&request)?)?;
                let data = ipc::control(&socket_path(cli.socket)?, request)?;
                let forgotten: Vec<String> = serde_json::from_value(data["forgotten"].clone())
                    .context("accepted forget omitted familiar IDs")?;
                let expected: std::collections::BTreeSet<_> = options.familiar.iter().collect();
                let returned: std::collections::BTreeSet<_> = forgotten.iter().collect();
                anyhow::ensure!(
                    forgotten.len() == options.familiar.len()
                        && returned.len() == forgotten.len()
                        && returned == expected,
                    "accepted forget returned unexpected familiar IDs"
                );
                println!("{}", serde_json::to_string_pretty(&data)?);
                Ok(())
            }
        },
        Command::FontLicense => {
            print!("{}", typography::FALLBACK_FONT_LICENSE);
            Ok(())
        }
        Command::Status(options) => print_control(
            socket_path(cli.socket)?,
            serde_json::json!({
                "version": 1, "type": "companion.status", "offset": options.offset, "familiar": options.familiar,
            }),
        ),
        Command::Focus(options) => {
            let request = if options.next {
                serde_json::json!({"version": 1, "type": "companion.next", "familiar": options.familiar})
            } else {
                serde_json::json!({"version": 1, "type": "companion.focus",
                    "source": options.source.context("--source is required")?,
                    "instance": options.instance,
                    "activity": options.activity.context("--activity is required")?,
                    "familiar": options.familiar})
            };
            print_control(socket_path(cli.socket)?, request)
        }
        Command::CheckAssets => {
            let atlas = SpriteAtlas::load()
                .with_context(|| format!("cannot load {DEFAULT_CHARACTER_NAME} runtime assets"))?;
            println!(
                "{DEFAULT_CHARACTER_NAME} ({DEFAULT_CHARACTER_ID}): validated embedded RGBA atlas; {CELL_WIDTH} × {CELL_HEIGHT} cells"
            );
            for animation in Animation::ALL {
                let clip = atlas.clip(animation);
                let total: Duration = clip.frame_durations.iter().copied().sum();
                println!(
                    "  {:<14} {:>2} frames, {:>4} ms, {}",
                    animation.label(),
                    clip.frame_durations.len(),
                    total.as_millis(),
                    if clip.looping {
                        "loop"
                    } else {
                        "once → idle"
                    }
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_command_keeps_run_flags_explicit_and_help_side_effect_free() {
        assert!(Cli::try_parse_from(["numen"]).unwrap().command.is_none());
        assert!(
            Cli::try_parse_from(["numen", "--socket", "/private/numen/s"])
                .unwrap()
                .command
                .is_none()
        );
        for args in [
            vec!["numen", "--empty"],
            vec!["numen", "--scale", "2"],
            vec!["numen", "unknown"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        for (flag, kind) in [
            ("--help", clap::error::ErrorKind::DisplayHelp),
            ("--version", clap::error::ErrorKind::DisplayVersion),
        ] {
            assert_eq!(
                Cli::try_parse_from(["numen", flag]).err().unwrap().kind(),
                kind
            );
        }
        assert!(
            Cli::command()
                .render_usage()
                .to_string()
                .contains("[COMMAND]")
        );
    }

    #[test]
    fn no_hotkey_help_names_all_global_chords_and_retains_focused_controls() {
        let help = Cli::command()
            .find_subcommand_mut("run")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("Option+I, Option+M, or Option+C"));
        assert!(help.contains("focused-window controls remain"));
        let Some(Command::Run(options)) = Cli::try_parse_from(["numen", "run", "--no-hotkey"])
            .unwrap()
            .command
        else {
            panic!("expected run");
        };
        assert!(options.no_hotkey);
        assert!(Cli::try_parse_from(["numen", "go-to-mouse"]).is_err());
        assert!(Cli::try_parse_from(["numen", "go-to-mouse", "--all"]).is_err());
        assert!(Cli::try_parse_from(["numen", "go-to-corner"]).is_err());
    }

    #[test]
    fn accepts_defaults_and_diagnostic_options() {
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["numen", "run"]).unwrap();
        let Some(Command::Run(options)) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(options.animation, Animation::Idle);
        assert_eq!(options.scale, 1.5);
        assert_eq!(options.font_size, 12.0);
        assert_eq!(options.sprite_scale, 0.5);
        assert_eq!(options.move_speed, 360.0);
        assert_eq!(options.bubble_duration, Duration::from_secs(5));
        assert!(!options.always_bubble);
        assert!(!options.empty);
        assert!(!options.focus);
        assert!(Cli::try_parse_from(["numen", "check-assets"]).is_ok());
        assert!(
            Cli::try_parse_from(["numen", "run", "--demo", "--opaque", "--exit-after", "0.5"])
                .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_cli_values() {
        for value in ["NaN", "inf", "0", "0.49", "4.1", "huge"] {
            assert!(Cli::try_parse_from(["numen", "run", "--scale", value]).is_err());
        }
        for value in ["NaN", "inf", "0", "-1", "86401"] {
            assert!(Cli::try_parse_from(["numen", "run", "--exit-after", value]).is_err());
        }
        assert!(Cli::try_parse_from(["numen", "run", "--demo", "--paused"]).is_err());
        assert!(Cli::try_parse_from(["numen", "run", "--animation", "unknown"]).is_err());
        for size in ["0", "7.9", "32.1", "NaN", "inf"] {
            assert!(Cli::try_parse_from(["numen", "run", "--font-size", size]).is_err());
        }
        for scale in ["0", "0.24", "2.1", "NaN", "inf"] {
            assert!(Cli::try_parse_from(["numen", "run", "--sprite-scale", scale]).is_err());
        }
        for speed in ["0", "39", "2001", "NaN", "inf"] {
            assert!(Cli::try_parse_from(["numen", "run", "--move-speed", speed]).is_err());
        }
        assert!(Cli::try_parse_from(["numen", "run", "--sprite-scale", "1"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "run", "--font-family", " "]).is_err());
        assert!(
            Cli::try_parse_from([
                "numen",
                "run",
                "--font-family",
                "SauceCodePro Nerd Font",
                "--font-file",
                "font.ttf"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "numen",
                "run",
                "--font-size",
                "18",
                "--font-family",
                "SauceCodePro Nerd Font"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["numen", "font-license"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "bubbles"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "menu", "--no-start"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "wisp-action"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "quit"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "run", "--empty"]).is_ok());
        assert!(Cli::try_parse_from(["numen", "run", "--empty", "--manual"]).is_err());
        assert!(Cli::try_parse_from(["numen", "run", "--empty", "--demo"]).is_err());
        assert!(Cli::try_parse_from(["numen", "run", "--no-bubble", "--always-bubble"]).is_err());
        for duration in ["0", "0.2", "31", "NaN", "inf"] {
            assert!(Cli::try_parse_from(["numen", "run", "--bubble-duration", duration]).is_err());
        }
    }

    #[test]
    fn focus_requires_an_unambiguous_target_or_next() {
        for args in [
            vec!["numen", "focus", "--next"],
            vec![
                "numen",
                "focus",
                "--source",
                "djinn",
                "--activity",
                "chat-1",
            ],
            vec![
                "numen",
                "focus",
                "--source",
                "djinn",
                "--instance",
                "one",
                "--activity",
                "chat-1",
            ],
            vec!["numen", "status", "--offset", "8"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        for args in [
            vec!["numen", "focus"],
            vec!["numen", "focus", "--source", "djinn"],
            vec!["numen", "focus", "--activity", "chat-1"],
            vec![
                "numen",
                "focus",
                "--next",
                "--source",
                "djinn",
                "--activity",
                "chat-1",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn familiar_controls_require_explicit_distinct_target_requests() {
        for args in [
            vec!["numen", "presets"],
            vec!["numen", "open"],
            vec!["numen", "familiars", "--offset", "4"],
            vec!["numen", "targets", "--familiar", "main"],
            vec![
                "numen",
                "open",
                "--source",
                "djinn",
                "--activity",
                "chat-2",
                "--id",
                "second",
            ],
            vec!["numen", "open", "--batch", "-"],
            vec![
                "numen",
                "attach",
                "--familiar",
                "main",
                "--source",
                "djinn",
                "--activity",
                "chat-3",
            ],
            vec!["numen", "attach", "--batch", "-"],
            vec!["numen", "close", "--familiar", "main", "second"],
            vec!["numen", "go-to-mouse", "--familiar", "main", "second"],
            vec!["numen", "bubbles", "--familiar", "second"],
            vec!["numen", "status", "--familiar", "second"],
            vec!["numen", "focus", "--next", "--familiar", "second"],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }
        for args in [
            vec!["numen", "open", "--source", "djinn"],
            vec!["numen", "open", "--activity", "chat-1"],
            vec!["numen", "open", "--instance", "one"],
            vec!["numen", "attach", "--familiar", "main"],
            vec!["numen", "open", "--batch", "-", "--source", "djinn"],
            vec![
                "numen",
                "attach",
                "--source",
                "djinn",
                "--activity",
                "chat-2",
            ],
            vec!["numen", "attach", "--batch", "-", "--familiar", "main"],
            vec!["numen", "close"],
            vec!["numen", "go-to-mouse"],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_err(), "{args:?}");
        }
    }

    #[test]
    fn settings_accepts_every_nonempty_patch_subset_and_sixteen_ids() {
        for mask in 1..8 {
            let mut args = vec!["numen".to_owned(), "settings".into(), "--familiar".into()];
            args.extend((0..16).map(|n| format!("f{n}")));
            for (bit, flag, value) in [
                (1, "--sprite-scale", "0.5"),
                (2, "--font-size", "16"),
                (4, "--bubble-duration", "5"),
            ] {
                if mask & bit != 0 {
                    args.extend([flag.into(), value.into()]);
                }
            }
            let Some(Command::Settings(options)) = Cli::try_parse_from(args).unwrap().command
            else {
                panic!("expected settings");
            };
            let request = options.request().unwrap();
            assert_eq!(request["ids"].as_array().unwrap().len(), 16);
            let patch = request["patch"].as_object().unwrap();
            assert_eq!(patch.contains_key("sprite_scale"), mask & 1 != 0);
            assert_eq!(patch.contains_key("font_size"), mask & 2 != 0);
            assert_eq!(patch.contains_key("bubble_duration"), mask & 4 != 0);
        }
    }

    #[test]
    fn batch_file_input_is_bounded_and_requires_an_array() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(br#"[{"preset":"djinn","source":"djinn","activity":"chat-1"}]"#)
            .unwrap();
        let items = read_batch(file.path()).unwrap();
        assert_eq!(items.len(), 1);
        let mut invalid = tempfile::NamedTempFile::new().unwrap();
        invalid.write_all(b"{}").unwrap();
        assert!(read_batch(invalid.path()).is_err());
        let mut oversized = tempfile::NamedTempFile::new().unwrap();
        oversized
            .write_all(&vec![b' '; protocol::MAX_LINE + 1])
            .unwrap();
        assert!(read_batch(oversized.path()).is_err());
    }

    use clap::CommandFactory;
}
