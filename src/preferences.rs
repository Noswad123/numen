//! Explicit appearance and manual foot placement only. No environment reads or
//! application/window actions. Publication preserves conflicts, not atomic CAS;
//! live editor inodes are retained for one generation, not indefinitely.
use std::{
    collections::BTreeSet,
    ffi::{CString, OsString},
    fs::{self, File, Metadata},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::protocol::{FamiliarSettings, validate_id};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const MAX_RECORDS: usize = 128;
pub const MAX_BYTES: usize = 256 * 1024;

fn identifier<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = String::deserialize(d)?;
    validate_id(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    #[serde(deserialize_with = "identifier")]
    pub source: String,
    #[serde(deserialize_with = "identifier")]
    pub activity: String,
}

impl Key {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.source)?;
        validate_id(&self.activity)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Foot center in desktop logical coordinates, independent of any window ID.
pub struct Placement {
    pub x: f64,
    pub y: f64,
}

impl Placement {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [self.x, self.y]
                .iter()
                .all(|v| v.is_finite() && (-1_000_000.0..=1_000_000.0).contains(v)),
            "invalid placement"
        );
        Ok(())
    }
}

fn settings<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<FamiliarSettings, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
        sprite_scale: f64,
        font_size: f64,
        bubble_duration: f64,
    }
    let v = Fields::deserialize(d)?;
    let settings = FamiliarSettings {
        sprite_scale: v.sprite_scale,
        font_size: v.font_size,
        bubble_duration: v.bubble_duration,
    };
    settings.validate().map_err(serde::de::Error::custom)?;
    Ok(settings)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(deserialize_with = "identifier")]
    pub source: String,
    #[serde(deserialize_with = "identifier")]
    pub activity: String,
    #[serde(deserialize_with = "settings")]
    pub settings: FamiliarSettings,
    pub placement: Option<Placement>,
}

impl Record {
    pub fn validate(&self) -> Result<()> {
        Key {
            source: self.source.clone(),
            activity: self.activity.clone(),
        }
        .validate()?;
        self.settings.validate()?;
        if let Some(p) = self.placement {
            p.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub records: Vec<Record>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            records: Vec::new(),
        }
    }
}

impl Document {
    pub fn validate(&self) -> Result<()> {
        check(self.version == 1, Reason::UnsupportedVersion)?;
        check(self.records.len() <= MAX_RECORDS, Reason::CapacityReached)?;
        let mut keys = BTreeSet::new();
        for r in &self.records {
            r.validate().map_err(|_| Failure(Reason::InvalidData))?;
            check(keys.insert((&r.source, &r.activity)), Reason::InvalidData)?;
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut sorted = self.clone();
        sorted
            .records
            .sort_by(|a, b| (&a.source, &a.activity).cmp(&(&b.source, &b.activity)));
        let bytes = serde_json::to_vec(&sorted)?;
        check(bytes.len() <= MAX_BYTES, Reason::TooLarge)?;
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        check(bytes.len() <= MAX_BYTES, Reason::TooLarge)?;
        // Derived struct visitors reject duplicate fields at every nesting level.
        let mut document: Self =
            serde_json::from_slice(bytes).map_err(|_| Failure(Reason::InvalidData))?;
        document.validate()?;
        document
            .records
            .sort_by(|a, b| (&a.source, &a.activity).cmp(&(&b.source, &b.activity)));
        Ok(document)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Disabled,
    ReadOnly,
    Writable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadState {
    NotLoaded,
    Absent,
    Loaded,
    Rejected,
    TimedOut,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteState {
    Clean,
    Pending,
    Saving,
    Blocked,
    Failed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Disabled,
    NoHome,
    RelativePath,
    UnsafePath,
    LockBusy,
    InvalidData,
    UnsupportedVersion,
    TooLarge,
    IoError,
    ExternalChange,
    RecoveryRequired,
    CapacityReached,
    StartupTimeout,
    ShutdownTimeout,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub mode: Mode,
    pub load: LoadState,
    pub write: WriteState,
    pub reason: Option<Reason>,
    pub accepted_revision: u64,
    pub durable_revision: u64,
    pub records: usize,
    pub path: Option<String>,
}

fn safe_text(s: &str) -> bool {
    s.len() <= 1024
        && !s.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

impl Status {
    pub fn disabled() -> Self {
        Self {
            mode: Mode::Disabled,
            load: LoadState::NotLoaded,
            write: WriteState::Clean,
            reason: Some(Reason::Disabled),
            accepted_revision: 0,
            durable_revision: 0,
            records: 0,
            path: None,
        }
    }
    pub fn unavailable(reason: Reason) -> Self {
        if reason == Reason::Disabled {
            return Self::disabled();
        }
        let load = match reason {
            Reason::StartupTimeout => LoadState::TimedOut,
            Reason::LockBusy => LoadState::Absent,
            _ => LoadState::NotLoaded,
        };
        Self {
            mode: Mode::ReadOnly,
            load,
            write: WriteState::Blocked,
            reason: Some(reason),
            ..Self::disabled()
        }
    }
    pub fn valid(&self) -> bool {
        if self.durable_revision > self.accepted_revision
            || self.records > MAX_RECORDS
            || self
                .path
                .as_ref()
                .is_some_and(|p| p.is_empty() || !Path::new(p).is_absolute() || !safe_text(p))
        {
            return false;
        }
        if matches!(
            self.load,
            LoadState::NotLoaded | LoadState::Rejected | LoadState::TimedOut
        ) && (self.records != 0 || self.accepted_revision != 0 || self.durable_revision != 0)
        {
            return false;
        }
        match self.mode {
            Mode::Disabled => {
                self.load == LoadState::NotLoaded
                    && self.write == WriteState::Clean
                    && self.reason == Some(Reason::Disabled)
                    && self.path.is_none()
                    && self.records == 0
                    && self.accepted_revision == 0
                    && self.durable_revision == 0
            }
            Mode::ReadOnly => {
                matches!(self.write, WriteState::Blocked | WriteState::Failed)
                    && self.reason.is_some()
                    && !matches!(
                        self.reason,
                        Some(Reason::Disabled | Reason::CapacityReached)
                    )
                    && ((self.load == LoadState::TimedOut)
                        == (self.reason == Some(Reason::StartupTimeout)))
                    && (self.reason != Some(Reason::LockBusy)
                        || matches!(self.load, LoadState::Loaded | LoadState::Absent))
            }
            Mode::Writable => {
                matches!(self.load, LoadState::Loaded | LoadState::Absent)
                    && self.path.is_some()
                    && matches!(
                        self.write,
                        WriteState::Clean
                            | WriteState::Pending
                            | WriteState::Saving
                            | WriteState::Blocked
                    )
                    && (self.reason.is_none() || self.reason == Some(Reason::CapacityReached))
                    && (self.write != WriteState::Blocked
                        || self.reason == Some(Reason::CapacityReached))
                    && (self.write != WriteState::Clean
                        || (self.accepted_revision == self.durable_revision
                            && self.reason.is_none()))
                    && (!matches!(self.write, WriteState::Pending | WriteState::Saving)
                        || self.accepted_revision > self.durable_revision)
            }
        }
    }
}

pub fn default_path(config_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let root = if let Some(root) = config_home.filter(|p| !p.is_empty()) {
        PathBuf::from(root)
    } else {
        let home = home
            .filter(|p| !p.is_empty())
            .ok_or(Failure(Reason::NoHome))?;
        let home = PathBuf::from(home);
        check(home.is_absolute(), Reason::RelativePath)?;
        home.join(".config")
    };
    check(root.is_absolute(), Reason::RelativePath)?;
    Ok(root.join("numen/preferences.json"))
}

#[derive(Debug)]
struct Failure(Reason);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "preferences: {:?}", self.0)
    }
}
impl std::error::Error for Failure {}
fn check(ok: bool, reason: Reason) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Failure(reason).into())
    }
}
fn reason(error: &anyhow::Error) -> Reason {
    error
        .downcast_ref::<Failure>()
        .map_or(Reason::IoError, |e| e.0)
}

