//! One-command terminal menu. Discovery is read-only; target choices commit as one batch.
use std::{
    collections::HashSet,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_FAMILIARS: usize = 16;
const MAX_TARGETS: usize = 512;
const MAX_OUTPUT: usize = 64 * 1024;

#[derive(clap::Args)]
pub struct MenuOptions {
    #[arg(long)]
    pub no_start: bool,
    #[arg(long, default_value = "fzf")]
    pub fzf: PathBuf,
}

/// The offline preset catalog is also used by the CLI's `presets` command.
pub fn presets() -> Value {
    json!({"presets": [{"id": "djinn", "name": "Djinn", "capabilities": ["open"]}]})
}

pub fn run(path: &Path, options: MenuOptions) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "Numen menu needs an interactive TTY; use Wisp run/summon, not exec"
    );
    let mut backend = NativeBackend {
        path,
        pinned_instance: None,
    };
    let mut picker = Fzf {
        executable: options.fzf,
    };
    if let Some(message) = menu(&mut backend, &mut picker, options.no_start)? {
        println!("{message}");
    }
    Ok(())
}

enum Service {
    Offline,
    Graphical,
    Unsupported,
}

trait Backend {
    fn service(&mut self) -> Result<Service>;
    fn ensure_graphical(&mut self) -> Result<()>;
    fn control(&mut self, request: Value) -> Result<Value>;
}

struct NativeBackend<'a> {
    path: &'a Path,
    pinned_instance: Option<String>,
}

impl NativeBackend<'_> {
    fn pin(&mut self, info: &crate::bootstrap::ServiceInfo) -> Result<()> {
        if let Some(instance) = &self.pinned_instance {
            ensure!(
                instance == &info.service_instance,
                "service instance changed; reopen menu"
            );
        } else {
            self.pinned_instance = Some(info.service_instance.clone());
        }
        Ok(())
    }
}

impl Backend for NativeBackend<'_> {
    fn service(&mut self) -> Result<Service> {
        Ok(match crate::bootstrap::probe(self.path)? {
            None => {
                ensure!(
                    self.pinned_instance.is_none(),
                    "service disappeared; reopen menu before starting a new service"
                );
                Service::Offline
            }
            Some(info) => {
                self.pin(&info)?;
                if info.api_version == 1 && info.graphical && info.lifecycle {
                    Service::Graphical
                } else {
                    Service::Unsupported
                }
            }
        })
    }

    fn ensure_graphical(&mut self) -> Result<()> {
        // Once pinned, never let bootstrap spawn a replacement for cached identities.
        if self.pinned_instance.is_some() {
            ensure!(
                matches!(self.service()?, Service::Graphical),
                "existing service unsupported; reopen menu"
            );
            return Ok(());
        }
        let info = crate::bootstrap::ensure_graphical(self.path)?;
        self.pin(&info)
    }

    fn control(&mut self, mut request: Value) -> Result<Value> {
        let instance = self
            .pinned_instance
            .as_ref()
            .context("service identity unavailable; reopen menu")?;
        request["service_instance"] = json!(instance.clone());
        crate::ipc::control(self.path, request)
    }
}

#[derive(Clone, Debug)]
struct Row {
    key: String,
    label: String,
}

enum Pick {
    Cancel,
    Refresh,
    Selected(Vec<String>),
}

trait Picker {
    fn pick(
        &mut self,
        rows: &[Row],
        multi: bool,
        selected: &[String],
        header: &str,
    ) -> Result<Pick>;
}

struct Fzf {
    executable: PathBuf,
}

impl Picker for Fzf {
    fn pick(
        &mut self,
        rows: &[Row],
        multi: bool,
        selected: &[String],
        header: &str,
    ) -> Result<Pick> {
        let mut command = Command::new(&self.executable);
        command
            .env_remove("FZF_DEFAULT_OPTS")
            .env_remove("FZF_DEFAULT_OPTS_FILE")
            .env_remove("FZF_DEFAULT_COMMAND")
            .args([
                "--delimiter",
                "\t",
                "--with-nth",
                "2..",
                "--height",
                "100%",
                "--border",
                "--sync",
            ])
            .args([
                "--prompt",
                "Numen > ",
                "--expect=ctrl-r",
                "--header",
                &clean(header),
            ])
            .arg(if multi { "--multi=16" } else { "--no-multi" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Only numeric positions enter this binding, never server text or shell actions.
        let bindings: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| multi && selected.contains(&row.key))
            .map(|(index, _)| format!("pos({})+select", index + 1))
            .collect();
        if !bindings.is_empty() {
            command.args(["--bind", &format!("start:{}+first", bindings.join("+"))]);
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "start fzf selector {}; install fzf or use --fzf PATH",
                self.executable.display()
            )
        })?;
        let mut stdin = child.stdin.take().context("fzf stdin unavailable")?;
        let input = rows
            .iter()
            .map(|row| format!("{}\t{}\n", row.key, clean(&row.label)))
            .collect::<String>();
        // Feed and drain concurrently so a full pipe cannot deadlock the selector.
        let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut output = Vec::new();
        let read = child
            .stdout
            .take()
            .context("fzf stdout unavailable")?
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut output);
        if read.is_err() || output.len() > MAX_OUTPUT {
            let _ = child.kill();
        }
        let status = child.wait().context("wait for fzf")?;
        let written = writer
            .join()
            .map_err(|_| anyhow::anyhow!("fzf input writer failed"))?;
        read.context("read fzf selection")?;
        ensure!(output.len() <= MAX_OUTPUT, "fzf output exceeds 64 KiB");
        match status.code() {
            Some(1 | 130) => return Ok(Pick::Cancel),
            Some(0) => {}
            _ => bail!("fzf failed ({status}); check the terminal and --fzf executable"),
        }
        written.context("provide rows to fzf")?;
        decode_selection(&output, rows, multi)
    }
}

fn decode_selection(output: &[u8], rows: &[Row], multi: bool) -> Result<Pick> {
    ensure!(output.len() <= MAX_OUTPUT, "fzf output exceeds 64 KiB");
    let text = std::str::from_utf8(output).context("fzf output is not UTF-8")?;
    let mut lines = text.lines();
    let key = lines.next().context("fzf omitted the expected key line")?;
    ensure!(key.is_empty() || key == "ctrl-r", "unexpected fzf key");
    let keys: Vec<_> = lines
        .map(|line| line.split('\t').next().unwrap_or("").to_owned())
        .collect();
    validate_selection(&keys, rows, multi, key == "ctrl-r")?;
    if key == "ctrl-r" {
        Ok(Pick::Refresh)
    } else {
        Ok(Pick::Selected(keys))
    }
}

fn validate_selection(keys: &[String], rows: &[Row], multi: bool, allow_empty: bool) -> Result<()> {
    ensure!(
        (allow_empty || !keys.is_empty()) && keys.len() <= if multi { MAX_FAMILIARS } else { 1 },
        "invalid selector selection count"
    );
    let mut seen = HashSet::new();
    for key in keys {
        ensure!(
            rows.iter().any(|row| row.key == *key),
            "selector returned an unoffered identity"
        );
        ensure!(seen.insert(key), "selector returned duplicate identities");
    }
    Ok(())
}

fn pick(
    picker: &mut impl Picker,
    rows: &[Row],
    multi: bool,
    selected: &[String],
    header: &str,
) -> Result<Pick> {
    let result = picker.pick(rows, multi, selected, &clean(header))?;
    if let Pick::Selected(keys) = &result {
        validate_selection(keys, rows, multi, false)?;
    }
    Ok(result)
}

