use std::{io, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{build_info, diagnostics, ipc, protocol};

#[derive(Serialize)]
pub struct Report {
    pub client: Client,
    pub socket: String,
    pub service: Option<Service>,
    pub service_state: ServiceState,
    /// Compares recorded fingerprints, not native executable bytes or build age.
    pub build_comparison: build_info::Comparison,
}

#[derive(Serialize)]
pub struct Client {
    pub executable: String,
    pub build: build_info::BuildInfo,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Reachable,
    NotRunning,
    Legacy,
    UnsupportedDiagnostics,
}

#[derive(Deserialize, Serialize)]
pub struct Service {
    pub api_version: u32,
    pub pid: u32,
    pub service_instance: String,
    pub graphical: bool,
    pub lifecycle: bool,
    pub navigation: bool,
    pub settings: Option<bool>,
    pub familiar_count: Option<u64>,
    pub source_count: Option<u64>,
    pub diagnostics: Option<diagnostics::Snapshot>,
}

// Never echo rejected peer strings, including serde's enum error text.
fn safe_text(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && !value.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

fn validate_build(build: &build_info::BuildInfo) -> Result<()> {
    ensure!(
        [&build.version, &build.target, &build.profile]
            .into_iter()
            .all(|s| safe_text(s, 256)
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-_+".contains(&c))),
        "invalid service build metadata"
    );
    for (fingerprint, scheme) in [
        (&build.fingerprint, "numen-build-v1-fnv1a128:"),
        (&build.source_fingerprint, "numen-inputs-v1-fnv1a128:"),
    ] {
        ensure!(
            fingerprint == "unknown"
                || fingerprint.strip_prefix(scheme).is_some_and(|hex| {
                    hex.len() == 32
                        && hex
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                }),
            "invalid service build fingerprint"
        );
    }
    Ok(())
}

fn parse_service(mut data: Value) -> Result<(Service, ServiceState)> {
    let root = data.as_object_mut().context("invalid service metadata")?;
    let raw = root.remove("diagnostics");
    let mut service: Service =
        serde_json::from_value(data).map_err(|_| anyhow::anyhow!("invalid service metadata"))?;
    ensure!(service.api_version == 1, "unsupported service API version");
    ensure!(service.pid != 0, "invalid service PID");
    protocol::validate_id(&service.service_instance)
        .map_err(|_| anyhow::anyhow!("invalid service instance"))?;
    let Some(raw) = raw else {
        return Ok((service, ServiceState::Legacy));
    };
    let version = raw
        .get("version")
        .and_then(Value::as_u64)
        .context("invalid diagnostics version")?;
    if version != 1 {
        // Future schemas must not be interpreted using today's field meanings.
        return Ok((service, ServiceState::UnsupportedDiagnostics));
    }
    let snapshot: diagnostics::Snapshot = serde_json::from_value(raw)
        .map_err(|_| anyhow::anyhow!("invalid service diagnostics metadata"))?;
    if let Some(path) = &snapshot.executable {
        ensure!(safe_text(path, 1024), "invalid service executable metadata");
    }
    validate_build(&snapshot.build)?;
    if let Some(preferences) = &snapshot.preferences {
        ensure!(
            preferences.valid(),
            "invalid preferences diagnostics metadata"
        );
        if let Some(path) = &preferences.path {
            ensure!(safe_text(path, 1024), "invalid preferences path metadata");
        }
    }
    ensure!(
        snapshot
            .hotkeys
            .option_i
            .valid_for(crate::hotkey::Shortcut::Reveal)
            && snapshot
                .hotkeys
                .option_m
                .valid_for(crate::hotkey::Shortcut::GoToMouse),
        "invalid service shortcut metadata"
    );
    ensure!(
        service.settings.is_some()
            && service.familiar_count.is_some()
            && service.source_count.is_some(),
        "incomplete service diagnostics metadata"
    );
    service.diagnostics = Some(snapshot);
    Ok((service, ServiceState::Reachable))
}

pub fn report(path: &Path) -> Result<Report> {
    let executable = std::env::current_exe().context("resolve client executable")?;
    let client = Client {
        executable: executable.to_string_lossy().into_owned(),
        build: build_info::current(),
    };
    let (service, service_state) =
        match ipc::control(path, json!({"version":1,"type":"companion.service"})) {
            Ok(data) => {
                let (service, state) = parse_service(data)?;
                (Some(service), state)
            }
            Err(error)
                if error.downcast_ref::<io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    )
                }) =>
            {
                (None, ServiceState::NotRunning)
            }
            Err(_) => anyhow::bail!(
                "could not read service metadata (private socket, protocol, or timeout failure)"
            ),
        };
    let build_comparison = build_info::compare(
        service
            .as_ref()
            .and_then(|s| s.diagnostics.as_ref())
            .map(|s| &s.build),
    );
    Ok(Report {
        client,
        socket: path.to_string_lossy().into_owned(),
        service,
        service_state,
        build_comparison,
    })
}