/// A bounded diagnostic category, never the underlying error text or IDs.
pub fn error_reason(error: &anyhow::Error) -> Reason {
    reason(error)
}

struct Pending {
    document: Document,
    revision: u64,
    changed: Instant,
}
struct State {
    status: Status,
    pending: Option<Pending>,
    first_dirty: Option<Instant>,
    flush_target: Option<u64>,
    capacity: bool,
    stop: bool,
    ended: bool,
    startup: Option<Document>,
}
struct Shared {
    state: Mutex<State>,
    notified: Mutex<Option<Status>>,
    wake: Condvar,
    cancelled: AtomicBool,
    callback: Box<dyn Fn(Status) + Send + Sync>,
}

/// The worker owns all filesystem operations. The live mailbox holds at most
/// one pending immutable document in addition to the worker's in-flight one.
pub struct Writer {
    shared: Arc<Shared>,
}

impl Writer {
    pub fn submit(&self, document: Document, revision: u64) -> bool {
        if document.validate().is_err() {
            return false;
        }
        let Ok(mut state) = self.shared.state.try_lock() else {
            return false;
        };
        if state.stop
            || state.ended
            || state.status.mode != Mode::Writable
            || revision <= state.status.accepted_revision
        {
            return false;
        }
        let now = Instant::now();
        state.status.accepted_revision = revision;
        state.status.records = document.records.len();
        state.status.write = WriteState::Pending;
        state.status.reason = None;
        state.capacity = false;
        state.first_dirty.get_or_insert(now);
        state.pending = Some(Pending {
            document,
            revision,
            changed: now,
        });
        self.shared.wake.notify_all();
        true
    }
    pub fn status(&self) -> Status {
        self.shared.state.lock().unwrap().status.clone()
    }
    pub fn can_write(&self) -> bool {
        self.status().mode == Mode::Writable
    }
    pub fn note_capacity(&self) {
        let mut state = self.shared.state.lock().unwrap();
        if state.status.mode == Mode::Writable && !state.stop {
            state.capacity = true;
            state.status.reason = Some(Reason::CapacityReached);
            state.status.write = WriteState::Blocked;
            self.shared.wake.notify_all();
        }
    }
    /// Flush the accepted revision. False also means a known capacity decline,
    /// even when the accepted subset is durable; it never means all edits saved.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut state = self.shared.state.lock().unwrap();
        let target = state.status.accepted_revision;
        if state.status.durable_revision >= target {
            return !state.capacity;
        }
        state.flush_target = Some(state.flush_target.unwrap_or(0).max(target));
        self.shared.wake.notify_all();
        loop {
            if state.status.durable_revision >= target {
                return !state.capacity;
            }
            if state.ended || state.status.mode != Mode::Writable {
                return false;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self.shared.wake.wait_timeout(state, left).unwrap().0;
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(450);
        let mut state = self.shared.state.lock().unwrap();
        state.stop = true;
        self.shared.wake.notify_all();
        while !state.ended {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                state.status.mode = Mode::ReadOnly;
                state.status.write = WriteState::Failed;
                state.status.reason = Some(Reason::ShutdownTimeout);
                self.shared.cancelled.store(true, Ordering::Release);
                return;
            };
            state = self.shared.wake.wait_timeout(state, left).unwrap().0;
        }
    }
}

pub fn start(
    path: PathBuf,
    on_status: impl Fn(Status) + Send + Sync + 'static,
) -> (Document, Writer) {
    let deadline = Instant::now() + Duration::from_millis(190);
    let mut status = Status::unavailable(Reason::IoError);
    status.path = path
        .to_str()
        .filter(|s| safe_text(s) && path.is_absolute())
        .map(str::to_owned);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            status,
            pending: None,
            first_dirty: None,
            flush_target: None,
            capacity: false,
            stop: false,
            ended: false,
            startup: None,
        }),
        notified: Mutex::new(None),
        wake: Condvar::new(),
        cancelled: AtomicBool::new(false),
        callback: Box::new(on_status),
    });
    let worker = shared.clone();
    let spawned = thread::Builder::new()
        .name("numen-preferences".into())
        .spawn(move || run(worker, path, deadline));
    let mut state = shared.state.lock().unwrap();
    if spawned.is_err() {
        state.ended = true;
        state.status.load = LoadState::Rejected;
        state.startup = Some(Document::default());
    }
    while state.startup.is_none() && !state.ended {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        state = shared.wake.wait_timeout(state, left).unwrap().0;
    }
    let document = if let Some(document) = state.startup.take() {
        document
    } else {
        shared.cancelled.store(true, Ordering::Release);
        state.status.mode = Mode::ReadOnly;
        state.status.load = LoadState::TimedOut;
        state.status.write = WriteState::Blocked;
        state.status.reason = Some(Reason::StartupTimeout);
        state.stop = true;
        Document::default()
    };
    drop(state);
    (document, Writer { shared })
}

fn notify(shared: &Shared) {
    let status = shared.state.lock().unwrap().status.clone();
    let mut last = shared.notified.lock().unwrap();
    if last.as_ref() == Some(&status) {
        return;
    }
    *last = Some(status.clone());
    drop(last);
    // No mutex is held across callbacks (including a slow diagnostics consumer).
    (shared.callback)(status);
}

fn run(shared: Arc<Shared>, path: PathBuf, deadline: Instant) {
    let loaded = Store::load(&path, &shared, deadline);
    let mut state = shared.state.lock().unwrap();
    if shared.cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        shared.cancelled.store(true, Ordering::Release);
        state.status.mode = Mode::ReadOnly;
        state.status.load = LoadState::TimedOut;
        state.status.write = WriteState::Blocked;
        state.status.reason = Some(Reason::StartupTimeout);
        state.ended = true;
        shared.wake.notify_all();
        drop(state);
        notify(&shared);
        return;
    }
    let mut store = match loaded {
        Ok((mut store, document)) => {
            store.deadline = None;
            state.status.mode = if store.locked {
                Mode::Writable
            } else {
                Mode::ReadOnly
            };
            state.status.load = if store.baseline.is_some() {
                LoadState::Loaded
            } else {
                LoadState::Absent
            };
            state.status.write = if store.locked {
                WriteState::Clean
            } else {
                WriteState::Blocked
            };
            state.status.reason = if store.locked {
                None
            } else {
                Some(Reason::LockBusy)
            };
            state.status.records = document.records.len();
            state.startup = Some(document);
            Some(store)
        }
        Err(error) => {
            state.status.load = LoadState::Rejected;
            state.status.reason = Some(reason(&error));
            state.startup = Some(Document::default());
            None
        }
    };
    shared.wake.notify_all();
    drop(state);
    notify(&shared);
    loop {
        let mut state = shared.state.lock().unwrap();
        if shared.cancelled.load(Ordering::Acquire) {
            break;
        }
        if state.pending.is_none() {
            if state.stop {
                break;
            }
            state = shared
                .wake
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap()
                .0;
            drop(state);
            notify(&shared);
            continue;
        }
        let pending = state.pending.as_ref().unwrap();
        let due = (pending.changed + Duration::from_millis(250))
            .min(state.first_dirty.unwrap() + Duration::from_secs(2));
        let flushing = state
            .flush_target
            .is_some_and(|target| state.status.durable_revision < target);
        if !state.stop && !flushing && Instant::now() < due {
            let left = due.saturating_duration_since(Instant::now());
            state = shared.wake.wait_timeout(state, left).unwrap().0;
            drop(state);
            continue;
        }
        let pending = state.pending.take().unwrap();
        state.first_dirty = None;
        state.status.write = if state.capacity {
            WriteState::Blocked
        } else {
            WriteState::Saving
        };
        drop(state);
        notify(&shared);
        let result = pending.document.encode().and_then(|bytes| {
            store
                .as_mut()
                .ok_or_else(|| Failure(Reason::IoError).into())
                .and_then(|s| s.save(&bytes, &shared))
        });
        let mut state = shared.state.lock().unwrap();
        if !shared.cancelled.load(Ordering::Acquire) {
            match result {
                Ok(()) => {
                    state.status.durable_revision = pending.revision;
                    if state
                        .flush_target
                        .is_some_and(|target| pending.revision >= target)
                    {
                        state.flush_target = None;
                    }
                    state.status.write = if state.capacity {
                        WriteState::Blocked
                    } else if state.pending.is_some() {
                        WriteState::Pending
                    } else {
                        WriteState::Clean
                    };
                }
                Err(error) => {
                    state.status.mode = Mode::ReadOnly;
                    state.status.write = WriteState::Failed;
                    state.status.reason = Some(reason(&error));
                    state.pending = None;
                }
            }
        }
        shared.wake.notify_all();
        drop(state);
        notify(&shared);
    }
    // Closing the directory/lock descriptors may block on unusual filesystems;
    // the caller never joins this detached worker unconditionally.
    drop(store.take());
    let mut state = shared.state.lock().unwrap();
    state.ended = true;
    shared.wake.notify_all();
    drop(state);
    notify(&shared);
}