/// Strip terminal escape sequences, control/bidi characters and line boundaries.
fn clean(text: &str) -> String {
    let mut plain = String::new();
    let mut escape = 0;
    for c in text.chars().take(8192) {
        match escape {
            1 => {
                escape = match c {
                    '[' => 2,
                    ']' | 'P' | '^' | '_' => 3,
                    _ => 0,
                }
            }
            2 => {
                if ('@'..='~').contains(&c) {
                    escape = 0;
                }
            }
            3 => {
                if c == '\u{7}' {
                    escape = 0;
                } else if c == '\u{1b}' {
                    escape = 4;
                }
            }
            4 => escape = if c == '\\' { 0 } else { 3 },
            _ if c == '\u{1b}' => escape = 1,
            _ if c.is_whitespace()
                || c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                plain.push(' ')
            }
            _ => plain.push(c),
        }
    }
    plain
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(800)
        .collect()
}

fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 96
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        "invalid protocol identifier"
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
struct Binding {
    source: String,
    instance: String,
    activity: String,
}

impl Binding {
    fn validate(&self) -> Result<()> {
        valid_id(&self.source)?;
        valid_id(&self.instance)?;
        valid_id(&self.activity)
    }

    fn context(&self) -> String {
        format!("{}/{}/{}", self.source, self.instance, self.activity)
    }

    fn item(&self) -> Value {
        json!({"source": self.source, "instance": self.instance, "activity": self.activity})
    }
}

#[derive(Clone, Deserialize)]
struct Familiar {
    id: String,
    preset: String,
    attachment: Option<Binding>,
    available: bool,
    summary: Option<String>,
    capabilities: Vec<String>,
    #[serde(default)]
    settings: Option<crate::protocol::FamiliarSettings>,
}

#[derive(Deserialize)]
struct Target {
    #[serde(flatten)]
    binding: Binding,
    state: String,
    attention: bool,
    reason: Option<String>,
    summary: Option<String>,
    claimed_by: Option<String>,
    eligible: bool,
}

#[derive(Deserialize)]
struct Page<T> {
    #[serde(alias = "familiars", alias = "targets")]
    rows: Vec<T>,
    offset: usize,
    total: usize,
    next_offset: Option<usize>,
}

fn pages<T: serde::de::DeserializeOwned>(
    backend: &mut impl Backend,
    kind: &str,
    familiar: Option<&str>,
    limit: usize,
) -> Result<Vec<T>> {
    let mut offset = 0;
    let mut all = Vec::new();
    let mut total = None;
    loop {
        let mut request = json!({"version": 1, "type": kind, "offset": offset});
        if let Some(id) = familiar {
            request["familiar"] = json!(id);
        }
        let page: Page<T> =
            serde_json::from_value(backend.control(request)?).context("invalid discovery page")?;
        ensure!(
            page.offset == offset && page.total <= limit && page.rows.len() <= 4,
            "invalid discovery page bounds"
        );
        ensure!(
            total.is_none_or(|n| n == page.total),
            "discovery changed during pagination; refresh to retry"
        );
        total = Some(page.total);
        let end = offset + page.rows.len();
        ensure!(
            end <= page.total && all.len() + page.rows.len() <= limit,
            "invalid discovery count"
        );
        all.extend(page.rows);
        match page.next_offset {
            Some(next) => {
                ensure!(
                    next == end && next > offset && next < page.total,
                    "discovery pagination did not progress"
                );
                offset = next;
            }
            None => {
                ensure!(end == page.total, "discovery page ended before total");
                return Ok(all);
            }
        }
    }
}

#[derive(Clone)]
struct Choice {
    row: Row,
    id: String,
    preset: bool,
    capabilities: Vec<String>,
    settings: Option<crate::protocol::FamiliarSettings>,
}

fn catalog(backend: &mut impl Backend) -> Result<(Vec<Choice>, String)> {
    let mut choices = Vec::new();
    let note = match backend.service()? {
        Service::Offline => "Service offline; Open can start an empty graphical service.",
        Service::Unsupported => {
            "Existing service unsupported (headless/outdated); stop it and run a current graphical service. It will not be replaced."
        }
        Service::Graphical => {
            let familiars: Vec<Familiar> = pages(backend, "familiar.list", None, MAX_FAMILIARS)?;
            let mut ids = HashSet::new();
            for familiar in familiars {
                valid_id(&familiar.id)?;
                valid_id(&familiar.preset)?;
                ensure!(
                    ids.insert(familiar.id.clone()),
                    "duplicate familiar in discovery"
                );
                if let Some(binding) = &familiar.attachment {
                    binding.validate()?;
                }
                let name = preset_name(&familiar.preset);
                let label = format!(
                    "{} | {} [{}] | {} | {}",
                    if familiar.available {
                        "Running"
                    } else {
                        "Waiting"
                    },
                    name,
                    familiar.id,
                    familiar.summary.as_deref().unwrap_or("No summary"),
                    familiar
                        .attachment
                        .as_ref()
                        .map(Binding::context)
                        .unwrap_or_else(|| "Unattached".into())
                );
                choices.push(Choice {
                    row: Row {
                        key: format!("familiar:{}", familiar.id),
                        label: clean(&label),
                    },
                    id: familiar.id,
                    preset: false,
                    capabilities: familiar.capabilities,
                    settings: familiar.settings,
                });
            }
            "Select one or more familiars or presets. Ctrl-R refreshes. Esc exits."
        }
    };
    for preset in presets()["presets"]
        .as_array()
        .context("invalid built-in preset catalog")?
    {
        let id = preset["id"]
            .as_str()
            .context("preset id missing")?
            .to_owned();
        valid_id(&id)?;
        choices.push(Choice {
            row: Row {
                key: format!("preset:{id}"),
                label: format!(
                    "Preset | {} [{}] | Open a new familiar",
                    preset["name"].as_str().unwrap_or(&id),
                    id
                ),
            },
            id,
            preset: true,
            capabilities: serde_json::from_value(preset["capabilities"].clone())?,
            settings: None,
        });
    }
    Ok((choices, note.into()))
}