fn label(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .expect("diagnostic enum serializes")
        .as_str()
        .expect("diagnostic enum is a string")
        .to_owned()
}

pub fn run(path: &Path, json: bool) -> Result<()> {
    let report = report(path)?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    println!("Client: {}", report.client.executable.escape_debug());
    println!("  Version: {}", report.client.build.version);
    println!(
        "  Recorded build fingerprint: {}",
        report.client.build.fingerprint
    );
    println!(
        "  Recorded input fingerprint: {}",
        report.client.build.source_fingerprint
    );
    println!("Socket: {}", report.socket.escape_debug());
    println!("Service: {}", label(&report.service_state));
    if let Some(service) = report.service {
        println!("  PID: {}", service.pid);
        if let Some(snapshot) = service.diagnostics {
            println!(
                "  Path: {}",
                snapshot.executable.as_deref().unwrap_or("unknown")
            );
            println!("  Version: {}", snapshot.build.version);
            println!(
                "  Recorded build fingerprint: {}",
                snapshot.build.fingerprint
            );
            println!(
                "  Recorded input fingerprint: {}",
                snapshot.build.source_fingerprint
            );
            for (chord, status) in [
                ("Option+I", snapshot.hotkeys.option_i),
                ("Option+M", snapshot.hotkeys.option_m),
            ] {
                println!(
                    "  {chord}: {} (reason: {})",
                    label(&status.state),
                    status
                        .reason
                        .as_ref()
                        .map(label)
                        .unwrap_or_else(|| "none".into())
                );
            }
            if let Some(preferences) = snapshot.preferences {
                println!(
                    "  Preferences: {} (load: {}, write: {}, reason: {})",
                    label(&preferences.mode),
                    label(&preferences.load),
                    label(&preferences.write),
                    preferences
                        .reason
                        .as_ref()
                        .map(label)
                        .unwrap_or_else(|| "none".into())
                );
                println!(
                    "    Revision: accepted {}, durable {}; remembered chats: {}",
                    preferences.accepted_revision,
                    preferences.durable_revision,
                    preferences.records
                );
                if let Some(path) = preferences.path {
                    println!("    Path: {}", path.escape_debug());
                }
            } else {
                println!("  Preferences: unknown (reason: not_reported)");
            }
        } else {
            println!("  Path/build: unknown");
            println!("  Option+I: unknown (reason: not_reported)");
            println!("  Option+M: unknown (reason: not_reported)");
            println!("  Preferences: unknown (reason: not_reported)");
        }
        println!("  Navigation: {}", service.navigation);
        println!(
            "  Settings: {}",
            service
                .settings
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
        println!(
            "  Familiars: {}",
            service
                .familiar_count
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
        println!(
            "  Sources: {}",
            service
                .source_count
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    } else {
        println!("  PID/path/build: unknown");
        println!("  Option+I: unknown (reason: not_reported)");
        println!("  Option+M: unknown (reason: not_reported)");
        println!("  Preferences: unknown (reason: not_reported)");
        println!("  Navigation/settings: unknown");
        println!("  Familiars/sources: unknown");
    }
    println!(
        "Comparison: {} (recorded build/input fingerprints)",
        label(&report.build_comparison)
    );
    println!("  Match does not guarantee identical executables; different is not proof of age.");
    println!("  Missing, legacy, or unsupported diagnostics leave comparison unknown.");
    Ok(())
}