struct Snapshot {
    bytes: Vec<u8>,
    metadata: Metadata,
}
struct Store<'a> {
    shared: &'a Shared,
    deadline: Option<Instant>,
    root: File,
    directory: File,
    root_path: PathBuf,
    leaf: CString,
    name: CString,
    stage: CString,
    snapshot: CString,
    live: CString,
    quarantine: CString,
    _lock: File,
    lock_name: CString,
    locked: bool,
    baseline: Option<Snapshot>,
    backups: Option<(Snapshot, Snapshot)>,
    #[cfg(test)]
    before_displace: Option<fn(&Self)>,
    #[cfg(test)]
    before_publish: Option<fn(&Self)>,
    #[cfg(test)]
    before_retire: Option<fn(&Self)>,
    #[cfg(test)]
    after_displace: Option<fn(&Self) -> Result<()>>,
}

fn cstring(s: &std::ffi::OsStr) -> Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| Failure(Reason::UnsafePath).into())
}
fn io_result(value: libc::c_int) -> Result<libc::c_int> {
    if value < 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(value)
    }
}
fn openat(dir: &File, name: &CString, flags: i32) -> Result<File> {
    let fd = io_result(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    })?;
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn private_file(m: &Metadata) -> Result<()> {
    check(
        m.is_file()
            && m.uid() == unsafe { libc::geteuid() }
            && m.nlink() == 1
            && m.mode() & 0o7777 == 0o600,
        Reason::UnsafePath,
    )
}
fn private_directory(m: &Metadata) -> Result<()> {
    check(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o7777 == 0o700,
        Reason::UnsafePath,
    )
}
fn same(a: &Metadata, b: &Metadata, moved: bool) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.mode() == b.mode()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && (moved || (a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()))
}
fn identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn active(shared: &Shared) -> Result<()> {
    check(
        !shared.cancelled.load(Ordering::Acquire),
        Reason::ShutdownTimeout,
    )
}
fn startup_active(shared: &Shared, deadline: Instant) -> Result<()> {
    check(
        !shared.cancelled.load(Ordering::Acquire) && Instant::now() < deadline,
        Reason::StartupTimeout,
    )
}

fn read_at(
    dir: &File,
    name: &CString,
    checkpoint: impl Fn() -> Result<()>,
) -> Result<Option<Snapshot>> {
    checkpoint()?;
    let mut file = match openat(dir, name, libc::O_RDONLY) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.raw_os_error() == Some(libc::ELOOP)) =>
        {
            return Err(Failure(Reason::UnsafePath).into());
        }
        Err(error) => return Err(error),
    };
    checkpoint()?;
    let metadata = file.metadata()?;
    private_file(&metadata)?;
    check(metadata.len() <= MAX_BYTES as u64, Reason::TooLarge)?;
    let mut bytes = Vec::new();
    // Read in bounded chunks, checking cancellation between syscalls. An OS
    // operation already in flight cannot be cancelled portably.
    let mut buffer = [0; 4096];
    loop {
        checkpoint()?;
        let limit = buffer.len().min(MAX_BYTES + 1 - bytes.len());
        if limit == 0 {
            break;
        }
        let n = file.read(&mut buffer[..limit])?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    check(bytes.len() <= MAX_BYTES, Reason::TooLarge)?;
    checkpoint()?;
    check(
        same(&metadata, &file.metadata()?, false),
        Reason::ExternalChange,
    )?;
    // Re-open the name to detect replacement during a read, without following it.
    checkpoint()?;
    let current = openat(dir, name, libc::O_RDONLY)?;
    checkpoint()?;
    check(
        same(&metadata, &current.metadata()?, false),
        Reason::ExternalChange,
    )?;
    Ok(Some(Snapshot { bytes, metadata }))
}

fn matches_snapshot(expected: &Snapshot, current: Option<&Snapshot>, moved: bool) -> bool {
    current
        .is_some_and(|c| c.bytes == expected.bytes && same(&expected.metadata, &c.metadata, moved))
}

#[cfg(target_os = "macos")]
fn no_replace_into(dir: &File, from: &CString, destination: &File, to: &CString) -> Result<()> {
    unsafe extern "C" {
        fn renameatx_np(
            fromfd: i32,
            from: *const libc::c_char,
            tofd: i32,
            to: *const libc::c_char,
            flags: u32,
        ) -> i32;
    }
    io_result(unsafe {
        renameatx_np(
            dir.as_raw_fd(),
            from.as_ptr(),
            destination.as_raw_fd(),
            to.as_ptr(),
            0x00000004,
        )
    })?;
    Ok(())
}
#[cfg(target_os = "linux")]
fn no_replace_into(dir: &File, from: &CString, destination: &File, to: &CString) -> Result<()> {
    io_result(unsafe {
        libc::renameat2(
            dir.as_raw_fd(),
            from.as_ptr(),
            destination.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    })?;
    Ok(())
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn no_replace_into(_: &File, _: &CString, _: &File, _: &CString) -> Result<()> {
    Err(Failure(Reason::UnsafePath).into())
}

fn no_replace(dir: &File, from: &CString, to: &CString) -> Result<()> {
    no_replace_into(dir, from, dir, to)
}

fn entry_exists(dir: &File, name: &CString) -> Result<bool> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(error.into())
    }
}