fn preset_name(id: &str) -> String {
    presets()["presets"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .and_then(|row| row["name"].as_str())
        .unwrap_or(id)
        .to_owned()
}

const ACTIONS: [(&str, &str); 6] = [
    ("open", "Open"),
    ("info", "Info bubble"),
    ("go-to-mouse", "Go to mouse"),
    ("attach", "Attach to process"),
    ("settings", "Settings"),
    ("close", "Close"),
];

fn commands(selection: &[Choice]) -> Vec<Row> {
    let mut rows: Vec<_> = ACTIONS
        .iter()
        .filter(|(action, _)| {
            !selection.is_empty()
                && selection.iter().all(|choice| {
                    // Presets can only Open; active identities can never Open.
                    (choice.preset == (*action == "open"))
                        && choice.capabilities.iter().any(|cap| cap == action)
                })
        })
        .map(|(key, label)| Row {
            key: (*key).into(),
            label: (*label).into(),
        })
        .collect();
    rows.push(back());
    rows
}

fn back() -> Row {
    Row {
        key: "back".into(),
        label: "Back".into(),
    }
}
fn refresh() -> Row {
    Row {
        key: "refresh".into(),
        label: "Refresh".into(),
    }
}

fn menu(
    backend: &mut impl Backend,
    picker: &mut impl Picker,
    no_start: bool,
) -> Result<Option<String>> {
    let mut retained = Vec::new();
    let mut notice = String::new();
    'families: loop {
        let (choices, note) = match catalog(backend) {
            Ok(catalog) => catalog,
            Err(error) => {
                // A failed probe is not permission to start/replace a service.
                let rows = [refresh(), back()];
                match pick(
                    picker,
                    &rows,
                    false,
                    &[],
                    &format!("Discovery failed: {error:#}. Refresh or Back to exit."),
                )? {
                    Pick::Cancel => return Ok(None),
                    Pick::Selected(keys) if keys[0] == "back" => return Ok(None),
                    _ => continue,
                }
            }
        };
        let rows: Vec<_> = choices.iter().map(|choice| choice.row.clone()).collect();
        let keys = match pick(picker, &rows, true, &retained, &format!("{notice} {note}"))? {
            Pick::Cancel => return Ok(None),
            Pick::Refresh => continue,
            Pick::Selected(keys) => keys,
        };
        retained = keys.clone();
        let mut selection: Vec<_> = keys
            .iter()
            .map(|key| {
                choices
                    .iter()
                    .find(|choice| choice.row.key == *key)
                    .unwrap()
                    .clone()
            })
            .collect();
        notice.clear();
        let mut header = String::new();
        loop {
            let rows = commands(&selection);
            let context = if rows.len() == 1 {
                "No command applies to the entire selection. Mixed presets and running familiars cannot share Open/Attach; choose Back."
            } else {
                "Choose one command for the entire selection. Esc returns. Ctrl-R refreshes capabilities."
            };
            let action = match pick(picker, &rows, false, &[], &format!("{header} {context}"))? {
                Pick::Cancel => continue 'families,
                Pick::Refresh => {
                    match catalog(backend) {
                        Ok((fresh, _)) => {
                            let updated: Vec<_> = keys
                                .iter()
                                .filter_map(|key| {
                                    fresh.iter().find(|choice| choice.row.key == *key).cloned()
                                })
                                .collect();
                            if updated.len() != keys.len() {
                                notice = "Selection changed or disappeared; select again.".into();
                                continue 'families;
                            }
                            selection = updated;
                            header.clear();
                        }
                        Err(error) => header = format!("Refresh failed: {error:#}"),
                    }
                    continue;
                }
                Pick::Selected(keys) => keys[0].clone(),
            };
            if action == "back" {
                continue 'families;
            }
            match execute(backend, picker, &selection, &action, no_start) {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => header = "Selection cancelled; no batch was sent.".into(),
                Err(error) => {
                    header = format!("Command failed: {error:#}. Retry, Ctrl-R refresh, or Back.")
                }
            }
        }
    }
}

fn execute(
    backend: &mut impl Backend,
    picker: &mut impl Picker,
    selection: &[Choice],
    action: &str,
    no_start: bool,
) -> Result<Option<String>> {
    if action == "settings" {
        return settings(backend, picker, selection);
    }
    let ids: Vec<_> = selection.iter().map(|choice| choice.id.clone()).collect();
    let request = if action == "open" || action == "attach" {
        if action == "open" {
            match backend.service()? {
                Service::Graphical => {}
                Service::Offline if no_start => bail!(
                    "Open requires a graphical lifecycle service; --no-start prevents starting one. Run numen run --empty first"
                ),
                Service::Offline => backend.ensure_graphical()?,
                Service::Unsupported => bail!(
                    "Existing service is headless or lacks current graphical lifecycle support; stop it manually and run an updated graphical service. It will not be replaced"
                ),
            }
        }
        let Some(items) = target_items(backend, picker, selection, action)? else {
            return Ok(None);
        };
        json!({"version": 1, "type": format!("familiar.{action}"), "items": items})
    } else {
        let (kind, field) = match action {
            "info" => ("companion.reveal", "familiars"),
            "go-to-mouse" => ("familiar.go-to-mouse", "ids"),
            "close" => ("familiar.close", "ids"),
            _ => bail!("unknown menu command"),
        };
        let mut request = json!({"version": 1, "type": kind});
        request[field] = json!(ids);
        request
    };
    let data = backend.control(request)?;
    let result_field = match action {
        "open" => "opened",
        "attach" => "attached",
        "close" => "closed",
        "go-to-mouse" => "moving",
        _ => "",
    };
    let result_ids = if result_field.is_empty() {
        ids
    } else {
        let returned: Vec<String> = serde_json::from_value(
            data.get(result_field)
                .cloned()
                .context("accepted command omitted result IDs")?,
        )?;
        ensure!(
            returned.len() == selection.len(),
            "accepted command returned unexpected ID count"
        );
        let mut seen = HashSet::new();
        for id in &returned {
            valid_id(id)?;
            ensure!(seen.insert(id), "accepted command returned duplicate IDs");
        }
        returned
    };
    Ok(Some(format!(
        "Accepted {action}: {}",
        result_ids.join(", ")
    )))
}

pub(crate) fn validate_updated_ids(data: &Value, ids: &[String]) -> Result<()> {
    let updated: Vec<String> = serde_json::from_value(
        data.get("updated")
            .cloned()
            .context("accepted settings omitted updated IDs")?,
    )?;
    let expected: HashSet<_> = ids.iter().collect();
    let returned: HashSet<_> = updated.iter().collect();
    ensure!(
        updated.len() == ids.len() && returned.len() == updated.len() && returned == expected,
        "accepted settings returned unexpected IDs"
    );
    Ok(())
}