fn open_directory(path: &Path) -> Result<File> {
    let path = cstring(path.as_os_str())?;
    let fd = io_result(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    })?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl<'a> Store<'a> {
    fn checkpoint(&self) -> Result<()> {
        if let Some(deadline) = self.deadline {
            startup_active(self.shared, deadline)
        } else {
            active(self.shared)
        }
    }

    fn read(&self, name: &CString) -> Result<Option<Snapshot>> {
        read_at(&self.directory, name, || self.checkpoint())
    }

    fn load(path: &Path, shared: &'a Shared, deadline: Instant) -> Result<(Self, Document)> {
        check(path.is_absolute(), Reason::RelativePath)?;
        check(
            path.components().all(|c| {
                !matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }),
            Reason::UnsafePath,
        )?;
        check(path.to_str().is_some_and(safe_text), Reason::UnsafePath)?;
        let parent = path.parent().ok_or(Failure(Reason::UnsafePath))?;
        let root_path = parent
            .parent()
            .ok_or(Failure(Reason::UnsafePath))?
            .to_owned();
        let leaf = cstring(parent.file_name().ok_or(Failure(Reason::UnsafePath))?)?;
        let name = cstring(path.file_name().ok_or(Failure(Reason::UnsafePath))?)?;
        startup_active(shared, deadline)?;
        // Config ancestors may intentionally be symlinks (e.g. Stow). Only a
        // missing, known .config directory may be created, never its ancestors.
        let root = match open_directory(&root_path) {
            Ok(root) => root,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    && root_path.file_name().is_some_and(|n| n == ".config") =>
            {
                let ancestor = root_path.parent().ok_or(Failure(Reason::UnsafePath))?;
                startup_active(shared, deadline)?;
                let ancestor = open_directory(ancestor)?;
                startup_active(shared, deadline)?;
                let metadata = ancestor.metadata()?;
                check(
                    metadata.is_dir()
                        && metadata.uid() == unsafe { libc::geteuid() }
                        && metadata.mode() & 0o022 == 0,
                    Reason::UnsafePath,
                )?;
                startup_active(shared, deadline)?;
                let config = CString::new(".config").unwrap();
                io_result(unsafe { libc::mkdirat(ancestor.as_raw_fd(), config.as_ptr(), 0o700) })?;
                startup_active(shared, deadline)?;
                openat(&ancestor, &config, libc::O_RDONLY | libc::O_DIRECTORY)?
            }
            Err(e) => return Err(e),
        };
        startup_active(shared, deadline)?;
        check(root.metadata()?.is_dir(), Reason::UnsafePath)?;
        startup_active(shared, deadline)?;
        let mkdir = unsafe { libc::mkdirat(root.as_raw_fd(), leaf.as_ptr(), 0o700) };
        if mkdir < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error.into());
            }
        }
        startup_active(shared, deadline)?;
        let directory = openat(&root, &leaf, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(|_| Failure(Reason::UnsafePath))?;
        startup_active(shared, deadline)?;
        private_directory(&directory.metadata()?)?;
        let suffix =
            |suffix: &str| CString::new(format!(".{}.{suffix}", name.to_string_lossy())).unwrap();
        let lock_name = suffix("lock");
        startup_active(shared, deadline)?;
        let lock = openat(&directory, &lock_name, libc::O_RDWR | libc::O_CREAT)?;
        startup_active(shared, deadline)?;
        private_file(&lock.metadata()?)?;
        startup_active(shared, deadline)?;
        let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !locked {
            let e = std::io::Error::last_os_error();
            check(
                e.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || e.raw_os_error() == Some(libc::EAGAIN),
                Reason::IoError,
            )?;
        }
        let mut store = Self {
            shared,
            deadline: Some(deadline),
            root,
            directory,
            root_path,
            leaf,
            stage: suffix("stage"),
            snapshot: suffix("snapshot"),
            live: suffix("live"),
            quarantine: suffix("quarantine"),
            name,
            _lock: lock,
            lock_name,
            locked,
            baseline: None,
            backups: None,
            #[cfg(test)]
            before_displace: None,
            #[cfg(test)]
            before_publish: None,
            #[cfg(test)]
            before_retire: None,
            #[cfg(test)]
            after_displace: None,
        };
        startup_active(shared, deadline)?;
        store.verify_directory()?;
        startup_active(shared, deadline)?;
        if locked {
            check(
                !entry_exists(&store.directory, &store.quarantine)?,
                Reason::RecoveryRequired,
            )?;
        }
        startup_active(shared, deadline)?;
        store.baseline = store.read(&store.name)?;
        if !locked && store.baseline.is_none() {
            startup_active(shared, deadline)?;
            store.baseline = store.read(&store.snapshot)?;
        }
        let document = match &store.baseline {
            Some(b) => Document::decode(&b.bytes)?,
            None => Document::default(),
        };
        startup_active(shared, deadline)?;
        // Another writer may be between displacement and publication. Its
        // snapshot can still be read, but we never inspect/remove its artifacts.
        if locked {
            check(
                store.read(&store.stage)?.is_none(),
                Reason::RecoveryRequired,
            )?;
            startup_active(shared, deadline)?;
            let snapshot = store.read(&store.snapshot)?;
            startup_active(shared, deadline)?;
            let live = store.read(&store.live)?;
            match (snapshot, live) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    check(
                        store.baseline.is_some() && a.bytes == b.bytes,
                        Reason::RecoveryRequired,
                    )?;
                    Document::decode(&a.bytes).map_err(|_| Failure(Reason::RecoveryRequired))?;
                    store.backups = Some((a, b));
                }
                _ => return Err(Failure(Reason::RecoveryRequired).into()),
            }
        }
        startup_active(shared, deadline)?;
        Ok((store, document))
    }

    fn verify_directory(&self) -> Result<()> {
        self.checkpoint()?;
        let named_root = fs::metadata(&self.root_path)?;
        self.checkpoint()?;
        check(
            identity(&named_root, &self.root.metadata()?),
            Reason::ExternalChange,
        )?;
        self.checkpoint()?;
        let named_leaf = openat(&self.root, &self.leaf, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(|_| Failure(Reason::UnsafePath))?;
        self.checkpoint()?;
        let metadata = self.directory.metadata()?;
        private_directory(&metadata)?;
        self.checkpoint()?;
        check(
            identity(&metadata, &named_leaf.metadata()?),
            Reason::ExternalChange,
        )?;
        self.checkpoint()?;
        let named_lock = openat(&self.directory, &self.lock_name, libc::O_RDONLY)
            .map_err(|_| Failure(Reason::ExternalChange))?;
        self.checkpoint()?;
        let named_metadata = named_lock.metadata()?;
        private_file(&named_metadata)?;
        self.checkpoint()?;
        check(
            identity(&named_metadata, &self._lock.metadata()?),
            Reason::ExternalChange,
        )?;
        Ok(())
    }

    fn verify_baseline(&self) -> Result<()> {
        let current = self
            .read(&self.name)
            .map_err(|_| Failure(Reason::ExternalChange))?;
        check(
            match &self.baseline {
                Some(b) => matches_snapshot(b, current.as_ref(), false),
                None => current.is_none(),
            },
            Reason::ExternalChange,
        )
    }

    fn create(&self, name: &CString, bytes: &[u8], shared: &Shared) -> Result<()> {
        active(shared)?;
        let mut file = openat(
            &self.directory,
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )
        .map_err(|_| Failure(Reason::RecoveryRequired))?;
        active(shared)?;
        private_file(&file.metadata()?)?;
        active(shared)?;
        file.write_all(bytes)?;
        active(shared)?;
        file.sync_all()?;
        Ok(())
    }

    fn retire_backups(&mut self, shared: &Shared) -> Result<()> {
        if let Some((a, b)) = &self.backups {
            active(shared)?;
            check(
                matches_snapshot(a, self.read(&self.snapshot)?.as_ref(), false)
                    && matches_snapshot(b, self.read(&self.live)?.as_ref(), false),
                Reason::RecoveryRequired,
            )?;
            #[cfg(test)]
            if let Some(hook) = self.before_retire.take() {
                hook(self);
            }
            active(shared)?;
            // Exclusive mkdir gives this transaction a fresh private namespace.
            // Never unlink the public backup names: first capture their ACTUAL
            // inodes, then verify both before deleting either through this FD.
            io_result(unsafe {
                libc::mkdirat(self.directory.as_raw_fd(), self.quarantine.as_ptr(), 0o700)
            })
            .map_err(|_| Failure(Reason::RecoveryRequired))?;
            active(shared)?;
            let quarantine = openat(
                &self.directory,
                &self.quarantine,
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?;
            active(shared)?;
            private_directory(&quarantine.metadata()?)?;
            for name in [&self.snapshot, &self.live] {
                active(shared)?;
                no_replace_into(&self.directory, name, &quarantine, name)
                    .map_err(|_| Failure(Reason::RecoveryRequired))?;
            }
            for (name, expected) in [(&self.snapshot, a), (&self.live, b)] {
                let actual = read_at(&quarantine, name, || active(shared))
                    .map_err(|_| Failure(Reason::RecoveryRequired))?;
                check(
                    matches_snapshot(expected, actual.as_ref(), true),
                    Reason::ExternalChange,
                )?;
            }
            for name in [&self.snapshot, &self.live] {
                active(shared)?;
                io_result(unsafe { libc::unlinkat(quarantine.as_raw_fd(), name.as_ptr(), 0) })?;
            }
            active(shared)?;
            io_result(unsafe {
                libc::unlinkat(
                    self.directory.as_raw_fd(),
                    self.quarantine.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            })?;
            self.backups = None;
        }
        Ok(())
    }

    fn save(&mut self, bytes: &[u8], shared: &Shared) -> Result<()> {
        active(shared)?;
        self.verify_directory()?;
        active(shared)?;
        self.verify_baseline()?;
        active(shared)?;
        check(
            !entry_exists(&self.directory, &self.quarantine)?
                && !entry_exists(&self.directory, &self.stage)?,
            Reason::RecoveryRequired,
        )?;
        self.retire_backups(shared)?;
        active(shared)?;
        check(
            self.read(&self.stage)?.is_none()
                && self.read(&self.snapshot)?.is_none()
                && self.read(&self.live)?.is_none(),
            Reason::RecoveryRequired,
        )?;
        self.create(&self.stage, bytes, shared)?;
        let mut displaced = false;
        let mut published = false;
        let result = (|| {
            if let Some(baseline) = &self.baseline {
                self.create(&self.snapshot, &baseline.bytes, shared)?;
                active(shared)?;
                self.directory.sync_all()?;
                active(shared)?;
                self.verify_directory()?;
                self.verify_baseline()?;
                active(shared)?;
                #[cfg(test)]
                if let Some(hook) = self.before_displace.take() {
                    hook(self);
                }
                no_replace(&self.directory, &self.name, &self.live)
                    .map_err(|_| Failure(Reason::ExternalChange))?;
                displaced = true;
                #[cfg(test)]
                if let Some(hook) = self.after_displace.take() {
                    hook(self)?;
                }
                active(shared)?;
                let actual = self.read(&self.live)?;
                check(
                    matches_snapshot(baseline, actual.as_ref(), true),
                    Reason::ExternalChange,
                )?;
                active(shared)?;
                self.directory.sync_all()?;
            }
            active(shared)?;
            self.verify_directory()?;
            #[cfg(test)]
            if let Some(hook) = self.before_publish.take() {
                hook(self);
            }
            active(shared)?;
            no_replace(&self.directory, &self.stage, &self.name)
                .map_err(|_| Failure(Reason::ExternalChange))?;
            published = true;
            active(shared)?;
            self.directory.sync_all()?;
            active(shared)?;
            let current = self
                .read(&self.name)?
                .ok_or(Failure(Reason::ExternalChange))?;
            check(current.bytes == bytes, Reason::ExternalChange)?;
            if self.baseline.is_some() {
                active(shared)?;
                let a = self
                    .read(&self.snapshot)?
                    .ok_or(Failure(Reason::RecoveryRequired))?;
                let b = self
                    .read(&self.live)?
                    .ok_or(Failure(Reason::RecoveryRequired))?;
                check(a.bytes == b.bytes, Reason::ExternalChange)?;
                self.backups = Some((a, b));
            }
            self.baseline = Some(current);
            Ok(())
        })();
        if result.is_err() && displaced && !published {
            // Rollback deliberately ignores cancellation and does not read the
            // displaced node. Even a symlink/oversized file must be restored if
            // possible. This is worker-only; Drop never joins a stalled syscall.
            if no_replace(&self.directory, &self.live, &self.name).is_ok() {
                let _ = self.directory.sync_all();
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn document(n: usize) -> Document {
        Document {
            version: 1,
            records: (0..n)
                .map(|i| Record {
                    source: "test".into(),
                    activity: format!("task-{i:03}"),
                    settings: FamiliarSettings::default(),
                    placement: Some(Placement {
                        x: i as f64,
                        y: -12.0,
                    }),
                })
                .collect(),
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("numen/preferences.json");
        (dir, path)
    }
    fn write_private(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    fn initialized() -> (tempfile::TempDir, PathBuf) {
        let (dir, path) = fixture();
        let (_, writer) = start(path.clone(), |_| {});
        assert!(writer.can_write(), "{:?}", writer.status());
        drop(writer);
        (dir, path)
    }
    fn submit(writer: &Writer, doc: Document, revision: u64) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !writer.submit(doc.clone(), revision) {
            assert!(Instant::now() < deadline, "{:?}", writer.status());
            thread::yield_now();
        }
    }

    #[test]
    fn schema_is_strict_bounded_and_deterministic() {
        let mut doc = document(MAX_RECORDS);
        doc.records.reverse();
        let bytes = doc.encode().unwrap();
        assert_eq!(Document::decode(&bytes).unwrap().encode().unwrap(), bytes);
        assert_eq!(
            Document::default().encode().unwrap(),
            br#"{"version":1,"records":[]}"#
        );
        assert!(document(MAX_RECORDS + 1).encode().is_err());
        doc.records[1] = doc.records[0].clone();
        assert!(doc.validate().is_err());
        for bytes in [
            br#"{"version":1,"version":1,"records":[]}"#.as_slice(),
            br#"{"version":2,"records":[]}"#,
            br#"{"version":1,"records":[],"instances":[]}"#,
            br#"{"version":1,"records":[]} {}"#,
            br#"{"records":[]}"#,
        ] {
            assert!(Document::decode(bytes).is_err());
        }
        let record = serde_json::to_string(&document(1).records[0]).unwrap();
        for bad in [
            record.replace(
                "\"font_size\":12.0",
                "\"font_size\":12.0,\"font_size\":12.0",
            ),
            record.replace("\"font_size\":12.0", "\"font_size\":12.0,\"pause\":false"),
            record.replace("\"font_size\":12.0,", ""),
            record.replace("\"font_size\":12.0", "\"font_size\":null"),
            record.replace("\"font_size\":12.0", "\"font_size\":99.0"),
            record.replace("\"source\":\"test\"", "\"source\":\"bad/id\""),
            record.replace(
                "\"source\":\"test\"",
                "\"source\":\"test\",\"source\":\"test\"",
            ),
            record.replace("\"x\":0.0", "\"x\":0.0,\"x\":0.0"),
            record.replace("\"x\":0.0", "\"x\":1000000.1"),
            record.replace("\"x\":0.0", "\"x\":0.0,\"monitor\":1"),
            record.replace(
                "\"source\":\"test\"",
                "\"source\":\"test\",\"instance\":\"x\"",
            ),
        ] {
            let json = format!("{{\"version\":1,\"records\":[{bad}]}}");
            assert!(Document::decode(json.as_bytes()).is_err(), "{json}");
        }
        assert!(Document::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1_000_000.1] {
            assert!(Placement { x: value, y: 0.0 }.validate().is_err());
        }
        Placement {
            x: -1_000_000.0,
            y: 1_000_000.0,
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn paths_are_injected_and_status_is_bounded() {
        assert!(default_path(None, None).is_err());
        assert!(default_path(Some("relative".into()), Some("/tmp".into())).is_err());
        assert!(default_path(None, Some("relative".into())).is_err());
        assert_eq!(
            default_path(Some("".into()), Some("/injected".into())).unwrap(),
            PathBuf::from("/injected/.config/numen/preferences.json")
        );
        assert_eq!(
            default_path(Some("/config".into()), None).unwrap(),
            PathBuf::from("/config/numen/preferences.json")
        );
        assert!(Status::disabled().valid());
        assert!(Status::unavailable(Reason::NoHome).valid());
        let mut s = Status::unavailable(Reason::UnsafePath);
        for path in [
            "relative",
            "/bad\npath",
            "/bad\u{202e}path",
            "/bad\u{061c}path",
            "/bad\u{200e}path",
            "/bad\u{200f}path",
            &format!("/{}", "x".repeat(1024)),
        ] {
            s.path = Some(path.into());
            assert!(!s.valid());
        }
        s.path = None;
        s.durable_revision = 1;
        assert!(!s.valid());
        s.durable_revision = 0;
        s.records = MAX_RECORDS + 1;
        assert!(!s.valid());
        let s = serde_json::to_value(Status::disabled()).unwrap();
        assert_eq!(s["mode"], "disabled");
        assert_eq!(s["load"], "not_loaded");
        assert_eq!(serde_json::to_value(Mode::ReadOnly).unwrap(), "read_only");
        assert_eq!(
            error_reason(&default_path(None, None).unwrap_err()),
            Reason::NoHome
        );
        assert_eq!(
            error_reason(&default_path(Some("relative".into()), None).unwrap_err()),
            Reason::RelativePath
        );
    }

    #[test]
    fn absent_does_not_create_json_and_normal_reopen_retains_bounded_backups() {
        let (_dir, path) = fixture();
        let (doc, writer) = start(path.clone(), |_| {});
        assert!(doc.records.is_empty());
        assert_eq!(writer.status().load, LoadState::Absent);
        assert!(writer.status().valid());
        assert!(!path.exists());
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o7777,
            0o700
        );
        for revision in 1..=4 {
            submit(&writer, document(revision), revision as u64);
            assert_eq!(writer.status().accepted_revision, revision as u64);
            assert!(
                writer.flush(Duration::from_secs(2)),
                "{:?}",
                writer.status()
            );
        }
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 4);
        drop(writer);
        let (doc, writer) = start(path.clone(), |_| {});
        assert_eq!(doc.records.len(), 4);
        assert_eq!(writer.status().load, LoadState::Loaded);
        assert!(writer.can_write(), "{:?}", writer.status());
        submit(&writer, document(2), 1);
        assert!(
            writer.flush(Duration::from_secs(2)),
            "{:?}",
            writer.status()
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 4);
    }

    #[test]
    fn second_writer_reads_but_cannot_write() {
        let (_dir, path) = fixture();
        let (_, first) = start(path.clone(), |_| {});
        submit(&first, document(2), 1);
        assert!(first.flush(Duration::from_secs(2)));
        let (doc, second) = start(path.clone(), |_| {});
        assert_eq!(doc.records.len(), 2);
        assert!(!second.can_write());
        assert_eq!(second.status().reason, Some(Reason::LockBusy));
        assert!(second.status().valid());
        assert!(!second.submit(document(0), 1));
        drop(second);
        submit(&first, document(3), 2);
        assert!(first.flush(Duration::from_secs(2)));
    }

    #[test]
    fn malformed_unknown_version_and_oversize_are_never_overwritten() {
        for (bytes, expected) in [
            (b"not json".to_vec(), Reason::InvalidData),
            (
                br#"{"version":2,"records":[]}"#.to_vec(),
                Reason::UnsupportedVersion,
            ),
            (vec![b' '; MAX_BYTES + 1], Reason::TooLarge),
        ] {
            let (_dir, path) = initialized();
            write_private(&path, &bytes);
            let (doc, writer) = start(path.clone(), |_| {});
            assert!(doc.records.is_empty());
            assert_eq!(writer.status().reason, Some(expected));
            assert_eq!(writer.status().load, LoadState::Rejected);
            assert!(writer.status().valid());
            assert!(!writer.submit(document(1), 1));
            drop(writer);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn unsafe_files_leaf_and_fifo_are_rejected_without_blocking() {
        for kind in ["symlink", "hardlink", "permissions", "fifo"] {
            let (dir, path) = initialized();
            let target = dir.path().join("untouched");
            write_private(&target, &document(1).encode().unwrap());
            match kind {
                "symlink" => symlink(&target, &path).unwrap(),
                "hardlink" => fs::hard_link(&target, &path).unwrap(),
                "permissions" => {
                    write_private(&path, &document(1).encode().unwrap());
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                "fifo" => {
                    let p = cstring(path.as_os_str()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(p.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            let before = Instant::now();
            let (_, writer) = start(path.clone(), |_| {});
            assert!(before.elapsed() < Duration::from_millis(400));
            assert!(!writer.can_write(), "{kind}");
            assert_eq!(writer.status().reason, Some(Reason::UnsafePath), "{kind}");
            assert_eq!(fs::read(target).unwrap(), document(1).encode().unwrap());
        }
        let (dir, path) = fixture();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&target, path.parent().unwrap()).unwrap();
        let (_, writer) = start(path, |_| {});
        assert!(!writer.can_write());
        assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    }

    #[test]
    fn config_symlink_allowed_but_missing_ancestors_are_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("config-link");
        symlink(&real, &link).unwrap();
        let (_, writer) = start(link.join("numen/preferences.json"), |_| {});
        assert!(writer.can_write(), "{:?}", writer.status());
        drop(writer);
        let missing = dir.path().join("missing/ancestor/numen/preferences.json");
        let (_, writer) = start(missing, |_| {});
        assert!(!writer.can_write());
        assert!(!dir.path().join("missing").exists());
        drop(writer);
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let (_, writer) = start(
            default_path(None, Some(home.clone().into_os_string())).unwrap(),
            |_| {},
        );
        assert!(writer.can_write(), "{:?}", writer.status());
        assert!(home.join(".config/numen").is_dir());
        assert!(!home.join(".config/numen/preferences.json").exists());
    }

    #[test]
    fn external_replacement_and_inplace_edits_are_preserved() {
        for replace in [false, true] {
            let (_dir, path) = fixture();
            let (_, writer) = start(path.clone(), |_| {});
            submit(&writer, document(1), 1);
            assert!(writer.flush(Duration::from_secs(2)));
            let external = document(3).encode().unwrap();
            if replace {
                let temp = path.with_extension("external");
                write_private(&temp, &external);
                fs::rename(temp, &path).unwrap();
            } else {
                fs::write(&path, &external).unwrap();
            }
            submit(&writer, document(2), 2);
            assert!(!writer.flush(Duration::from_secs(2)));
            assert_eq!(writer.status().reason, Some(Reason::ExternalChange));
            assert_eq!(fs::read(&path).unwrap(), external);
            assert!(!writer.can_write());
        }
    }

    #[test]
    fn modified_backups_and_unexpected_stages_require_recovery() {
        let (_dir, path) = fixture();
        let (_, writer) = start(path.clone(), |_| {});
        for revision in 1..=2 {
            submit(&writer, document(revision), revision as u64);
            assert!(writer.flush(Duration::from_secs(2)));
        }
        let live = path.parent().unwrap().join(".preferences.json.live");
        fs::write(&live, b"external old-fd edit").unwrap();
        submit(&writer, document(3), 3);
        assert!(!writer.flush(Duration::from_secs(2)));
        assert_eq!(writer.status().reason, Some(Reason::RecoveryRequired));
        assert_eq!(fs::read(&live).unwrap(), b"external old-fd edit");
        drop(writer);
        let (_, writer) = start(path.clone(), |_| {});
        assert!(!writer.can_write());
        assert_eq!(writer.status().reason, Some(Reason::RecoveryRequired));
        drop(writer);
        let (_dir, path) = initialized();
        write_private(
            &path.parent().unwrap().join(".preferences.json.stage"),
            b"keep me",
        );
        let (_, writer) = start(path, |_| {});
        assert_eq!(writer.status().reason, Some(Reason::RecoveryRequired));
    }

    #[test]
    fn no_replace_and_rollback_never_clobber_existing_names() {
        let dir = tempfile::tempdir().unwrap();
        let fd = File::open(dir.path()).unwrap();
        let from = CString::new("from").unwrap();
        let to = CString::new("to").unwrap();
        write_private(&dir.path().join("from"), b"original");
        write_private(&dir.path().join("to"), b"external");
        assert!(no_replace(&fd, &from, &to).is_err());
        assert_eq!(fs::read(dir.path().join("from")).unwrap(), b"original");
        assert_eq!(fs::read(dir.path().join("to")).unwrap(), b"external");
        fs::remove_file(dir.path().join("to")).unwrap();
        no_replace(&fd, &from, &to).unwrap();
        assert!(!dir.path().join("from").exists());
        assert_eq!(fs::read(dir.path().join("to")).unwrap(), b"original");
    }

    #[test]
    fn latest_only_capacity_recovery_and_drop_flush() {
        let (_dir, path) = fixture();
        let (_, writer) = start(path.clone(), |_| {});
        writer.note_capacity();
        assert_eq!(writer.status().reason, Some(Reason::CapacityReached));
        assert!(writer.can_write());
        assert!(!writer.submit(document(MAX_RECORDS + 1), 1));
        for revision in 1..=100 {
            submit(&writer, document(revision % 4), revision as u64);
        }
        assert_eq!(writer.status().accepted_revision, 100);
        assert_eq!(writer.status().reason, None);
        assert!(!writer.submit(document(2), 99));
        let state = writer.shared.state.lock().unwrap();
        assert_eq!(state.pending.as_ref().unwrap().revision, 100);
        drop(state);
        drop(writer);
        assert_eq!(
            Document::decode(&fs::read(path).unwrap())
                .unwrap()
                .records
                .len(),
            0
        );
    }

    #[test]
    fn inflight_keeps_only_the_latest_pending_and_accepts_before_io() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let e = entered.clone();
        let r = release.clone();
        let saves = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = saves.clone();
        let (_dir, path) = fixture();
        let (_, writer) = start(path.clone(), move |status| {
            if status.write == WriteState::Saving && count.fetch_add(1, Ordering::Relaxed) == 0 {
                *e.0.lock().unwrap() = true;
                e.1.notify_all();
                let mut released = r.0.lock().unwrap();
                while !*released {
                    released = r.1.wait(released).unwrap();
                }
            }
        });
        submit(&writer, document(1), 1);
        let mut seen = entered.0.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !*seen {
            assert!(Instant::now() < deadline);
            seen = entered
                .1
                .wait_timeout(seen, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        drop(seen);
        for revision in 2..=100 {
            submit(&writer, document(revision % 4), revision as u64);
        }
        assert_eq!(writer.status().accepted_revision, 100);
        assert_eq!(writer.status().durable_revision, 0);
        assert_eq!(
            writer
                .shared
                .state
                .lock()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .revision,
            100
        );
        assert!(!writer.flush(Duration::from_millis(10)));
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        assert!(
            writer.flush(Duration::from_secs(2)),
            "{:?}",
            writer.status()
        );
        assert_eq!(writer.status().durable_revision, 100);
        assert_eq!(saves.load(Ordering::Relaxed), 2);
        assert!(
            Document::decode(&fs::read(path).unwrap())
                .unwrap()
                .records
                .is_empty()
        );
    }

    #[test]
    fn debounce_is_not_postponed_indefinitely_by_continuous_edits() {
        let (_dir, path) = fixture();
        let (_, writer) = start(path.clone(), |_| {});
        submit(&writer, document(1), 1);
        thread::sleep(Duration::from_millis(80));
        assert!(!path.exists());
        let started = Instant::now();
        let mut revision = 1;
        while started.elapsed() < Duration::from_millis(2400) {
            revision += 1;
            submit(&writer, document(1), revision);
            thread::sleep(Duration::from_millis(30));
        }
        assert!(
            writer.status().durable_revision > 0,
            "{:?}",
            writer.status()
        );
        assert!(writer.status().durable_revision < revision);
        assert!(writer.flush(Duration::from_secs(2)));
        assert_eq!(writer.status().durable_revision, revision);
    }

    fn shared() -> Shared {
        Shared {
            state: Mutex::new(State {
                status: Status::unavailable(Reason::IoError),
                pending: None,
                first_dirty: None,
                flush_target: None,
                capacity: false,
                stop: false,
                ended: false,
                startup: None,
            }),
            notified: Mutex::new(None),
            wake: Condvar::new(),
            cancelled: AtomicBool::new(false),
            callback: Box::new(|_| {}),
        }
    }

    fn store_path(store: &Store<'_>) -> PathBuf {
        store
            .root_path
            .join(store.leaf.to_str().unwrap())
            .join(store.name.to_str().unwrap())
    }

    #[test]
    fn publication_races_preserve_actual_external_inode_and_never_clobber() {
        for phase in ["before_displace", "before_publish", "absent"] {
            let (_dir, path) = fixture();
            let shared = shared();
            let (mut store, _) =
                Store::load(&path, &shared, Instant::now() + Duration::from_secs(2)).unwrap();
            store.deadline = None;
            if phase != "absent" {
                store.save(&document(1).encode().unwrap(), &shared).unwrap();
            }
            match phase {
                "before_displace" => {
                    store.before_displace = Some(|s| {
                        let path = store_path(s);
                        let replacement = path.with_extension("replacement");
                        write_private(&replacement, b"actual external inode");
                        fs::rename(replacement, path).unwrap();
                    })
                }
                _ => {
                    store.before_publish =
                        Some(|s| write_private(&store_path(s), b"actual external inode"))
                }
            }
            let error = store
                .save(&document(2).encode().unwrap(), &shared)
                .unwrap_err();
            assert_eq!(reason(&error), Reason::ExternalChange);
            assert_eq!(
                fs::read(&path).unwrap(),
                b"actual external inode",
                "{phase}"
            );
            assert_eq!(
                fs::read(path.parent().unwrap().join(".preferences.json.stage")).unwrap(),
                document(2).encode().unwrap()
            );
            if phase == "before_publish" {
                assert_eq!(
                    fs::read(path.parent().unwrap().join(".preferences.json.live")).unwrap(),
                    document(1).encode().unwrap()
                );
                assert_eq!(
                    fs::read(path.parent().unwrap().join(".preferences.json.snapshot")).unwrap(),
                    document(1).encode().unwrap()
                );
            }
        }
    }

    #[test]
    fn retirement_captures_and_preserves_backup_replaced_after_validation() {
        for replace_snapshot in [false, true] {
            let (_dir, path) = fixture();
            let shared = shared();
            let (mut store, _) =
                Store::load(&path, &shared, Instant::now() + Duration::from_secs(2)).unwrap();
            store.deadline = None;
            for n in 1..=2 {
                store.save(&document(n).encode().unwrap(), &shared).unwrap();
            }
            fn replace(store: &Store<'_>, snapshot: bool) {
                let name = if snapshot {
                    &store.snapshot
                } else {
                    &store.live
                };
                let path = store_path(store).with_file_name(name.to_str().unwrap());
                let replacement = path.with_extension("external");
                write_private(&replacement, b"external replacement must survive");
                fs::rename(replacement, path).unwrap();
            }
            store.before_retire = Some(if replace_snapshot {
                |s| replace(s, true)
            } else {
                |s| replace(s, false)
            });
            let error = store
                .save(&document(3).encode().unwrap(), &shared)
                .unwrap_err();
            assert_eq!(error_reason(&error), Reason::ExternalChange);
            let name = if replace_snapshot {
                &store.snapshot
            } else {
                &store.live
            };
            let quarantine = path
                .parent()
                .unwrap()
                .join(store.quarantine.to_str().unwrap());
            assert_eq!(
                fs::read(quarantine.join(name.to_str().unwrap())).unwrap(),
                b"external replacement must survive"
            );
            assert_eq!(fs::read_dir(&quarantine).unwrap().count(), 2);
            assert_eq!(fs::read(&path).unwrap(), document(2).encode().unwrap());
            drop(store);
            let (_, writer) = start(path, |_| {});
            assert_eq!(writer.status().reason, Some(Reason::RecoveryRequired));
            assert!(!writer.can_write());
        }
    }

    #[test]
    fn all_prepublication_failures_restore_displaced_node_without_reading_it() {
        for failure in ["oversize", "symlink", "cancel", "io", "cancel_recreated"] {
            let (_dir, path) = fixture();
            let shared = shared();
            let (mut store, _) =
                Store::load(&path, &shared, Instant::now() + Duration::from_secs(2)).unwrap();
            store.deadline = None;
            store.save(&document(1).encode().unwrap(), &shared).unwrap();
            match failure {
                "oversize" => {
                    store.before_displace = Some(|s| {
                        let path = store_path(s);
                        let replacement = path.with_extension("external");
                        write_private(&replacement, &vec![b'x'; MAX_BYTES + 1]);
                        fs::rename(replacement, path).unwrap();
                    })
                }
                "symlink" => {
                    store.before_displace = Some(|s| {
                        let path = store_path(s);
                        let replacement = path.with_extension("external");
                        symlink("do-not-follow", &replacement).unwrap();
                        fs::rename(replacement, path).unwrap();
                    })
                }
                "io" => store.after_displace = Some(|_| Err(Failure(Reason::IoError).into())),
                "cancel_recreated" => {
                    store.after_displace = Some(|s| {
                        write_private(&store_path(s), b"recreated main wins");
                        s.shared.cancelled.store(true, Ordering::Release);
                        Ok(())
                    })
                }
                _ => {
                    store.after_displace = Some(|s| {
                        s.shared.cancelled.store(true, Ordering::Release);
                        Ok(())
                    })
                }
            }
            let error = store
                .save(&document(2).encode().unwrap(), &shared)
                .unwrap_err();
            assert_eq!(
                error_reason(&error),
                match failure {
                    "oversize" => Reason::TooLarge,
                    "symlink" => Reason::UnsafePath,
                    "io" => Reason::IoError,
                    _ => Reason::ShutdownTimeout,
                }
            );
            match failure {
                "oversize" => assert_eq!(fs::read(&path).unwrap(), vec![b'x'; MAX_BYTES + 1]),
                "symlink" => assert_eq!(
                    fs::read_link(&path).unwrap(),
                    PathBuf::from("do-not-follow")
                ),
                "cancel_recreated" => {
                    assert_eq!(fs::read(&path).unwrap(), b"recreated main wins");
                    assert_eq!(
                        fs::read(path.parent().unwrap().join(".preferences.json.live")).unwrap(),
                        document(1).encode().unwrap()
                    );
                }
                _ => assert_eq!(fs::read(&path).unwrap(), document(1).encode().unwrap()),
            }
            assert_eq!(
                fs::read(path.parent().unwrap().join(".preferences.json.stage")).unwrap(),
                document(2).encode().unwrap()
            );
            assert_eq!(
                fs::read(path.parent().unwrap().join(".preferences.json.snapshot")).unwrap(),
                document(1).encode().unwrap()
            );
        }
    }

    #[test]
    fn clean_flush_does_not_disable_debounce_for_future_revisions() {
        let (_dir, path) = fixture();
        let (_, writer) = start(path.clone(), |_| {});
        assert!(writer.flush(Duration::ZERO));
        assert!(writer.shared.state.lock().unwrap().flush_target.is_none());
        submit(&writer, document(1), 1);
        thread::sleep(Duration::from_millis(80));
        assert!(!path.exists());
        assert!(writer.flush(Duration::from_secs(2)));
        assert!(writer.shared.state.lock().unwrap().flush_target.is_none());
    }

    #[test]
    fn inflight_flush_clears_satisfied_target_and_capacity_warning_survives_commit() {
        for capacity in [false, true] {
            let entered = Arc::new((Mutex::new(false), Condvar::new()));
            let release = Arc::new((Mutex::new(false), Condvar::new()));
            let e = entered.clone();
            let r = release.clone();
            let (_dir, path) = fixture();
            let (_, writer) = start(path.clone(), move |status| {
                if status.write == WriteState::Saving && status.accepted_revision == 1 {
                    *e.0.lock().unwrap() = true;
                    e.1.notify_all();
                    let mut released = r.0.lock().unwrap();
                    while !*released {
                        released = r.1.wait(released).unwrap();
                    }
                }
            });
            submit(&writer, document(1), 1);
            let mut seen = entered.0.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !*seen {
                assert!(Instant::now() < deadline);
                seen = entered
                    .1
                    .wait_timeout(seen, Duration::from_millis(10))
                    .unwrap()
                    .0;
            }
            drop(seen);
            assert!(!writer.flush(Duration::ZERO));
            assert_eq!(writer.shared.state.lock().unwrap().flush_target, Some(1));
            if capacity {
                writer.note_capacity();
            }
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            assert_eq!(writer.flush(Duration::from_secs(2)), !capacity);
            assert_eq!(writer.status().durable_revision, 1);
            assert!(writer.shared.state.lock().unwrap().flush_target.is_none());
            if capacity {
                assert_eq!(writer.status().reason, Some(Reason::CapacityReached));
                assert_eq!(writer.status().write, WriteState::Blocked);
                assert!(writer.status().valid());
                assert!(writer.can_write());
                assert!(!writer.flush(Duration::ZERO));
            }
            submit(&writer, document(2), 2);
            assert_eq!(writer.status().reason, None);
            thread::sleep(Duration::from_millis(80));
            assert_eq!(
                Document::decode(&fs::read(&path).unwrap())
                    .unwrap()
                    .records
                    .len(),
                1
            );
            assert!(writer.flush(Duration::from_secs(2)));
            assert_eq!(writer.status().write, WriteState::Clean);
            writer.note_capacity();
            assert!(!writer.flush(Duration::ZERO));
            assert_eq!(writer.status().write, WriteState::Blocked);
        }
    }

    #[test]
    fn directory_or_lock_replacement_blocks_publication() {
        for replace_lock in [false, true] {
            let (_dir, path) = fixture();
            let (_, writer) = start(path.clone(), |_| {});
            let leaf = path.parent().unwrap();
            if replace_lock {
                let lock = leaf.join(".preferences.json.lock");
                fs::remove_file(&lock).unwrap();
                write_private(&lock, b"");
            } else {
                fs::rename(leaf, leaf.with_extension("old")).unwrap();
                fs::create_dir(leaf).unwrap();
                fs::set_permissions(leaf, fs::Permissions::from_mode(0o700)).unwrap();
            }
            submit(&writer, document(1), 1);
            assert!(!writer.flush(Duration::from_secs(2)));
            assert_eq!(writer.status().reason, Some(Reason::ExternalChange));
            assert!(!path.exists());
        }
    }

    #[test]
    fn expired_or_cancelled_load_never_creates_or_applies_data() {
        let (_dir, path) = fixture();
        let shared = shared();
        let error = Store::load(&path, &shared, Instant::now() - Duration::from_millis(1))
            .err()
            .unwrap();
        assert_eq!(reason(&error), Reason::StartupTimeout);
        assert!(!path.parent().unwrap().exists());
        shared.cancelled.store(true, Ordering::Release);
        assert!(Store::load(&path, &shared, Instant::now() + Duration::from_secs(2)).is_err());
        assert!(!path.parent().unwrap().exists());
        let shared = Arc::new(shared);
        run(
            shared.clone(),
            path.clone(),
            Instant::now() - Duration::from_millis(1),
        );
        let state = shared.state.lock().unwrap();
        assert!(state.ended);
        assert!(state.startup.is_none());
        assert_eq!(state.status.load, LoadState::TimedOut);
        assert_eq!(state.status.reason, Some(Reason::StartupTimeout));
        assert!(state.status.valid());
        assert!(!path.exists());
    }

    #[test]
    fn fifo_config_root_and_unsafe_leaf_permissions_do_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let name = cstring(fifo.as_os_str()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let before = Instant::now();
        let (_, writer) = start(fifo.join("numen/preferences.json"), |_| {});
        assert!(!writer.can_write());
        drop(writer);
        assert!(before.elapsed() < Duration::from_millis(400));
        let (_dir, path) = initialized();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        let (_, writer) = start(path, |_| {});
        assert_eq!(writer.status().reason, Some(Reason::UnsafePath));
    }

    #[test]
    fn callbacks_hold_no_state_mutex_and_shutdown_is_bounded() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let e = entered.clone();
        let r = release.clone();
        let (_dir, path) = fixture();
        let (_, writer) = start(path, move |_| {
            *e.0.lock().unwrap() = true;
            e.1.notify_all();
            let mut released = r.0.lock().unwrap();
            while !*released {
                released = r.1.wait(released).unwrap();
            }
        });
        let mut seen = entered.0.lock().unwrap();
        while !*seen {
            seen = entered
                .1
                .wait_timeout(seen, Duration::from_secs(2))
                .unwrap()
                .0;
        }
        drop(seen);
        submit(&writer, document(1), 1);
        assert!(!writer.flush(Duration::from_millis(10)));
        let shared = writer.shared.clone();
        let before = Instant::now();
        drop(writer);
        assert!(before.elapsed() < Duration::from_millis(650));
        assert_eq!(
            shared.state.lock().unwrap().status.reason,
            Some(Reason::ShutdownTimeout)
        );
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut state = shared.state.lock().unwrap();
        while !state.ended {
            assert!(Instant::now() < deadline);
            state = shared
                .wake
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        assert_eq!(state.status.reason, Some(Reason::ShutdownTimeout));
    }
}