pub(crate) type SettingDefinition = (&'static str, &'static str, [(&'static str, f64); 4]);

pub(crate) const SETTINGS: [SettingDefinition; 3] = [
    (
        "sprite_scale",
        "Sprite size",
        [
            ("Small", 0.25),
            ("Default", 0.5),
            ("Large", 1.0),
            ("XL", 2.0),
        ],
    ),
    (
        "font_size",
        "Text size",
        [
            ("Small", 10.0),
            ("Default", 12.0),
            ("Large", 16.0),
            ("XL", 20.0),
        ],
    ),
    (
        "bubble_duration",
        "Bubble duration (seconds)",
        [
            ("Short", 2.0),
            ("Default", 5.0),
            ("Long", 10.0),
            ("XL", 30.0),
        ],
    ),
];

fn setting_value(choice: &Choice, field: &str) -> Option<f64> {
    choice.settings.as_ref().map(|settings| match field {
        "sprite_scale" => settings.sprite_scale,
        "font_size" => settings.font_size,
        "bubble_duration" => settings.bubble_duration,
        _ => unreachable!("trusted setting field"),
    })
}

fn settings_selection(backend: &mut impl Backend, selection: &[Choice]) -> Result<Vec<Choice>> {
    let (fresh, _) = catalog(backend)?;
    selection
        .iter()
        .map(|old| {
            let choice = fresh
                .iter()
                .find(|choice| choice.row.key == old.row.key)
                .context("selected familiar disappeared; refresh commands")?;
            ensure!(
                !choice.preset && choice.capabilities.iter().any(|cap| cap == "settings"),
                "selected familiar no longer supports settings; refresh commands"
            );
            Ok(choice.clone())
        })
        .collect()
}

fn settings(
    backend: &mut impl Backend,
    picker: &mut impl Picker,
    selection: &[Choice],
) -> Result<Option<String>> {
    ensure!(
        commands(selection).iter().any(|row| row.key == "settings"),
        "settings unavailable for selection"
    );
    let ids: Vec<_> = selection.iter().map(|choice| choice.id.clone()).collect();
    crate::protocol::validate_ids(&ids)?;
    let mut selection = selection.to_vec();
    'fields: loop {
        let mut rows: Vec<_> = SETTINGS
            .iter()
            .map(|(field, label, presets)| {
                let first = setting_value(&selection[0], field);
                let current = if selection
                    .iter()
                    .any(|choice| setting_value(choice, field) != first)
                {
                    "Mixed".into()
                } else if let Some(value) = first {
                    let name = presets
                        .iter()
                        .find(|(_, preset)| *preset == value)
                        .map(|(name, _)| *name)
                        .unwrap_or("Unknown");
                    format!("{name} ({value})")
                } else {
                    "Unknown".into()
                };
                Row {
                    key: (*field).into(),
                    label: format!("{label} | Current: {current}"),
                }
            })
            .collect();
        rows.push(back());
        let field = match pick(
            picker,
            &rows,
            false,
            &[],
            "Settings. Choose one field. Esc returns to commands. Ctrl-R discards and refreshes.",
        )? {
            Pick::Cancel => return Ok(None),
            Pick::Refresh => {
                selection = settings_selection(backend, &selection)?;
                continue;
            }
            Pick::Selected(keys) if keys[0] == "back" => return Ok(None),
            Pick::Selected(keys) => keys[0].clone(),
        };
        let index = SETTINGS
            .iter()
            .position(|(key, _, _)| *key == field)
            .context("unknown setting field")?;
        let (_, _, presets) = &SETTINGS[index];
        let mut values: Vec<_> = presets
            .iter()
            .enumerate()
            .map(|(index, (label, value))| Row {
                key: format!("value:{index}"),
                label: format!(
                    "{label} ({value}){}",
                    if selection
                        .iter()
                        .all(|choice| setting_value(choice, &field) == Some(*value))
                    {
                        " | Current"
                    } else {
                        ""
                    }
                ),
            })
            .collect();
        values.push(back());
        let value = match pick(
            picker,
            &values,
            false,
            &[],
            &format!(
                "{}. Choose a preset for all selected familiars. Esc returns to fields. Ctrl-R discards and refreshes.",
                rows[index].label
            ),
        )? {
            Pick::Cancel => continue 'fields,
            Pick::Refresh => {
                selection = settings_selection(backend, &selection)?;
                continue 'fields;
            }
            Pick::Selected(keys) if keys[0] == "back" => continue 'fields,
            Pick::Selected(keys) => {
                let index = values
                    .iter()
                    .position(|row| row.key == keys[0])
                    .context("unknown setting value")?;
                presets[index].1
            }
        };
        // Recheck all identities/capabilities without starting or replacing a service.
        // NativeBackend also seals the atomic request to the original service instance.
        settings_selection(backend, &selection)?;
        let mut patch = json!({});
        patch[&field] = json!(value);
        let request = json!({"version":1,"type":"familiar.settings","ids":ids,"patch":patch});
        crate::protocol::Request::parse(&serde_json::to_vec(&request)?)?;
        let data = backend.control(request)?;
        validate_updated_ids(&data, &ids)?;
        return Ok(Some(format!("Accepted settings: {}", ids.join(", "))));
    }
}

fn target_items(
    backend: &mut impl Backend,
    picker: &mut impl Picker,
    selection: &[Choice],
    action: &str,
) -> Result<Option<Vec<Value>>> {
    let mut reserved = HashSet::new();
    let mut items = Vec::new();
    for (index, choice) in selection.iter().enumerate() {
        loop {
            let familiar = if action == "attach" {
                Some(choice.id.as_str())
            } else {
                None
            };
            let targets: Vec<Target> = pages(backend, "companion.targets", familiar, MAX_TARGETS)?;
            let mut seen = HashSet::new();
            for target in &targets {
                target.binding.validate()?;
                if let Some(owner) = &target.claimed_by {
                    valid_id(owner)?;
                }
                ensure!(
                    seen.insert(target.binding.clone()),
                    "duplicate target in discovery"
                );
            }
            let eligible: Vec<_> = targets
                .iter()
                .filter(|target| target.eligible && !reserved.contains(&target.binding))
                .collect();
            let mut rows: Vec<_> = eligible
                .iter()
                .enumerate()
                .map(|(n, target)| Row {
                    key: format!("target:{n}"),
                    label: clean(&format!(
                        "{} | {}{} | {}{} | {}",
                        target.binding.context(),
                        target.state,
                        if target.attention { " (attention)" } else { "" },
                        target.summary.as_deref().unwrap_or("No summary"),
                        if familiar.is_some() && target.claimed_by.as_deref() == familiar {
                            " (current)"
                        } else {
                            ""
                        },
                        target.reason.as_deref().unwrap_or("")
                    )),
                })
                .collect();
            rows.extend([refresh(), back()]);
            let status = if eligible.is_empty() {
                "No compatible running targets. Open a chat in an updated local Djinn UI, then Refresh. Discovery is on by default unless disabled; no OS scanning."
            } else {
                "Choose one connected target. Esc discards all pending choices."
            };
            let header = format!(
                "Target {}/{}. {status} For: {}",
                index + 1,
                selection.len(),
                choice.row.label
            );
            match pick(picker, &rows, false, &[], &header)? {
                Pick::Cancel => return Ok(None),
                Pick::Refresh => continue,
                Pick::Selected(keys) if keys[0] == "back" => return Ok(None),
                Pick::Selected(keys) if keys[0] == "refresh" => continue,
                Pick::Selected(keys) => {
                    let n = rows
                        .iter()
                        .position(|row| row.key == keys[0])
                        .context("target not offered")?;
                    let target = eligible.get(n).context("target not offered")?;
                    reserved.insert(target.binding.clone());
                    let mut item = target.binding.item();
                    item[if action == "open" { "preset" } else { "id" }] = json!(choice.id);
                    items.push(item);
                    break;
                }
            }
        }
    }
    Ok(Some(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeBackend {
        graphical: bool,
        calls: Vec<Value>,
        replies: VecDeque<(Value, Result<Value>)>,
        starts: usize,
    }

    impl FakeBackend {
        fn new(graphical: bool, replies: Vec<(Value, Result<Value>)>) -> Self {
            Self {
                graphical,
                calls: Vec::new(),
                replies: replies.into(),
                starts: 0,
            }
        }
    }

    impl Backend for FakeBackend {
        fn service(&mut self) -> Result<Service> {
            Ok(if self.graphical {
                Service::Graphical
            } else {
                Service::Offline
            })
        }
        fn ensure_graphical(&mut self) -> Result<()> {
            self.starts += 1;
            self.graphical = true;
            Ok(())
        }
        fn control(&mut self, request: Value) -> Result<Value> {
            self.calls.push(request.clone());
            let (expected, reply) = self.replies.pop_front().expect("unexpected RPC");
            assert_eq!(request, expected);
            reply
        }
    }

    struct Script {
        steps: VecDeque<Pick>,
        shown: Vec<(Vec<Row>, Vec<String>, String)>,
    }

    impl Script {
        fn new(steps: Vec<Pick>) -> Self {
            Self {
                steps: steps.into(),
                shown: Vec::new(),
            }
        }
    }

    impl Picker for Script {
        fn pick(
            &mut self,
            rows: &[Row],
            _: bool,
            selected: &[String],
            header: &str,
        ) -> Result<Pick> {
            self.shown
                .push((rows.to_vec(), selected.to_vec(), header.into()));
            Ok(self.steps.pop_front().expect("unexpected picker"))
        }
    }

    fn select(keys: &[&str]) -> Pick {
        Pick::Selected(keys.iter().map(|key| (*key).into()).collect())
    }
    fn request(kind: &str, familiar: Option<&str>, offset: usize) -> Value {
        let mut value = json!({"version": 1, "type": kind, "offset": offset});
        if let Some(id) = familiar {
            value["familiar"] = json!(id);
        }
        value
    }
    fn familiar(id: &str, capabilities: &[&str]) -> Value {
        json!({"id": id, "preset": "djinn", "attachment": null, "available": false, "capabilities": capabilities})
    }
    fn familiar_page(rows: Vec<Value>) -> Value {
        json!({"total": rows.len(), "offset": 0, "next_offset": null, "familiars": rows})
    }
    fn target(activity: &str, owner: Option<&str>, eligible: bool) -> Value {
        json!({"source": "djinn", "instance": "i", "activity": activity, "state": "working", "attention": false,
            "reason": null, "claimed_by": owner, "eligible": eligible})
    }
    fn target_page(rows: Vec<Value>) -> Value {
        json!({"total": rows.len(), "offset": 0, "next_offset": null, "targets": rows})
    }
    fn choices() -> Vec<Choice> {
        ["a", "b"]
            .iter()
            .map(|id| Choice {
                id: (*id).into(),
                preset: false,
                row: Row {
                    key: format!("familiar:{id}"),
                    label: (*id).into(),
                },
                capabilities: vec![
                    "attach".into(),
                    "info".into(),
                    "close".into(),
                    "go-to-mouse".into(),
                ],
                settings: None,
            })
            .collect()
    }
    fn item(id: &str, activity: &str) -> Value {
        json!({"id": id, "source": "djinn", "instance": "i", "activity": activity})
    }

    fn service_info(instance: &str) -> Value {
        json!({"api_version":1,"pid":123,"graphical":true,"lifecycle":true,
            "navigation":false,"service_instance":instance})
    }

    fn settings_page() -> Value {
        let mut a = familiar("a", &["settings"]);
        a["settings"] = json!({"sprite_scale":0.5,"font_size":12.0,"bubble_duration":5.0});
        let mut b = familiar("b", &["settings"]);
        b["settings"] = json!({"sprite_scale":0.5,"font_size":16.0,"bubble_duration":5.0});
        familiar_page(vec![a, b])
    }

    #[test]
    fn settings_unavailable_batch_intersection_and_current_values() {
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(settings_page())),
                (request("familiar.list", None, 0), Ok(settings_page())),
                (
                    json!({"version":1,"type":"familiar.settings","ids":["a","b"],"patch":{"font_size":16.0}}),
                    Ok(json!({"updated":["b","a"]})),
                ),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["familiar:a", "familiar:b"]),
            select(&["settings"]),
            select(&["font_size"]),
            select(&["value:2"]),
        ]);
        assert_eq!(
            menu(&mut backend, &mut picker, false).unwrap().unwrap(),
            "Accepted settings: a, b"
        );
        assert!(picker.shown[0].0[0].label.contains("Waiting"));
        assert_eq!(
            picker.shown[1]
                .0
                .iter()
                .map(|row| row.key.as_str())
                .collect::<Vec<_>>(),
            ["settings", "back"]
        );
        assert!(picker.shown[2].0[0].label.contains("Default (0.5)"));
        assert!(picker.shown[2].0[1].label.contains("Mixed"));
        assert!(
            picker.shown[3]
                .0
                .iter()
                .all(|row| !row.label.contains("Current"))
        );
        assert_eq!(backend.starts, 0);
        assert!(backend.replies.is_empty());
    }

    #[test]
    fn settings_cancel_and_back_at_every_stage_never_write() {
        for steps in [
            vec![Pick::Cancel],
            vec![select(&["familiar:a"]), Pick::Cancel, Pick::Cancel],
            vec![
                select(&["familiar:a"]),
                select(&["settings"]),
                Pick::Cancel,
                Pick::Cancel,
                Pick::Cancel,
            ],
            vec![
                select(&["familiar:a"]),
                select(&["settings"]),
                select(&["back"]),
                Pick::Cancel,
                Pick::Cancel,
            ],
            vec![
                select(&["familiar:a"]),
                select(&["settings"]),
                select(&["font_size"]),
                Pick::Cancel,
                select(&["bubble_duration"]),
                select(&["back"]),
                select(&["back"]),
                Pick::Cancel,
                Pick::Cancel,
            ],
        ] {
            let mut backend = FakeBackend::new(
                true,
                (0..2)
                    .map(|_| (request("familiar.list", None, 0), Ok(settings_page())))
                    .collect(),
            );
            let mut picker = Script::new(steps);
            assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
            assert!(
                backend
                    .calls
                    .iter()
                    .all(|call| call["type"] == "familiar.list")
            );
            assert_eq!(backend.starts, 0);
            if picker.shown.len() > 2 {
                assert_eq!(picker.shown.last().unwrap().1, ["familiar:a"]);
            }
        }
    }

    #[test]
    fn settings_unknown_legacy_and_mixed_presets_do_not_fake_defaults() {
        let mut backend = FakeBackend::new(
            true,
            vec![(
                request("familiar.list", None, 0),
                Ok(familiar_page(vec![familiar("a", &["settings"])])),
            )],
        );
        let (mut choices, _) = catalog(&mut backend).unwrap();
        assert!(!commands(&choices).iter().any(|row| row.key == "settings"));
        assert_eq!(commands(&choices[1..])[0].key, "open");
        let mut picker = Script::new(vec![select(&["font_size"]), Pick::Cancel, Pick::Cancel]);
        assert!(
            settings(&mut backend, &mut picker, &choices[..1])
                .unwrap()
                .is_none()
        );
        assert!(
            picker.shown[0]
                .0
                .iter()
                .take(3)
                .all(|row| row.label.contains("Unknown"))
        );
        assert!(
            picker.shown[1]
                .0
                .iter()
                .all(|row| !row.label.contains("Current"))
        );
        choices[0].settings = Some(crate::protocol::FamiliarSettings {
            sprite_scale: 0.7,
            font_size: 13.0,
            bubble_duration: 7.0,
        });
        let mut picker = Script::new(vec![Pick::Cancel]);
        settings(&mut backend, &mut picker, &choices[..1]).unwrap();
        assert!(picker.shown[0].0[1].label.contains("Unknown (13)"));
    }

    #[test]
    fn settings_refresh_discards_value_and_rechecks_capabilities_before_dispatch() {
        for refresh in [true, false] {
            let mut backend = FakeBackend::new(
                true,
                vec![
                    (request("familiar.list", None, 0), Ok(settings_page())),
                    (
                        request("familiar.list", None, 0),
                        Ok(familiar_page(vec![familiar("a", &["info"])])),
                    ),
                ],
            );
            let (choices, _) = catalog(&mut backend).unwrap();
            let mut picker = Script::new(vec![
                select(&["font_size"]),
                if refresh {
                    Pick::Refresh
                } else {
                    select(&["value:2"])
                },
            ]);
            assert!(
                settings(&mut backend, &mut picker, &choices[..1])
                    .unwrap_err()
                    .to_string()
                    .contains("no longer supports")
            );
            assert!(
                backend
                    .calls
                    .iter()
                    .all(|call| call["type"] == "familiar.list")
            );
            assert_eq!(backend.starts, 0);
        }
        let mut backend = FakeBackend::new(
            true,
            (0..3)
                .map(|_| (request("familiar.list", None, 0), Ok(settings_page())))
                .collect(),
        );
        let (choices, _) = catalog(&mut backend).unwrap();
        let mut picker = Script::new(vec![
            Pick::Refresh,
            select(&["font_size"]),
            Pick::Refresh,
            Pick::Cancel,
        ]);
        assert!(
            settings(&mut backend, &mut picker, &choices[..1])
                .unwrap()
                .is_none()
        );
        assert_eq!(picker.shown[0].0[0].key, picker.shown[3].0[0].key);
        assert_eq!(backend.calls.len(), 3);
    }

    #[test]
    fn settings_forged_fields_values_and_wrong_ack_sets_are_rejected() {
        for steps in [
            vec![select(&["font_size; touch x"])],
            vec![select(&["font_size"]), select(&["16"])],
        ] {
            let mut backend = FakeBackend::new(
                true,
                vec![(request("familiar.list", None, 0), Ok(settings_page()))],
            );
            let (choices, _) = catalog(&mut backend).unwrap();
            assert!(settings(&mut backend, &mut Script::new(steps), &choices[..1]).is_err());
            assert_eq!(backend.calls.len(), 1);
        }
        let ids = vec!["a".into(), "b".into()];
        for data in [
            json!({}),
            json!({"updated":["a"]}),
            json!({"updated":["a","a"]}),
            json!({"updated":["a","c"]}),
            json!({"updated":["a","b","c"]}),
        ] {
            assert!(validate_updated_ids(&data, &ids).is_err(), "{data}");
        }
        validate_updated_ids(&json!({"updated":["b","a"]}), &ids).unwrap();
    }

    fn native_server(
        replies: Vec<(Value, Value)>,
    ) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<()>) {
        use std::{
            fs,
            io::{BufRead, BufReader},
            os::unix::{fs::PermissionsExt, net::UnixListener},
            time::{Duration, Instant},
        };
        let dir = tempfile::Builder::new().prefix("numen-").tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            for (expected, reply) in replies {
                let deadline = Instant::now() + Duration::from_secs(3);
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing request: {expected}");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
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
                assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected);
                writeln!(reader.get_mut(), "{reply}").unwrap();
                // Keep the peer alive until the client has read the ACK and
                // dropped it; Darwin may reject timeout changes after teardown.
                let _ = std::io::Read::read(&mut reader, &mut [0]);
            }
        });
        (dir, path, server)
    }

    #[test]
    fn native_settings_rechecks_generation_without_repins_or_writes() {
        let ok = |data| json!({"version":1,"ok":true,"data":data});
        let probe = json!({"version":1,"type":"companion.service"});
        let (dir, path, server) = native_server(vec![
            (probe.clone(), ok(service_info("old"))),
            (
                json!({"version":1,"type":"familiar.list","offset":0,"service_instance":"old"}),
                ok(settings_page()),
            ),
            (probe, ok(service_info("replacement"))),
        ]);
        let mut backend = NativeBackend {
            path: &path,
            pinned_instance: None,
        };
        let (choices, _) = catalog(&mut backend).unwrap();
        let mut picker = Script::new(vec![select(&["font_size"]), select(&["value:2"])]);
        let error = settings(&mut backend, &mut picker, &choices[..2]).unwrap_err();
        assert!(error.to_string().contains("service instance changed"));
        assert_eq!(backend.pinned_instance.as_deref(), Some("old"));
        assert!(!dir.path().join("service.log").exists());
        server.join().unwrap();
    }

    #[test]
    fn native_restart_rejects_selected_id_and_refresh_never_repins() {
        let probe = json!({"version":1,"type":"companion.service"});
        let ok = |data| json!({"version":1,"ok":true,"data":data});
        let (dir, path, server) = native_server(vec![
            (probe.clone(), ok(service_info("old"))),
            (
                json!({"version":1,"type":"familiar.list","offset":0,"service_instance":"old"}),
                ok(familiar_page(vec![familiar("a", &["info"])])),
            ),
            // The service restarted while fzf was open and reused familiar a.
            // The root precondition lets the server reject before revealing it.
            (
                json!({"version":1,"type":"companion.reveal","familiars":["a"],"service_instance":"old"}),
                json!({"version":1,"ok":false,"error":"service instance changed; reopen menu"}),
            ),
            (probe.clone(), ok(service_info("new"))),
            (probe, ok(service_info("new"))),
        ]);
        let mut backend = NativeBackend {
            path: &path,
            pinned_instance: None,
        };
        let mut picker = Script::new(vec![
            select(&["familiar:a"]),
            select(&["info"]),
            Pick::Refresh,
            select(&["back"]),
            select(&["back"]),
        ]);
        assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
        for index in [2, 3, 4] {
            assert!(
                picker.shown[index]
                    .2
                    .contains("service instance changed; reopen menu")
            );
        }
        assert_eq!(backend.pinned_instance.as_deref(), Some("old"));
        assert!(!dir.path().join("service.log").exists());
        server.join().unwrap();
    }

    #[test]
    fn native_bootstrap_pins_and_seals_every_menu_rpc() {
        let requests = vec![
            request("familiar.list", None, 0),
            request("companion.targets", Some("a"), 0),
            json!({"version":1,"type":"familiar.open","items":[{"preset":"djinn","source":"djinn","instance":"i","activity":"one"}]}),
            json!({"version":1,"type":"familiar.attach","items":[item("a", "one")]}),
            json!({"version":1,"type":"familiar.close","ids":["a"]}),
            json!({"version":1,"type":"familiar.go-to-mouse","ids":["a"]}),
            json!({"version":1,"type":"familiar.settings","ids":["a"],"patch":{"font_size":16}}),
            json!({"version":1,"type":"companion.reveal","familiars":["a"],"service_instance":"forged"}),
        ];
        let mut replies = vec![(
            json!({"version":1,"type":"companion.service"}),
            json!({"version":1,"ok":true,"data":service_info("old")}),
        )];
        for request in &requests {
            let mut sealed = request.clone();
            sealed["service_instance"] = json!("old");
            replies.push((sealed, json!({"version":1,"ok":true,"data":{}})));
        }
        let (_dir, path, server) = native_server(replies);
        let mut backend = NativeBackend {
            path: &path,
            pinned_instance: None,
        };
        assert!(
            backend
                .control(requests[0].clone())
                .unwrap_err()
                .to_string()
                .contains("reopen menu")
        );
        backend.ensure_graphical().unwrap();
        assert_eq!(backend.pinned_instance.as_deref(), Some("old"));
        for request in requests {
            backend.control(request).unwrap();
        }
        server.join().unwrap();
    }

    #[test]
    fn native_missing_pinned_service_never_bootstraps_replacement() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new().prefix("numen-").tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("s");
        let mut backend = NativeBackend {
            path: &path,
            pinned_instance: Some("old".into()),
        };
        assert!(
            backend
                .service()
                .err()
                .unwrap()
                .to_string()
                .contains("reopen menu")
        );
        assert!(
            backend
                .ensure_graphical()
                .unwrap_err()
                .to_string()
                .contains("reopen menu")
        );
        assert_eq!(backend.pinned_instance.as_deref(), Some("old"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn offline_catalog_is_single_source_and_never_starts() {
        let mut backend = FakeBackend::new(false, vec![]);
        let (catalog, _) = catalog(&mut backend).unwrap();
        assert_eq!(
            presets(),
            json!({"presets":[{"id":"djinn","name":"Djinn","capabilities":["open"]}]})
        );
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].row.key, "preset:djinn");
        let mut picker = Script::new(vec![Pick::Cancel]);
        assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
        assert_eq!(backend.starts, 0);
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn typed_ids_capability_intersection_and_sanitized_labels() {
        let mut row = familiar("djinn", &["info", "attach", "close"]);
        row["summary"] = json!("a\t\u{1b}[31mred\u{1b}[0m\nline");
        let mut backend = FakeBackend::new(
            true,
            vec![(
                request("familiar.list", None, 0),
                Ok(familiar_page(vec![row])),
            )],
        );
        let (catalog, _) = catalog(&mut backend).unwrap();
        assert_eq!(catalog[0].row.key, "familiar:djinn");
        assert_eq!(catalog[1].row.key, "preset:djinn");
        assert!(
            catalog[0]
                .row
                .label
                .contains("Waiting | Djinn [djinn] | a red line")
        );
        assert!(
            !commands(&catalog[..1])
                .iter()
                .any(|row| row.key == "go-to-mouse")
        );
        assert_eq!(commands(&catalog).len(), 1);
        assert_eq!(commands(&catalog[1..])[0].key, "open");
    }

    #[test]
    fn fzf_rejects_forgery_duplicates_oversize_and_ignores_labels() {
        let rows = vec![Row {
            key: "familiar:a".into(),
            label: "Original".into(),
        }];
        for bytes in [
            b"\nforged\tlabel\n".as_slice(),
            b"\nfamiliar:a\tx\nfamiliar:a\ty\n",
            b"wrong\nfamiliar:a\tx\n",
            b"\n",
        ] {
            assert!(decode_selection(bytes, &rows, true).is_err());
        }
        assert!(decode_selection(&vec![b'x'; MAX_OUTPUT + 1], &rows, true).is_err());
        assert!(matches!(
            decode_selection(b"\nfamiliar:a\tforged label\n", &rows, true).unwrap(),
            Pick::Selected(_)
        ));
        assert!(matches!(
            decode_selection(b"ctrl-r\n", &rows, true).unwrap(),
            Pick::Refresh
        ));
        let many = (0..17)
            .map(|n| Row {
                key: n.to_string(),
                label: "x".into(),
            })
            .collect::<Vec<_>>();
        let output = format!(
            "\n{}",
            many.iter()
                .map(|row| format!("{}\tx\n", row.key))
                .collect::<String>()
        );
        assert!(decode_selection(output.as_bytes(), &many, true).is_err());
    }

    #[test]
    fn source_identifiers_and_terminal_sequences_are_not_trusted() {
        for id in ["", "a\tb", "$(touch x)", "a/b"] {
            assert!(valid_id(id).is_err());
        }
        assert_eq!(
            clean("a\u{1b}]8;;https://example.test\u{7}b\u{1b}]8;;\u{1b}\\\n\t c\u{202e}"),
            "ab c"
        );
    }

    #[test]
    fn paginates_four_rows_and_rejects_stalls_counts_and_growth() {
        let rows: Vec<_> = (0..4).map(|n| familiar(&format!("f{n}"), &[])).collect();
        let first = json!({"familiars": rows, "offset": 0, "total": 5, "next_offset": 4});
        let last = json!({"familiars": [familiar("f4", &[])], "offset": 4, "total": 5, "next_offset": null});
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(first.clone())),
                (request("familiar.list", None, 4), Ok(last)),
            ],
        );
        assert_eq!(
            pages::<Familiar>(&mut backend, "familiar.list", None, 16)
                .unwrap()
                .len(),
            5
        );
        for bad in [
            json!({"familiars": [], "offset": 0, "total": 1, "next_offset": 0}),
            json!({"familiars": [], "offset": 0, "total": 17, "next_offset": null}),
            json!({"familiars": [], "offset": 0, "total": 1, "next_offset": null}),
            json!({"familiars": [], "offset": 1, "total": 0, "next_offset": null}),
        ] {
            let mut backend =
                FakeBackend::new(true, vec![(request("familiar.list", None, 0), Ok(bad))]);
            assert!(pages::<Familiar>(&mut backend, "familiar.list", None, 16).is_err());
        }
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(first)),
                (
                    request("familiar.list", None, 4),
                    Ok(json!({"familiars": [], "offset": 4, "total": 4, "next_offset": null})),
                ),
            ],
        );
        assert!(pages::<Familiar>(&mut backend, "familiar.list", None, 16).is_err());
    }

    #[test]
    fn target_bound_is_512() {
        let mut replies = Vec::new();
        for offset in (0..512).step_by(4) {
            let rows: Vec<_> = (offset..offset + 4)
                .map(|n| target(&format!("t{n}"), None, true))
                .collect();
            replies.push((
                request("companion.targets", None, offset),
                Ok(json!({"targets":rows,"offset":offset,"total":512,
                "next_offset": if offset + 4 < 512 { Some(offset + 4) } else { None }})),
            ));
        }
        let mut backend = FakeBackend::new(true, replies);
        assert_eq!(
            pages::<Target>(&mut backend, "companion.targets", None, MAX_TARGETS)
                .unwrap()
                .len(),
            512
        );
        assert_eq!(backend.calls.len(), 128);
    }

    #[test]
    fn attach_queries_per_identity_reserves_targets_and_commits_once() {
        let targets = target_page(vec![
            target("one", Some("a"), true),
            target("two", None, true),
            target("blocked", Some("other"), false),
        ]);
        let mut backend = FakeBackend::new(
            true,
            vec![
                (
                    request("companion.targets", Some("a"), 0),
                    Ok(targets.clone()),
                ),
                (request("companion.targets", Some("b"), 0), Ok(targets)),
                (
                    json!({"version":1,"type":"familiar.attach","items":[item("a","one"),item("b","two")]}),
                    Ok(json!({"attached":["a","b"]})),
                ),
            ],
        );
        let mut picker = Script::new(vec![select(&["target:0"]), select(&["target:0"])]);
        assert_eq!(
            execute(&mut backend, &mut picker, &choices(), "attach", false)
                .unwrap()
                .unwrap(),
            "Accepted attach: a, b"
        );
        assert!(picker.shown[0].0[0].label.contains("(current)"));
        assert!(
            picker.shown[1]
                .0
                .iter()
                .all(|row| !row.label.contains("/one") && !row.label.contains("/blocked"))
        );
        assert_eq!(backend.starts, 0);
    }

    #[test]
    fn cancelling_any_target_discards_entire_batch() {
        for cancel_first in [true, false] {
            let mut replies = vec![(
                request("companion.targets", Some("a"), 0),
                Ok(target_page(vec![target("one", None, true)])),
            )];
            let steps = if cancel_first {
                vec![Pick::Cancel]
            } else {
                replies.push((
                    request("companion.targets", Some("b"), 0),
                    Ok(target_page(vec![target("two", None, true)])),
                ));
                vec![select(&["target:0"]), Pick::Cancel]
            };
            let mut backend = FakeBackend::new(true, replies);
            assert!(
                execute(
                    &mut backend,
                    &mut Script::new(steps),
                    &choices(),
                    "attach",
                    false
                )
                .unwrap()
                .is_none()
            );
            assert!(
                backend
                    .calls
                    .iter()
                    .all(|call| call["type"] == "companion.targets")
            );
        }
    }

    #[test]
    fn empty_targets_refresh_then_back_never_mutates() {
        let mut backend = FakeBackend::new(
            true,
            (0..3)
                .map(|_| {
                    (
                        request("companion.targets", Some("a"), 0),
                        Ok(target_page(vec![])),
                    )
                })
                .collect(),
        );
        let mut picker = Script::new(vec![Pick::Refresh, select(&["refresh"]), select(&["back"])]);
        assert!(
            execute(&mut backend, &mut picker, &choices()[..1], "attach", false)
                .unwrap()
                .is_none()
        );
        assert!(
            picker.shown.iter().all(|(rows, _, header)| rows.len() == 2
                && header.contains("No compatible running targets"))
        );
    }

    #[test]
    fn direct_commands_target_exact_ids_and_do_not_bootstrap() {
        for (action, kind, field, data) in [
            (
                "info",
                "companion.reveal",
                "familiars",
                json!({"summary":"not printed"}),
            ),
            (
                "go-to-mouse",
                "familiar.go-to-mouse",
                "ids",
                json!({"moving":["a","b"]}),
            ),
            (
                "close",
                "familiar.close",
                "ids",
                json!({"closed":["a","b"]}),
            ),
        ] {
            let mut request = json!({"version":1,"type":kind});
            request[field] = json!(["a", "b"]);
            let mut backend = FakeBackend::new(false, vec![(request, Ok(data))]);
            let message = execute(
                &mut backend,
                &mut Script::new(vec![]),
                &choices(),
                action,
                false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(message, format!("Accepted {action}: a, b"));
            assert_eq!(backend.starts, 0);
        }
    }

    #[test]
    fn offline_no_start_open_is_visible_and_retryable() {
        let mut backend = FakeBackend::new(false, vec![]);
        let mut picker = Script::new(vec![
            select(&["preset:djinn"]),
            select(&["open"]),
            Pick::Cancel,
            Pick::Cancel,
        ]);
        assert!(menu(&mut backend, &mut picker, true).unwrap().is_none());
        assert!(picker.shown[2].2.contains("--no-start"));
        assert_eq!(picker.shown[3].1, vec!["preset:djinn"]);
        assert_eq!(backend.starts, 0);
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn open_bootstraps_then_queries_and_uses_generated_ids() {
        let mut backend = FakeBackend::new(
            false,
            vec![
                (
                    request("companion.targets", None, 0),
                    Ok(target_page(vec![target("one", None, true)])),
                ),
                (
                    json!({"version":1,"type":"familiar.open","items":[{"preset":"djinn","source":"djinn","instance":"i","activity":"one"}]}),
                    Ok(json!({"opened":["familiar-1"]})),
                ),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["preset:djinn"]),
            select(&["open"]),
            select(&["target:0"]),
        ]);
        assert_eq!(
            menu(&mut backend, &mut picker, false).unwrap().unwrap(),
            "Accepted open: familiar-1"
        );
        assert_eq!(backend.starts, 1);
        assert!(picker.steps.is_empty());
    }

    #[test]
    fn race_error_preserves_selection_and_allows_fresh_retry() {
        for error in ["source exited", "target claimed by another familiar"] {
            let mut backend = FakeBackend::new(
                true,
                vec![
                    (
                        request("familiar.list", None, 0),
                        Ok(familiar_page(vec![familiar("a", &["attach"])])),
                    ),
                    (
                        request("companion.targets", Some("a"), 0),
                        Ok(target_page(vec![target("one", None, true)])),
                    ),
                    (
                        json!({"version":1,"type":"familiar.attach","items":[item("a","one")]}),
                        Err(anyhow::anyhow!("{error}")),
                    ),
                    (
                        request("companion.targets", Some("a"), 0),
                        Ok(target_page(vec![target("two", None, true)])),
                    ),
                    (
                        json!({"version":1,"type":"familiar.attach","items":[item("a","two")]}),
                        Ok(json!({"attached":["a"]})),
                    ),
                ],
            );
            let mut picker = Script::new(vec![
                select(&["familiar:a"]),
                select(&["attach"]),
                select(&["target:0"]),
                select(&["attach"]),
                select(&["target:0"]),
            ]);
            assert_eq!(
                menu(&mut backend, &mut picker, false).unwrap().unwrap(),
                "Accepted attach: a"
            );
            assert!(picker.shown[3].2.contains(error));
            assert_eq!(backend.starts, 0);
        }
    }

    #[test]
    fn target_cancel_returns_to_command_and_command_cancel_retains_selection() {
        let list = familiar_page(vec![familiar("a", &["attach", "info"])]);
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(list.clone())),
                (
                    request("companion.targets", Some("a"), 0),
                    Ok(target_page(vec![target("one", None, true)])),
                ),
                (request("familiar.list", None, 0), Ok(list)),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["familiar:a"]),
            select(&["attach"]),
            Pick::Cancel,
            Pick::Cancel,
            Pick::Cancel,
        ]);
        assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
        assert!(picker.shown[3].2.contains("no batch was sent"));
        assert_eq!(picker.shown[4].1, vec!["familiar:a"]);
    }

    #[test]
    fn command_refresh_updates_capabilities_without_dispatch() {
        let mut backend = FakeBackend::new(
            true,
            vec![
                (
                    request("familiar.list", None, 0),
                    Ok(familiar_page(vec![familiar("a", &["info", "go-to-mouse"])])),
                ),
                (
                    request("familiar.list", None, 0),
                    Ok(familiar_page(vec![familiar("a", &["info"])])),
                ),
                (
                    request("familiar.list", None, 0),
                    Ok(familiar_page(vec![familiar("a", &["info"])])),
                ),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["familiar:a"]),
            Pick::Refresh,
            Pick::Cancel,
            Pick::Cancel,
        ]);
        assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
        assert!(picker.shown[1].0.iter().any(|row| row.key == "go-to-mouse"));
        assert!(!picker.shown[2].0.iter().any(|row| row.key == "go-to-mouse"));
        assert_eq!(backend.calls.len(), 3);
    }

    #[test]
    fn forged_fake_picker_cannot_dispatch() {
        let mut backend = FakeBackend::new(false, vec![]);
        let mut picker = Script::new(vec![select(&["familiar:forged"])]);
        assert!(menu(&mut backend, &mut picker, false).is_err());
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn unsupported_service_never_shows_logical_main_or_gets_replaced() {
        struct Unsupported;
        impl Backend for Unsupported {
            fn service(&mut self) -> Result<Service> {
                Ok(Service::Unsupported)
            }
            fn ensure_graphical(&mut self) -> Result<()> {
                panic!("must not replace service")
            }
            fn control(&mut self, _: Value) -> Result<Value> {
                panic!("must not query logical headless main")
            }
        }
        let mut picker = Script::new(vec![
            select(&["preset:djinn"]),
            select(&["open"]),
            Pick::Cancel,
            Pick::Cancel,
        ]);
        assert!(
            menu(&mut Unsupported, &mut picker, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(picker.shown[0].0.len(), 1);
        assert_eq!(picker.shown[0].0[0].key, "preset:djinn");
        assert!(picker.shown[0].2.contains("unsupported"));
        assert!(picker.shown[2].2.contains("will not be replaced"));
    }

    #[test]
    fn mixed_selection_has_only_back_and_dispatches_nothing() {
        let list = familiar_page(vec![familiar("djinn", &["info", "attach", "close"])]);
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(list.clone())),
                (request("familiar.list", None, 0), Ok(list)),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["familiar:djinn", "preset:djinn"]),
            select(&["back"]),
            Pick::Cancel,
        ]);
        assert!(menu(&mut backend, &mut picker, false).unwrap().is_none());
        assert_eq!(picker.shown[1].0.len(), 1);
        assert_eq!(picker.shown[1].0[0].key, "back");
        assert!(
            picker.shown[1]
                .2
                .contains("No command applies to the entire selection")
        );
        assert_eq!(backend.calls.len(), 2);
    }

    #[test]
    fn cancelled_batch_reservations_are_discarded_before_retry() {
        let targets = target_page(vec![target("one", None, true), target("two", None, true)]);
        let list = familiar_page(vec![familiar("a", &["attach"]), familiar("b", &["attach"])]);
        let mut backend = FakeBackend::new(
            true,
            vec![
                (request("familiar.list", None, 0), Ok(list)),
                (
                    request("companion.targets", Some("a"), 0),
                    Ok(targets.clone()),
                ),
                (
                    request("companion.targets", Some("b"), 0),
                    Ok(targets.clone()),
                ),
                (
                    request("companion.targets", Some("a"), 0),
                    Ok(targets.clone()),
                ),
                (request("companion.targets", Some("b"), 0), Ok(targets)),
                (
                    json!({"version":1,"type":"familiar.attach","items":[item("a","one"),item("b","two")]}),
                    Ok(json!({"attached":["a","b"]})),
                ),
            ],
        );
        let mut picker = Script::new(vec![
            select(&["familiar:a", "familiar:b"]),
            select(&["attach"]),
            select(&["target:0"]),
            Pick::Cancel,
            select(&["attach"]),
            select(&["target:0"]),
            select(&["target:0"]),
        ]);
        assert_eq!(
            menu(&mut backend, &mut picker, false).unwrap().unwrap(),
            "Accepted attach: a, b"
        );
        assert!(picker.shown[5].0[0].label.contains("/one"));
        assert_eq!(
            backend
                .calls
                .iter()
                .filter(|request| request["type"] == "familiar.attach")
                .count(),
            1
        );
    }

    #[test]
    fn opening_on_existing_gui_does_not_require_bootstrap_or_navigation() {
        let mut backend = FakeBackend::new(
            true,
            vec![(
                request("companion.targets", None, 0),
                Ok(target_page(vec![])),
            )],
        );
        let preset = Choice {
            id: "djinn".into(),
            preset: true,
            row: Row {
                key: "preset:djinn".into(),
                label: "Preset | Djinn [djinn]".into(),
            },
            capabilities: vec!["open".into()],
            settings: None,
        };
        assert!(
            execute(
                &mut backend,
                &mut Script::new(vec![Pick::Cancel]),
                &[preset],
                "open",
                false
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(backend.starts, 0);
    }
}
