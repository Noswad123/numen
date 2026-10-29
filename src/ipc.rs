use std::{
    fs::{self, DirBuilder, Metadata},
    io::{self, Read, Write},
    mem,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::json;

use crate::{
    activity::{
        AttachmentBinding, FamiliarPresentation, FocusTarget, LiveTarget, PreferenceBinding,
        Presentation, SourceId, Store,
    },
    diagnostics::RuntimeDiagnostics,
    preferences::{self, Placement},
    protocol::{
        self, Activity, ActivityState, Event, FamiliarSettings, MAX_LINE, MAX_SOURCES, Request,
        SettingsPatch,
    },
};

const TICK: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(2);
const EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 16 * 1024;
const MESSAGES_PER_TICK: usize = 8;
type TargetedReveal = dyn Fn(Option<Vec<String>>) + Send;
type GoToMouse = dyn Fn(Vec<String>) + Send;

#[derive(Default)]
struct TargetedControls {
    reveal: Option<Box<TargetedReveal>>,
    go_to_mouse: Option<Box<GoToMouse>>,
    quit: Option<Box<dyn Fn() + Send>>,
}

struct WorkerConfig {
    target_memo: TargetCache,
    preferences: Option<(preferences::Document, preferences::Writer)>,
    diagnostics: RuntimeDiagnostics,
    settings_support: bool,
    defaults: FamiliarSettings,
    targeted: TargetedControls,
    bootstrap_main: bool,
    service_instance: String,
}

struct ControlContext {
    diagnostics: RuntimeDiagnostics,
    settings_support: bool,
    targeted: TargetedControls,
    multi: bool,
    service_instance: String,
}

fn new_service_instance() -> Result<String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let instance = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    );
    protocol::validate_id(&instance)?;
    Ok(instance)
}
// Leave bounded control capacity even when all producer slots are occupied.
const MAX_CLIENTS: usize = MAX_SOURCES + 8;

fn uid() -> u32 {
    // geteuid has no preconditions and does not mutate process state.
    unsafe { libc::geteuid() }
}

pub fn default_socket_path() -> Result<PathBuf> {
    let parent = match std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("numen"),
        None => std::env::temp_dir().join(format!("numen-{}", uid())),
    };
    let path = parent.join("events.sock");
    check_path(&path)?;
    Ok(path)
}

fn check_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().as_bytes().contains(&0),
        "socket path contains NUL"
    );
    ensure!(
        path.as_os_str().as_bytes().len() <= 100,
        "socket path exceeds 100 bytes; choose a shorter private path with --socket"
    );
    ensure!(
        path.file_name().is_some(),
        "socket path must name a socket file"
    );
    Ok(())
}

/// Validate or create only the private parent directory, without touching `path`.
pub fn prepare_socket_parent(path: &Path) -> Result<()> {
    private_parent(path, true)
}

fn private_parent(path: &Path, create: bool) -> Result<()> {
    check_path(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if create {
        match DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "create private socket directory {}; its ancestor must already exist",
                        parent.display()
                    )
                });
            }
        }
    }
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("inspect socket directory {}", parent.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "socket parent must be a directory, not a symlink"
    );
    ensure!(
        metadata.uid() == uid(),
        "socket parent must be owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "socket parent must be private (mode 0700); refusing to chmod an existing directory"
    );
    Ok(())
}

fn socket_metadata(path: &Path) -> Result<Metadata> {
    let m =
        fs::symlink_metadata(path).with_context(|| format!("inspect socket {}", path.display()))?;
    ensure!(
        m.file_type().is_socket() && !m.file_type().is_symlink(),
        "socket path is not a Unix socket (symlinks are forbidden)"
    );
    ensure!(m.uid() == uid(), "socket is owned by another user");
    Ok(m)
}

struct SocketOwner {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketOwner {
    fn new(path: &Path, m: &Metadata) -> Self {
        Self {
            path: path.into(),
            dev: m.dev(),
            ino: m.ino(),
        }
    }
    fn matches(&self) -> bool {
        fs::symlink_metadata(&self.path).is_ok_and(|m| {
            m.file_type().is_socket()
                && m.uid() == uid()
                && m.dev() == self.dev
                && m.ino() == self.ino
        })
    }
}

impl Drop for SocketOwner {
    fn drop(&mut self) {
        if self.matches() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// Use nonblocking connect plus poll so even a full Unix accept backlog cannot
// hang the GUI or synchronous publisher indefinitely.
fn connect_bounded(path: &Path) -> io::Result<UnixStream> {
    check_path(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    // socket returns a new descriptor, transferred immediately into OwnedFd.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // Raw sockets bypass std's socket constructor. Suppress SIGPIPE on
        // Apple platforms without changing the process-wide signal handler.
        let enabled: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const libc::c_int).cast(),
                mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // This descriptor is owned here, and fcntl arguments match their commands.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // Zero is a valid initial representation for sockaddr_un; fill its address.
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let length = mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        address.sun_len = length as u8;
    }
    // Address is initialized and remains valid for the duration of connect.
    let result = unsafe {
        libc::connect(
            raw,
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut pollfd = libc::pollfd {
                fd: raw,
                events: libc::POLLOUT,
                revents: 0,
            };
            // pollfd points to one valid descriptor entry.
            let result = unsafe { libc::poll(&mut pollfd, 1, remaining.as_millis().max(1) as i32) };
            if result < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if result == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut error: libc::c_int = 0;
            let mut size = mem::size_of_val(&error) as libc::socklen_t;
            // SO_ERROR writes an integer into the supplied correctly sized buffer.
            if unsafe {
                libc::getsockopt(
                    raw,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut error as *mut libc::c_int).cast(),
                    &mut size,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            break;
        }
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

pub struct Server {
    diagnostics: RuntimeDiagnostics,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    _socket: SocketOwner,
    focus: FocusControl,
}

#[derive(Clone)]
pub struct FocusControl {
    sender: SyncSender<FamiliarCommand>,
    target_memo: TargetCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeBinding {
    pub service_instance: String,
    pub attachment: AttachmentBinding,
}

#[derive(Clone, Debug)]
pub struct FamiliarTargetSnapshot {
    pub binding: RuntimeBinding,
    pub targets: Arc<[LiveTarget]>,
}

#[derive(PartialEq, Eq)]
struct TargetMemo {
    service_instance: String,
    bindings: Vec<(String, AttachmentBinding)>,
    targets: Arc<[LiveTarget]>,
}

type TargetCache = Arc<Mutex<Option<Arc<TargetMemo>>>>;

fn publish_target_memo(
    store: &Store,
    service_instance: &str,
    cache: &TargetCache,
    previous: &mut Option<Arc<TargetMemo>>,
) {
    let memo = Arc::new(TargetMemo {
        service_instance: service_instance.into(),
        bindings: store.list_binding_snapshot(),
        targets: store.live_targets().into(),
    });
    if previous.as_ref() == Some(&memo) {
        return;
    }
    let old = {
        let mut slot = cache.lock().unwrap_or_else(|e| e.into_inner());
        slot.replace(memo.clone())
    };
    drop(old);
    *previous = Some(memo);
}

#[derive(Debug)]
enum FamiliarCommand {
    Attach {
        id: String,
        expected: RuntimeBinding,
        target: FocusTarget,
        incarnation: u64,
    },
    ForgetPreferences {
        id: String,
        binding: PreferenceBinding,
    },
    Placement {
        id: String,
        binding: PreferenceBinding,
        sequence: u64,
        placement: Placement,
    },
    Next(String),
    Close(String),
    Settings {
        id: String,
        patch: protocol::SettingsPatch,
    },
}

#[derive(Clone)]
pub struct FamiliarControl {
    sender: SyncSender<FamiliarCommand>,
    target_memo: TargetCache,
}

impl FamiliarControl {
    pub fn target_snapshot(&self, id: &str) -> Result<FamiliarTargetSnapshot> {
        protocol::validate_id(id)?;
        let memo = {
            let slot = self
                .target_memo
                .try_lock()
                .map_err(|_| anyhow::anyhow!("target snapshot busy"))?;
            slot.clone().context("target snapshot not ready")?
        };
        let attachment = memo
            .bindings
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, binding)| binding.clone())
            .context("unknown familiar")?;
        Ok(FamiliarTargetSnapshot {
            binding: RuntimeBinding {
                service_instance: memo.service_instance.clone(),
                attachment,
            },
            targets: memo.targets.clone(),
        })
    }

    /// Success means queue acceptance only. The worker revalidates before commit.
    pub fn attach(
        &self,
        id: &str,
        expected: &RuntimeBinding,
        target: &FocusTarget,
        incarnation: u64,
    ) -> Result<()> {
        validate_attach(id, expected, target, incarnation)?;
        self.sender
            .try_send(FamiliarCommand::Attach {
                id: id.into(),
                expected: expected.clone(),
                target: target.clone(),
                incarnation,
            })
            .context("queue familiar attach")
    }

    pub fn forget_preferences(&self, id: &str, binding: &PreferenceBinding) -> Result<()> {
        protocol::validate_id(id)?;
        protocol::validate_id(&binding.target.source)?;
        protocol::validate_id(&binding.target.instance)?;
        protocol::validate_id(&binding.target.activity)?;
        ensure!(
            binding.generation > 0,
            "binding generation must be positive"
        );
        self.sender
            .try_send(FamiliarCommand::ForgetPreferences {
                id: id.into(),
                binding: binding.clone(),
            })
            .context("queue forget preferences")
    }

    pub fn placement(
        &self,
        id: &str,
        binding: &PreferenceBinding,
        sequence: u64,
        placement: Placement,
    ) -> Result<()> {
        protocol::validate_id(id)?;
        protocol::validate_id(&binding.target.source)?;
        protocol::validate_id(&binding.target.instance)?;
        protocol::validate_id(&binding.target.activity)?;
        ensure!(
            binding.generation > 0 && sequence > 0,
            "binding generation and placement sequence must be positive"
        );
        placement.validate()?;
        self.sender
            .try_send(FamiliarCommand::Placement {
                id: id.into(),
                binding: binding.clone(),
                sequence,
                placement,
            })
            .context("queue familiar placement")
    }

    pub fn next(&self, id: &str) -> Result<()> {
        protocol::validate_id(id)?;
        self.sender
            .try_send(FamiliarCommand::Next(id.into()))
            .context("queue familiar cycle")
    }

    pub fn close(&self, id: &str) -> Result<()> {
        protocol::validate_id(id)?;
        self.sender
            .try_send(FamiliarCommand::Close(id.into()))
            .context("queue familiar close")
    }

    /// Queue a patch for exactly one familiar without blocking the GUI.
    /// Success means queue acceptance, not a committed change. Unsupported
    /// settings or an unknown/closed ID are logged and discarded by the worker.
    /// Display checkmarks only from the authoritative on_views settings snapshot.
    pub fn settings(&self, id: &str, patch: SettingsPatch) -> Result<()> {
        protocol::validate_id(id)?;
        patch.validate()?;
        self.sender
            .try_send(FamiliarCommand::Settings {
                id: id.into(),
                patch,
            })
            .context("queue familiar settings")
    }
}

fn validate_attach(
    id: &str,
    expected: &RuntimeBinding,
    target: &FocusTarget,
    incarnation: u64,
) -> Result<()> {
    protocol::validate_id(id)?;
    protocol::validate_id(&expected.service_instance)?;
    ensure!(
        expected.attachment.generation > 0,
        "attachment generation must be positive"
    );
    ensure!(incarnation > 0, "source incarnation must be positive");
    for target in std::iter::once(target).chain(expected.attachment.target.iter()) {
        protocol::validate_id(&target.source)?;
        protocol::validate_id(&target.instance)?;
        protocol::validate_id(&target.activity)?;
    }
    Ok(())
}

#[cfg(test)]
impl FocusControl {
    pub fn next(&self) -> Result<()> {
        self.sender
            .try_send(FamiliarCommand::Next("main".into()))
            .context("queue focus cycle")
    }
}

impl Server {
    pub(crate) fn diagnostics(&self) -> RuntimeDiagnostics {
        self.diagnostics.clone()
    }

    pub fn familiar_control(&self) -> FamiliarControl {
        FamiliarControl {
            sender: self.focus.sender.clone(),
            target_memo: self.focus.target_memo.clone(),
        }
    }
    #[cfg(test)]
    pub fn focus_control(&self) -> FocusControl {
        self.focus.clone()
    }

    /// The callback runs on the service worker and must return promptly (for
    /// example, enqueue a GUI event). Drop wakes and joins that worker.
    pub fn start(path: &Path, on_change: impl Fn(Presentation) + Send + 'static) -> Result<Self> {
        Self::start_inner(
            path,
            on_change,
            None,
            None,
            TargetedControls::default(),
            true,
        )
    }

    /// Reveal requests call `on_reveal` once on the worker. It must return
    /// promptly, for example by enqueueing a coalesced GUI broadcast event.
    /// A successful acknowledgment means accepted, not that a frame was drawn.
    #[cfg(test)]
    pub fn start_with_reveal(
        path: &Path,
        on_change: impl Fn(Presentation) + Send + 'static,
        on_reveal: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        Self::start_inner(
            path,
            on_change,
            Some(Box::new(on_reveal)),
            None,
            TargetedControls::default(),
            true,
        )
    }

    /// Callbacks run on the worker and must promptly enqueue/coalesce UI work.
    /// Views include the initial registry and every changed complete snapshot.
    #[cfg(test)]
    pub fn start_with_familiars(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
    ) -> Result<Self> {
        Self::start_with_familiars_config(path, on_views, on_reveal, true)
    }

    /// With `bootstrap_main` false, the initial view snapshot is empty.
    #[cfg(test)]
    pub fn start_with_familiars_config(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        bootstrap_main: bool,
    ) -> Result<Self> {
        Self::start_inner(
            path,
            |_| {},
            None,
            Some(Box::new(on_views)),
            TargetedControls {
                reveal: Some(Box::new(on_reveal)),
                go_to_mouse: None,
                quit: None,
            },
            bootstrap_main,
        )
    }

    /// Callbacks run on the worker and must only post bounded UI events, not do
    /// OS work. A `data: {moving: ids}` acknowledgment means the navigation
    /// request was accepted, not that the familiars have arrived.
    #[cfg(test)]
    pub fn start_with_navigation(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        on_go_to_mouse: impl Fn(Vec<String>) + Send + 'static,
    ) -> Result<Self> {
        Self::start_with_navigation_config(path, on_views, on_reveal, on_go_to_mouse, true)
    }

    /// With `bootstrap_main` false, the initial view snapshot is empty.
    #[cfg(test)]
    pub fn start_with_navigation_config(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        on_go_to_mouse: impl Fn(Vec<String>) + Send + 'static,
        bootstrap_main: bool,
    ) -> Result<Self> {
        Self::start_inner(
            path,
            |_| {},
            None,
            Some(Box::new(on_views)),
            TargetedControls {
                reveal: Some(Box::new(on_reveal)),
                go_to_mouse: Some(Box::new(on_go_to_mouse)),
                quit: None,
            },
            bootstrap_main,
        )
    }

    /// Callbacks run on the worker and must promptly enqueue bounded UI work.
    /// Quit is scheduled after its ACK is flushed and the peer closes, or after
    /// 500 ms if the peer stays open. The UI owns event-loop exit and server drop.
    #[cfg(test)]
    pub fn start_graphical(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        on_go_to_mouse: Option<Box<GoToMouse>>,
        on_quit: impl Fn() + Send + 'static,
        bootstrap_main: bool,
    ) -> Result<Self> {
        Self::start_inner(
            path,
            |_| {},
            None,
            Some(Box::new(on_views)),
            TargetedControls {
                reveal: Some(Box::new(on_reveal)),
                go_to_mouse: on_go_to_mouse,
                quit: Some(Box::new(on_quit)),
            },
            bootstrap_main,
        )
    }

    /// Enables live per-familiar settings with immutable defaults for future opens.
    #[cfg(test)]
    pub fn start_graphical_with_settings(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        on_go_to_mouse: Option<Box<GoToMouse>>,
        on_quit: impl Fn() + Send + 'static,
        bootstrap_main: bool,
        settings: FamiliarSettings,
    ) -> Result<Self> {
        Self::start_inner_with_settings(
            path,
            |_| {},
            None,
            Some(Box::new(on_views)),
            TargetedControls {
                reveal: Some(Box::new(on_reveal)),
                go_to_mouse: on_go_to_mouse,
                quit: Some(Box::new(on_quit)),
            },
            bootstrap_main,
            Some(settings),
        )
    }

    fn start_inner(
        path: &Path,
        on_change: impl Fn(Presentation) + Send + 'static,
        on_reveal: Option<Box<dyn Fn() + Send>>,
        on_views: Option<Box<dyn Fn(Vec<FamiliarPresentation>) + Send>>,
        targeted: TargetedControls,
        bootstrap_main: bool,
    ) -> Result<Self> {
        Self::start_inner_with_settings(
            path,
            on_change,
            on_reveal,
            on_views,
            targeted,
            bootstrap_main,
            None,
        )
    }

    fn start_inner_with_settings(
        path: &Path,
        on_change: impl Fn(Presentation) + Send + 'static,
        on_reveal: Option<Box<dyn Fn() + Send>>,
        on_views: Option<Box<dyn Fn(Vec<FamiliarPresentation>) + Send>>,
        targeted: TargetedControls,
        bootstrap_main: bool,
        settings: Option<FamiliarSettings>,
    ) -> Result<Self> {
        Self::start_inner_with_preferences(
            path,
            on_change,
            on_reveal,
            on_views,
            targeted,
            bootstrap_main,
            settings,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_graphical_with_preferences(
        path: &Path,
        on_views: impl Fn(Vec<FamiliarPresentation>) + Send + 'static,
        on_reveal: impl Fn(Option<Vec<String>>) + Send + 'static,
        on_go_to_mouse: Option<Box<GoToMouse>>,
        on_quit: impl Fn() + Send + 'static,
        bootstrap_main: bool,
        settings: FamiliarSettings,
        preferences_path: Option<PathBuf>,
    ) -> Result<Self> {
        let disabled = preferences_path.is_none();
        let server = Self::start_inner_with_preferences(
            path,
            |_| {},
            None,
            Some(Box::new(on_views)),
            TargetedControls {
                reveal: Some(Box::new(on_reveal)),
                go_to_mouse: on_go_to_mouse,
                quit: Some(Box::new(on_quit)),
            },
            bootstrap_main,
            Some(settings),
            preferences_path,
        )?;
        if disabled {
            server
                .diagnostics
                .set_preferences_status(preferences::Status::disabled());
        }
        Ok(server)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_inner_with_preferences(
        path: &Path,
        on_change: impl Fn(Presentation) + Send + 'static,
        on_reveal: Option<Box<dyn Fn() + Send>>,
        on_views: Option<Box<dyn Fn(Vec<FamiliarPresentation>) + Send>>,
        targeted: TargetedControls,
        bootstrap_main: bool,
        settings: Option<FamiliarSettings>,
        preferences_path: Option<PathBuf>,
    ) -> Result<Self> {
        let defaults = settings.unwrap_or_default();
        defaults.validate()?;
        private_parent(path, true)?;
        match fs::symlink_metadata(path) {
            Ok(_) => {
                let metadata = socket_metadata(path)?;
                match connect_bounded(path) {
                    Ok(_) => bail!("Numen socket is already active: {}", path.display()),
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                        let owner = SocketOwner::new(path, &metadata);
                        ensure!(
                            owner.matches(),
                            "socket changed while checking for a stale listener"
                        );
                        drop(owner);
                    }
                    Err(e) => {
                        // A failed probe other than refusal never authorizes removal.
                        return Err(e).context("cannot safely replace existing socket");
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("inspect existing socket"),
        }
        let listener =
            UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
        let owner = SocketOwner::new(path, &socket_metadata(path)?);
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        // Listener identity is a restart precondition, not an authentication secret.
        let service_instance = new_service_instance()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::sync_channel(32);
        let target_memo = TargetCache::default();
        let worker_memo = target_memo.clone();
        let diagnostics = RuntimeDiagnostics::new(on_views.is_some());
        let preferences = preferences_path.map(|path| {
            let status_diagnostics = diagnostics.clone();
            let loaded = preferences::start(path, move |status| {
                status_diagnostics.set_preferences_status(status)
            });
            diagnostics.set_preferences_status(loaded.1.status());
            loaded
        });
        let worker_diagnostics = diagnostics.clone();
        let worker = thread::Builder::new()
            .name("numen-ipc".into())
            .spawn(move || {
                run(
                    listener,
                    worker_stop,
                    receiver,
                    on_change,
                    on_reveal,
                    on_views,
                    WorkerConfig {
                        target_memo: worker_memo,
                        preferences,
                        diagnostics: worker_diagnostics,
                        settings_support: settings.is_some(),
                        defaults,
                        targeted,
                        bootstrap_main,
                        service_instance,
                    },
                )
            })?;
        Ok(Self {
            diagnostics,
            stop,
            worker: Some(worker),
            _socket: owner,
            focus: FocusControl {
                sender,
                target_memo,
            },
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    connected: Instant,
    source: Option<SourceId>,
    read_eof: bool,
    drain_started: Option<Instant>,
    pending_quit: bool,
    quit_flushed: Option<Instant>,
}

impl Client {
    #[cfg(test)]
    fn step(
        &mut self,
        store: &mut Store,
        now: Instant,
        changed: &mut impl FnMut(&mut Store, Instant),
        on_reveal: Option<&(dyn Fn() + Send)>,
    ) -> bool {
        self.step_inner(
            store,
            now,
            changed,
            on_reveal,
            &ControlContext {
                diagnostics: RuntimeDiagnostics::new(false),
                settings_support: false,
                targeted: TargetedControls::default(),
                multi: false,
                service_instance: "test-service".into(),
            },
        )
    }

    fn step_inner(
        &mut self,
        store: &mut Store,
        now: Instant,
        changed: &mut impl FnMut(&mut Store, Instant),
        on_reveal: Option<&(dyn Fn() + Send)>,
        controls: &ControlContext,
    ) -> bool {
        let multi = controls.multi;
        let targeted = &controls.targeted;
        if self
            .drain_started
            .is_some_and(|started| now.saturating_duration_since(started) >= EOF_DRAIN_TIMEOUT)
        {
            return false;
        }
        if self.source.is_none()
            && now.saturating_duration_since(self.connected) >= REGISTER_TIMEOUT
        {
            return false;
        }
        // Only a single bounded read and eight messages per client per tick.
        let mut bytes = [0; MAX_LINE + 1];
        let room = MAX_LINE + 1 - self.input.len();
        if !self.read_eof && room > 0 {
            match self.stream.read(&mut bytes[..room]) {
                Ok(0) => {
                    self.read_eof = true;
                    self.drain_started = Some(now);
                }
                Ok(n) => self.input.extend_from_slice(&bytes[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return false,
            }
        }
        for _ in 0..MESSAGES_PER_TICK {
            if self.pending_quit {
                // Discard pipelined requests, but keep bounded reads to detect EOF.
                self.input.clear();
                break;
            }
            let Some(end) = self.input.iter().position(|b| *b == b'\n') else {
                if self.read_eof {
                    // EOF cannot complete a trailing frame. Discard it only
                    // after all preceding complete frames have been handled.
                    self.input.clear();
                } else if self.input.len() > MAX_LINE {
                    return false;
                }
                break;
            };
            if end > MAX_LINE {
                return false;
            }
            let mut skip_changed = false;
            let result = Request::parse(&self.input[..end]).and_then(|request| {
                if request
                    .service_instance
                    .as_ref()
                    .is_some_and(|expected| expected != &controls.service_instance)
                {
                    skip_changed = true;
                    bail!("service instance changed; reopen menu");
                }
                let event = match request.event {
                    Event::Quit => {
                        skip_changed = true;
                        ensure!(
                            targeted.quit.is_some(),
                            "companion.quit unsupported: no graphical shutdown callback"
                        );
                        self.pending_quit = true;
                        return Ok(Some(json!({"quitting":true})));
                    }
                    Event::Service => {
                        skip_changed = true;
                        // Reports control readiness, not completion of a GPU frame.
                        return Ok(Some(json!({
                            "api_version": 1,
                            "service_instance": controls.service_instance,
                            "pid": std::process::id(),
                            "graphical": multi,
                            "lifecycle": multi,
                            "navigation": targeted.go_to_mouse.is_some(),
                            "settings": multi && controls.settings_support,
                            "preferences": multi && store.preferences_writable(),
                            "familiar_count": store.familiar_count(),
                            "source_count": store.source_count(),
                            "diagnostics": controls.diagnostics.snapshot(),
                        })));
                    }
                    Event::Status { offset, familiar } => {
                        return Ok(Some(serde_json::to_value(
                            store.status_for(offset, familiar.as_deref())?,
                        )?));
                    }
                    Event::Focus {
                        source,
                        instance,
                        activity,
                        familiar,
                    } => {
                        if let Some(id) = familiar.as_deref() {
                            store.focus_for(id, &source, instance.as_deref(), &activity)?;
                        } else {
                            store.focus(&source, instance.as_deref(), &activity)?;
                        }
                        return Ok(Some(serde_json::to_value(
                            store.status_for(0, familiar.as_deref())?,
                        )?));
                    }
                    Event::Next { familiar } => {
                        if let Some(id) = familiar.as_deref() {
                            store.cycle_for(id)?;
                        } else {
                            store.cycle()?;
                        }
                        return Ok(Some(serde_json::to_value(
                            store.status_for(0, familiar.as_deref())?,
                        )?));
                    }
                    Event::Reveal { familiars } => {
                        if let Some(ids) = &familiars {
                            for id in ids {
                                store.require_familiar(id)?;
                            }
                        }
                        if let Some(reveal) = &targeted.reveal {
                            reveal(familiars);
                        } else {
                            ensure!(
                                familiars.is_none(),
                                "targeted reveal requires multi-familiar graphical support"
                            );
                            on_reveal.context("no graphical familiar")?();
                        }
                        return Ok(Some(serde_json::to_value(store.status(0))?));
                    }
                    Event::FamiliarList { offset } => {
                        let mut list = store.list(offset);
                        if let Some(rows) = list["familiars"].as_array_mut() {
                            for row in rows {
                                if !multi {
                                    row["capabilities"] = json!([]);
                                } else {
                                    let caps = row["capabilities"].as_array_mut().unwrap();
                                    if targeted.go_to_mouse.is_some() {
                                        caps.push(json!("go-to-mouse"));
                                    }
                                    if controls.settings_support {
                                        caps.push(json!("settings"));
                                    }
                                    if store.preferences_writable() {
                                        caps.push(json!("preferences"));
                                    }
                                }
                            }
                        }
                        return Ok(Some(list));
                    }
                    Event::GoToMouse { ids } => {
                        let go_to_mouse = targeted
                            .go_to_mouse
                            .as_ref()
                            .context("go-to-mouse requires graphical navigation support")?;
                        for id in &ids {
                            store.require_familiar(id)?;
                        }
                        go_to_mouse(ids.clone());
                        return Ok(Some(json!({"moving":ids})));
                    }
                    Event::Settings { ids, patch } => {
                        ensure!(
                            multi && controls.settings_support,
                            "settings requires live graphical settings support"
                        );
                        return Ok(Some(json!({"updated":store.settings(&ids, patch)?})));
                    }
                    Event::ForgetPreferences { ids } => {
                        ensure!(multi, "no graphical familiar");
                        return Ok(Some(json!({"forgotten":store.forget_preferences(&ids)?})));
                    }
                    Event::Targets { offset, familiar } => {
                        return Ok(Some(store.targets(offset, familiar.as_deref())?));
                    }
                    Event::Open { items } => {
                        ensure!(multi, "no graphical familiar");
                        return Ok(Some(json!({"opened":store.open(&items)?})));
                    }
                    Event::Attach { items } => {
                        ensure!(multi, "no graphical familiar");
                        return Ok(Some(json!({"attached":store.attach(&items)?})));
                    }
                    Event::Close { ids } => {
                        ensure!(multi, "no graphical familiar");
                        return Ok(Some(json!({"closed":store.close(&ids)?})));
                    }
                    event => event,
                };
                match (&self.source, event) {
                    (
                        None,
                        Event::Register {
                            source,
                            instance,
                            activities,
                        },
                    ) => {
                        let id = (source, instance);
                        store.register(id.clone(), activities, now)?;
                        self.source = Some(id);
                        Ok(None)
                    }
                    (None, _) => bail!("source.register must be the first message"),
                    (Some(id), event) => store.apply(id, event, now).map(|()| None),
                }
            });
            self.input.drain(..=end);
            let response = match result {
                Ok(Some(data)) => json!({"version":1,"ok":true,"data":data}),
                Ok(None) => json!({"version":1,"ok":true}),
                Err(e) => json!({"version":1,"ok":false,"error":e.to_string()}),
            };
            let mut line = serde_json::to_vec(&response).expect("JSON response is serializable");
            if line.len() > MAX_LINE {
                return false;
            }
            line.push(b'\n');
            if self.output.len() + line.len() > MAX_OUTPUT {
                return false;
            }
            self.output.extend(line);
            if !skip_changed {
                changed(store, now);
            }
        }
        if self.written < self.output.len() {
            match self.stream.write(&self.output[self.written..]) {
                Ok(0) => return false,
                Ok(n) => self.written += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return false,
            }
        }
        if self.written == self.output.len() {
            self.output.clear();
            self.written = 0;
            if self.pending_quit {
                let flushed = *self.quit_flushed.get_or_insert_with(Instant::now);
                if self.read_eof || now.saturating_duration_since(flushed) >= IO_TIMEOUT {
                    return false;
                }
            }
        }
        if self.read_eof && !self.input.contains(&b'\n') {
            self.input.clear();
        }
        !self.read_eof || !self.input.is_empty() || !self.output.is_empty()
    }
}

fn run(
    listener: UnixListener,
    stop: Arc<AtomicBool>,
    focus: Receiver<FamiliarCommand>,
    on_change: impl Fn(Presentation),
    on_reveal: Option<Box<dyn Fn() + Send>>,
    on_views: Option<Box<dyn Fn(Vec<FamiliarPresentation>) + Send>>,
    config: WorkerConfig,
) {
    let WorkerConfig {
        target_memo,
        preferences,
        diagnostics,
        settings_support,
        defaults,
        targeted,
        bootstrap_main,
        service_instance,
    } = config;
    let controls = ControlContext {
        diagnostics,
        settings_support,
        targeted,
        multi: on_views.is_some(),
        service_instance,
    };
    let mut store = Store::with_settings(bootstrap_main, defaults);
    let writer = preferences.map(|(document, writer)| {
        store.enable_preferences(document, writer.can_write());
        writer
    });
    let mut submitted_revision = 0;
    let mut previous = Presentation::default();
    on_change(previous.clone());
    let mut previous_views = store.presentations(Instant::now());
    let mut previous_memo = None;
    publish_target_memo(
        &store,
        &controls.service_instance,
        &target_memo,
        &mut previous_memo,
    );
    if let Some(callback) = &on_views {
        callback(previous_views.clone());
    }
    let mut changed = |store: &mut Store, now| {
        publish_target_memo(
            store,
            &controls.service_instance,
            &target_memo,
            &mut previous_memo,
        );
        let view = store.presentation(now);
        if view != previous {
            previous = view.clone();
            on_change(view);
        }
        let views = store.presentations(now);
        if views != previous_views {
            previous_views = views.clone();
            if let Some(callback) = &on_views {
                callback(views);
            }
        }
    };
    let mut clients: Vec<Client> = Vec::new();
    let mut quit_requested = false;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if let Some(writer) = &writer {
            store.set_preferences_writable(writer.can_write());
        }
        let expired = store.expire(now);
        let mut quit_ready = false;
        clients.retain(|c| {
            let alive = !c.source.as_ref().is_some_and(|id| expired.contains(id));
            quit_ready |= !alive && c.pending_quit;
            alive
        });
        for _ in 0..MESSAGES_PER_TICK {
            let Ok(command) = focus.try_recv() else {
                break;
            };
            let result = apply_familiar_command(&mut store, command, &controls);
            if let Err(error) = result {
                eprintln!("numen familiar control: {error}");
            }
            changed(&mut store, now);
        }
        changed(&mut store, now);
        for _ in 0..MAX_CLIENTS {
            match listener.accept() {
                Ok((stream, _)) if clients.len() < MAX_CLIENTS => {
                    if stream.set_nonblocking(true).is_ok() {
                        clients.push(Client {
                            stream,
                            input: Vec::new(),
                            output: Vec::new(),
                            written: 0,
                            connected: now,
                            source: None,
                            read_eof: false,
                            drain_started: None,
                            pending_quit: false,
                            quit_flushed: None,
                        });
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        clients.retain_mut(|client| {
            let alive = client.step_inner(
                &mut store,
                now,
                &mut changed,
                on_reveal.as_deref(),
                &controls,
            );
            if !alive {
                quit_ready |= client.pending_quit;
                if let Some(id) = &client.source {
                    store.disconnect(id);
                }
                changed(&mut store, now);
            }
            alive
        });
        if quit_ready && !quit_requested {
            quit_requested = true;
            if let Some(quit) = &controls.targeted.quit {
                quit();
            }
        }
        if let Some(writer) = &writer {
            if store.preference_revision() != submitted_revision
                && let Some((revision, document)) = store.preference_document()
                && writer.submit(document, revision)
            {
                submitted_revision = revision;
            }
            if store.preference_revision() == submitted_revision
                && store.preference_capacity_pending()
            {
                writer.note_capacity();
                if writer.status().reason == Some(preferences::Reason::CapacityReached) {
                    store.take_preference_capacity();
                }
            }
        }
        thread::park_timeout(TICK);
    }
    // The GUI queue is bounded at 32; drain only the commands accepted before stop.
    for _ in 0..32 {
        let Ok(command) = focus.try_recv() else {
            break;
        };
        let _ = apply_familiar_command(&mut store, command, &controls);
    }
    if let Some(writer) = writer {
        let deadline = Instant::now() + Duration::from_millis(500);
        let capacity = store.take_preference_capacity();
        let final_document = store
            .preference_document()
            .filter(|(revision, _)| *revision != submitted_revision);
        let (finished, completion) = mpsc::sync_channel(1);
        // Writer drop has its own bounded cleanup. Keep that separate from the
        // root worker's single shutdown budget, including a blocked filesystem.
        let _ = thread::Builder::new()
            .name("numen-preferences-stop".into())
            .spawn(move || {
                let mut published = final_document.is_none();
                if let Some((revision, document)) = final_document {
                    while Instant::now() < deadline {
                        if writer.submit(document.clone(), revision) {
                            published = true;
                            break;
                        }
                        if !writer.can_write() {
                            break;
                        }
                        thread::yield_now();
                    }
                }
                if capacity && published {
                    writer.note_capacity();
                }
                writer.flush(deadline.saturating_duration_since(Instant::now()));
                drop(writer);
                let _ = finished.try_send(());
            });
        let _ = completion.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn apply_familiar_command(
    store: &mut Store,
    command: FamiliarCommand,
    controls: &ControlContext,
) -> Result<()> {
    match command {
        FamiliarCommand::Attach {
            id,
            expected,
            target,
            incarnation,
        } => {
            ensure!(controls.multi, "no graphical familiar");
            validate_attach(&id, &expected, &target, incarnation)?;
            ensure!(
                expected.service_instance == controls.service_instance,
                "service instance changed"
            );
            ensure!(
                store.attachment_binding(&id)? == expected.attachment,
                "stale attachment binding"
            );
            let live = store
                .live_targets()
                .into_iter()
                .find(|row| FocusTarget::from(&row.selection) == target)
                .context("target no longer live")?;
            ensure!(
                live.incarnation == incarnation,
                "source incarnation changed"
            );
            ensure!(
                live.claimed_by.as_deref().is_none_or(|owner| owner == id),
                "target already claimed"
            );
            store
                .attach(&[protocol::AttachRequest {
                    id,
                    target: protocol::TargetRequest {
                        source: target.source,
                        instance: Some(target.instance),
                        activity: target.activity,
                    },
                }])
                .map(|_| ())
        }
        FamiliarCommand::Next(id) => store.cycle_for(&id),
        FamiliarCommand::Close(id) => {
            ensure!(controls.multi, "no graphical familiar");
            store.close(&[id]).map(|_| ())
        }
        FamiliarCommand::Settings { id, patch } => {
            ensure!(
                controls.multi && controls.settings_support,
                "settings requires live graphical settings support"
            );
            store.settings(&[id], patch).map(|_| ())
        }
        FamiliarCommand::ForgetPreferences { id, binding } => {
            ensure!(controls.multi, "no graphical familiar");
            store.require_preference_binding(&id, &binding)?;
            store.forget_preferences(&[id]).map(|_| ())
        }
        FamiliarCommand::Placement {
            id,
            binding,
            sequence,
            placement,
        } => {
            ensure!(controls.multi, "no graphical familiar");
            store.placement(&id, &binding, sequence, placement)
        }
    }
}

pub struct Publisher {
    stream: UnixStream,
}

#[derive(Deserialize)]
struct Ack {
    version: u32,
    ok: bool,
    error: Option<String>,
    data: Option<serde_json::Value>,
}

// Validate uniqueness before Value can discard duplicate metadata, including in
// unknown optional fields. Each object owns its own key set; arrays recurse too.
struct UniqueValue(serde_json::Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }

            fn visit_bool<E: serde::de::Error>(
                self,
                value: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }

            fn visit_i64<E: serde::de::Error>(
                self,
                value: i64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }

            fn visit_u64<E: serde::de::Error>(
                self,
                value: u64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }

            fn visit_f64<E: serde::de::Error>(
                self,
                value: f64,
            ) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(number.into()))
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }

            fn visit_string<E: serde::de::Error>(
                self,
                value: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }

            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(serde_json::Value::Null))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(values.into()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut object: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = object.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON object key"));
                    }
                    let UniqueValue(value) = object.next_value()?;
                    values.insert(key, value);
                }
                Ok(UniqueValue(values.into()))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

fn parse_ack(response: &[u8]) -> Result<Ack> {
    ensure!(response.len() <= MAX_LINE, "oversized Numen acknowledgment");
    let UniqueValue(value) =
        serde_json::from_slice(response).context("invalid Numen acknowledgment")?;
    serde_json::from_value(value).context("invalid Numen acknowledgment")
}

fn connect_private(path: &Path) -> Result<UnixStream> {
    private_parent(path, false)?;
    let metadata = socket_metadata(path)?;
    ensure!(
        metadata.mode() & 0o077 == 0,
        "socket must have private permissions (0600)"
    );
    connect_bounded(path).with_context(|| {
        format!(
            "connect to Numen at {}; is the service running?",
            path.display()
        )
    })
}

pub fn control(path: &Path, request: serde_json::Value) -> Result<serde_json::Value> {
    let bytes = serde_json::to_vec(&request)?;
    ensure!(
        matches!(
            Request::parse(&bytes)?.event,
            Event::Status { .. }
                | Event::Service
                | Event::Quit
                | Event::Focus { .. }
                | Event::Next { .. }
                | Event::Reveal { .. }
                | Event::FamiliarList { .. }
                | Event::Targets { .. }
                | Event::Open { .. }
                | Event::Attach { .. }
                | Event::Close { .. }
                | Event::GoToMouse { .. }
                | Event::Settings { .. }
                | Event::ForgetPreferences { .. }
        ),
        "expected companion control request"
    );
    let mut client = Publisher {
        stream: connect_private(path)?,
    };
    client
        .exchange(request)?
        .context("control acknowledgment is missing data")
}

impl Publisher {
    pub fn connect(path: &Path, source: &str, instance: &str) -> Result<Self> {
        protocol::validate_id(source)?;
        protocol::validate_id(instance)?;
        let stream = connect_private(path)?;
        let mut publisher = Self { stream };
        publisher.send(json!({"type":"source.register","version":1,"source":source,"instance":instance,"activities":[]}))?;
        Ok(publisher)
    }

    pub fn update(
        &mut self,
        activity: &str,
        sequence: u64,
        state: ActivityState,
        attention: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        self.update_with_summary(activity, sequence, state, attention, reason, None)
    }

    pub fn update_with_summary(
        &mut self,
        activity: &str,
        sequence: u64,
        state: ActivityState,
        attention: bool,
        reason: Option<&str>,
        summary: Option<&str>,
    ) -> Result<()> {
        Activity {
            activity: activity.into(),
            sequence,
            state,
            attention,
            reason: reason.map(str::to_owned),
            summary: summary.map(str::to_owned),
        }
        .validate()?;
        self.send(json!({"type":"activity.state","version":1,"activity":activity,"sequence":sequence,"state":state,"attention":attention,"reason":reason,"summary":summary}))
    }

    pub fn heartbeat(&mut self) -> Result<()> {
        self.send(json!({"type":"source.heartbeat","version":1}))
    }

    pub fn remove(&mut self, activity: &str, sequence: u64) -> Result<()> {
        protocol::validate_id(activity)?;
        ensure!(sequence > 0, "sequence must be positive");
        self.send(
            json!({"type":"activity.remove","version":1,"activity":activity,"sequence":sequence}),
        )
    }

    fn send(&mut self, value: serde_json::Value) -> Result<()> {
        self.exchange(value).map(|_| ())
    }

    fn exchange(&mut self, value: serde_json::Value) -> Result<Option<serde_json::Value>> {
        let mut line = serde_json::to_vec(&value)?;
        ensure!(line.len() <= MAX_LINE, "message exceeds 8 KiB");
        line.push(b'\n');
        self.stream
            .write_all(&line)
            .context("write Numen event (500 ms I/O timeout)")?;
        // A deadline also bounds a peer that trickles bytes without finishing a line.
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut response = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "Numen acknowledgment timed out after 500 ms"
            );
            self.stream.set_read_timeout(Some(remaining))?;
            let mut byte = [0];
            match self.stream.read(&mut byte) {
                Ok(0) => bail!("Numen closed the connection before acknowledging the event"),
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => {
                    response.push(byte[0]);
                    ensure!(response.len() <= MAX_LINE, "oversized Numen acknowledgment");
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("read Numen acknowledgment (500 ms timeout)"),
            }
        }
        let ack = parse_ack(&response)?;
        ensure!(
            ack.version == protocol::VERSION,
            "unsupported Numen acknowledgment version"
        );
        ensure!(
            ack.ok,
            "Numen rejected event: {}",
            ack.error.as_deref().unwrap_or("unspecified protocol error")
        );
        Ok(ack.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::sync::mpsc::{self, Receiver};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("events.sock");
        (dir, path)
    }

    fn picker_store() -> (Store, ControlContext) {
        let mut store = Store::default();
        store
            .register(
                ("test".into(), "one".into()),
                ["a", "b", "c"]
                    .into_iter()
                    .map(|name| Activity {
                        activity: name.into(),
                        sequence: 1,
                        state: ActivityState::Working,
                        attention: false,
                        reason: None,
                        summary: None,
                    })
                    .collect(),
                Instant::now(),
            )
            .unwrap();
        (
            store,
            ControlContext {
                diagnostics: RuntimeDiagnostics::new(true),
                settings_support: true,
                targeted: TargetedControls::default(),
                multi: true,
                service_instance: "test-service".into(),
            },
        )
    }

    #[test]
    fn picker_cache_is_immutable_complete_shared_and_nonblocking() {
        let mut store = Store::default();
        let now = Instant::now();
        for source in 0..16 {
            store
                .register(
                    (format!("source-{source:02}"), "one".into()),
                    (0..32)
                        .map(|activity| Activity {
                            activity: format!("task-{activity:02}"),
                            sequence: 1,
                            state: ActivityState::Working,
                            attention: false,
                            reason: None,
                            summary: None,
                        })
                        .collect(),
                    now,
                )
                .unwrap();
        }
        let opened = store
            .open(
                &(1..16)
                    .map(|index| protocol::OpenRequest {
                        id: Some(format!("picker-{index:02}")),
                        preset: "djinn".into(),
                        target: protocol::TargetRequest {
                            source: "source-00".into(),
                            instance: Some("one".into()),
                            activity: format!("task-{index:02}"),
                        },
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let cache = TargetCache::default();
        let (sender, _receiver) = mpsc::sync_channel(32);
        let focus = FocusControl {
            sender,
            target_memo: cache.clone(),
        };
        let gui = FamiliarControl {
            sender: focus.sender.clone(),
            target_memo: focus.target_memo.clone(),
        };
        assert!(gui.target_snapshot("main").is_err());
        let mut previous = None;
        publish_target_memo(&store, "service", &cache, &mut previous);
        let first = gui.target_snapshot("main").unwrap();
        assert_eq!(first.targets.len(), 512);
        assert_eq!(previous.as_ref().unwrap().bindings.len(), 16);
        for id in std::iter::once("main").chain(opened.iter().map(String::as_str)) {
            let snapshot = gui.target_snapshot(id).unwrap();
            assert_eq!(
                snapshot.binding.attachment,
                store.attachment_binding(id).unwrap()
            );
            assert!(Arc::ptr_eq(&snapshot.targets, &first.targets));
        }
        assert_eq!(first.targets[0].claimed_by.as_deref(), Some("main"));
        assert_eq!(first.targets[511].selection.activity, "task-31");
        let cloned = gui.clone().target_snapshot("main").unwrap();
        assert!(Arc::ptr_eq(&first.targets, &cloned.targets));
        let held = cache.lock().unwrap();
        let started = Instant::now();
        assert!(gui.target_snapshot("main").is_err());
        assert!(started.elapsed() < Duration::from_millis(50));
        drop(held);
        assert!(gui.target_snapshot("bad/id").is_err());
        assert!(gui.target_snapshot("missing").is_err());
        let views = store.presentations(now);
        store
            .apply(
                &("source-15".into(), "one".into()),
                Event::State {
                    activity: Activity {
                        activity: "task-31".into(),
                        sequence: 2,
                        state: ActivityState::Failed,
                        attention: false,
                        reason: Some("changed".into()),
                        summary: Some("new".into()),
                    },
                },
                now,
            )
            .unwrap();
        assert_eq!(store.presentations(now), views);
        publish_target_memo(&store, "service", &cache, &mut previous);
        let latest = gui.target_snapshot("main").unwrap();
        assert_eq!(latest.targets.len(), 512);
        assert_eq!(latest.targets[511].selection.state, ActivityState::Failed);
        assert_eq!(
            latest.targets[511].selection.summary.as_deref(),
            Some("new")
        );
        assert!(first.targets[511].selection.summary.is_none());
        assert_eq!(latest.binding, first.binding);
        assert_eq!(
            store.expire(now + crate::activity::SOURCE_TIMEOUT).len(),
            16
        );
        publish_target_memo(&store, "service", &cache, &mut previous);
        assert!(gui.target_snapshot("main").unwrap().targets.is_empty());
        assert_eq!(gui.target_snapshot("main").unwrap().binding, first.binding);
        assert_eq!(previous.as_ref().unwrap().bindings.len(), 16);
        for id in opened {
            let snapshot = gui.target_snapshot(&id).unwrap();
            assert!(snapshot.targets.is_empty());
            assert_eq!(
                snapshot.binding.attachment,
                store.attachment_binding(&id).unwrap()
            );
        }
    }

    #[test]
    fn picker_attach_queue_validates_every_binding_and_preserves_queued_close() {
        let (store, controls) = picker_store();
        let binding = RuntimeBinding {
            service_instance: controls.service_instance,
            attachment: store.attachment_binding("main").unwrap(),
        };
        let target = binding.attachment.target.clone().unwrap();
        let (sender, receiver) = mpsc::sync_channel(32);
        let gui = FamiliarControl {
            sender,
            target_memo: TargetCache::default(),
        };
        assert!(gui.attach("bad/id", &binding, &target, 1).is_err());
        assert!(gui.attach("main", &binding, &target, 0).is_err());
        let mut invalid = binding.clone();
        invalid.attachment.generation = 0;
        assert!(gui.attach("main", &invalid, &target, 1).is_err());
        invalid = binding.clone();
        invalid.service_instance = "bad/id".into();
        assert!(gui.attach("main", &invalid, &target, 1).is_err());
        invalid = binding.clone();
        invalid.attachment.target.as_mut().unwrap().instance = "bad/id".into();
        assert!(gui.attach("main", &invalid, &target, 1).is_err());
        let mut bad_target = target.clone();
        bad_target.activity = "bad/id".into();
        assert!(gui.attach("main", &binding, &bad_target, 1).is_err());
        assert!(receiver.try_recv().is_err());
        gui.close("main").unwrap();
        for _ in 0..31 {
            gui.attach("main", &binding, &target, 1).unwrap();
        }
        assert!(gui.attach("main", &binding, &target, 1).is_err());
        assert!(matches!(receiver.try_recv().unwrap(), FamiliarCommand::Close(id) if id == "main"));
        for _ in 0..31 {
            assert!(matches!(
                receiver.try_recv().unwrap(),
                FamiliarCommand::Attach { .. }
            ));
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn picker_attach_revalidates_stale_choices_without_mutation() {
        for scenario in [
            "reconnect",
            "aba",
            "removed",
            "disconnected",
            "claimed",
            "closed",
            "headless",
            "service",
            "binding",
            "binding-target",
            "binding-unbound",
            "expired",
        ] {
            let (mut store, mut controls) = picker_store();
            store.enable_preferences(preferences::Document::default(), true);
            let expected = RuntimeBinding {
                service_instance: controls.service_instance.clone(),
                attachment: store.attachment_binding("main").unwrap(),
            };
            let chosen = store.live_targets()[1].clone();
            let target = FocusTarget::from(&chosen.selection);
            let mut command_expected = expected.clone();
            match scenario {
                "reconnect" => {
                    store.disconnect(&("test".into(), "one".into()));
                    store
                        .register(
                            ("test".into(), "one".into()),
                            vec![Activity {
                                activity: "b".into(),
                                sequence: 1,
                                state: ActivityState::Working,
                                attention: false,
                                reason: None,
                                summary: None,
                            }],
                            Instant::now(),
                        )
                        .unwrap();
                }
                "aba" => {
                    store.focus("test", Some("one"), "c").unwrap();
                    store.focus("test", Some("one"), "a").unwrap();
                }
                "removed" => {
                    store
                        .apply(
                            &("test".into(), "one".into()),
                            Event::Remove {
                                activity: "b".into(),
                                sequence: 2,
                            },
                            Instant::now(),
                        )
                        .unwrap();
                }
                "disconnected" => store.disconnect(&("test".into(), "one".into())),
                "claimed" => {
                    store
                        .open(&[protocol::OpenRequest {
                            id: Some("other".into()),
                            preset: "djinn".into(),
                            target: protocol::TargetRequest {
                                source: "test".into(),
                                instance: Some("one".into()),
                                activity: "b".into(),
                            },
                        }])
                        .unwrap();
                }
                "closed" => {
                    store.close(&["main".into()]).unwrap();
                }
                "headless" => controls.multi = false,
                "service" => command_expected.service_instance = "previous-service".into(),
                "binding" => command_expected.attachment.generation += 1,
                "binding-target" => command_expected.attachment.target = Some(target.clone()),
                "binding-unbound" => command_expected.attachment.target = None,
                "expired" => {
                    store.expire(Instant::now() + crate::activity::SOURCE_TIMEOUT);
                }
                _ => unreachable!(),
            }
            let now = Instant::now();
            let before = store.presentations(now);
            let rows = store.live_targets();
            let preferences = store.preference_document().unwrap().1.encode().unwrap();
            assert!(
                apply_familiar_command(
                    &mut store,
                    FamiliarCommand::Attach {
                        id: "main".into(),
                        expected: command_expected,
                        target,
                        incarnation: chosen.incarnation,
                    },
                    &controls
                )
                .is_err(),
                "{scenario}"
            );
            assert_eq!(store.presentations(now), before, "{scenario}");
            assert_eq!(store.live_targets(), rows, "{scenario}");
            assert_eq!(
                store.preference_document().unwrap().1.encode().unwrap(),
                preferences,
                "{scenario}"
            );
        }
    }

    #[test]
    fn picker_attach_changes_only_exact_attachment_and_never_refreshes_source_ttl() {
        let (mut store, controls) = picker_store();
        let now = Instant::now();
        let destination_style = FamiliarSettings {
            font_size: 30.0,
            ..Default::default()
        };
        store.enable_preferences(
            preferences::Document {
                version: 1,
                records: vec![preferences::Record {
                    source: "test".into(),
                    activity: "b".into(),
                    settings: destination_style,
                    placement: Some(Placement { x: 900.0, y: 800.0 }),
                }],
            },
            true,
        );
        store
            .settings(
                &["main".into()],
                SettingsPatch {
                    font_size: Some(18.0),
                    ..Default::default()
                },
            )
            .unwrap();
        let before = store.presentations(now).remove(0);
        let preference = before.preference.as_ref().unwrap();
        let placement = Placement { x: 200.0, y: 100.0 };
        store
            .placement("main", &preference.binding, 1, placement)
            .unwrap();
        let document = store.preference_document().unwrap();
        let expected = RuntimeBinding {
            service_instance: controls.service_instance.clone(),
            attachment: before.runtime_binding,
        };
        let chosen = store.live_targets()[1].clone();
        store
            .apply(
                &("test".into(), "one".into()),
                Event::State {
                    activity: Activity {
                        activity: "b".into(),
                        sequence: 2,
                        state: ActivityState::Waiting,
                        attention: true,
                        reason: Some("question".into()),
                        summary: Some("updated metadata".into()),
                    },
                },
                now,
            )
            .unwrap();
        // Metadata changes do not revoke the selected registration incarnation.
        apply_familiar_command(
            &mut store,
            FamiliarCommand::Attach {
                id: "main".into(),
                expected: expected.clone(),
                target: FocusTarget::from(&chosen.selection),
                incarnation: chosen.incarnation,
            },
            &controls,
        )
        .unwrap();
        let after = store.presentations(now).remove(0);
        assert_eq!(
            after.runtime_binding.generation,
            expected.attachment.generation + 1
        );
        assert_eq!(after.runtime_binding.target.as_ref().unwrap().activity, "b");
        assert_eq!(after.settings, before.settings);
        assert_eq!(after.preference.unwrap().placement, Some(placement));
        assert_eq!(store.preference_document().unwrap().0, document.0);
        assert_eq!(
            store.preference_document().unwrap().1.encode().unwrap(),
            document.1.encode().unwrap()
        );
        assert_eq!(store.expire(now + crate::activity::SOURCE_TIMEOUT).len(), 1);
    }

    #[test]
    fn picker_attach_accepts_self_and_forget_does_not_revoke() {
        let (mut store, controls) = picker_store();
        store.enable_preferences(preferences::Document::default(), true);
        let expected = RuntimeBinding {
            service_instance: controls.service_instance.clone(),
            attachment: store.attachment_binding("main").unwrap(),
        };
        store.forget_preferences(&["main".into()]).unwrap();
        let row = store.live_targets().remove(0);
        let deadline = Instant::now() + crate::activity::SOURCE_TIMEOUT;
        apply_familiar_command(
            &mut store,
            FamiliarCommand::Attach {
                id: "main".into(),
                expected: expected.clone(),
                target: FocusTarget::from(&row.selection),
                incarnation: row.incarnation,
            },
            &controls,
        )
        .unwrap();
        assert_eq!(
            store.attachment_binding("main").unwrap(),
            expected.attachment
        );
        assert_eq!(store.expire(deadline).len(), 1);
    }

    #[test]
    fn picker_queued_unbound_binding_is_revoked_by_normal_bootstrap() {
        let (_, controls) = picker_store();
        let mut store = Store::default();
        let cache = TargetCache::default();
        let (sender, receiver) = mpsc::sync_channel(32);
        let gui = FamiliarControl {
            sender,
            target_memo: cache.clone(),
        };
        publish_target_memo(&store, &controls.service_instance, &cache, &mut None);
        let snapshot = gui.target_snapshot("main").unwrap();
        assert_eq!(
            snapshot.binding.attachment,
            AttachmentBinding {
                target: None,
                generation: 1
            }
        );
        assert!(snapshot.targets.is_empty());
        let target = FocusTarget {
            source: "test".into(),
            instance: "one".into(),
            activity: "b".into(),
        };
        gui.attach("main", &snapshot.binding, &target, 1).unwrap();
        let now = Instant::now();
        store
            .register(
                ("test".into(), "one".into()),
                ["a", "b"]
                    .into_iter()
                    .map(|activity| Activity {
                        activity: activity.into(),
                        sequence: 1,
                        state: ActivityState::Working,
                        attention: false,
                        reason: None,
                        summary: None,
                    })
                    .collect(),
                now,
            )
            .unwrap();
        let before = store.presentations(now);
        assert_eq!(before[0].runtime_binding.generation, 2);
        assert_eq!(
            before[0].runtime_binding.target.as_ref().unwrap().activity,
            "a"
        );
        assert!(before[0].preference.is_none());
        let rows = store.live_targets();
        assert_eq!(rows[1].incarnation, 1);
        assert!(rows[1].claimed_by.is_none());
        // Registering the first live source binds main before this queued intent commits.
        assert!(
            apply_familiar_command(&mut store, receiver.try_recv().unwrap(), &controls).is_err()
        );
        assert_eq!(store.presentations(now), before);
        assert_eq!(store.live_targets(), rows);
        assert!(store.preference_document().is_none());
        publish_target_memo(&store, &controls.service_instance, &cache, &mut None);
        assert_eq!(
            gui.target_snapshot("main").unwrap().binding.attachment,
            before[0].runtime_binding
        );
        assert_eq!(store.expire(now + crate::activity::SOURCE_TIMEOUT).len(), 1);
    }

    #[test]
    fn picker_initial_memo_is_published_before_callback_and_target_only_updates_are_visible() {
        let (_dir, path) = fixture();
        let (control_tx, control_rx) = mpsc::sync_channel::<FamiliarControl>(1);
        let (result_tx, result_rx) = mpsc::channel();
        let callback_control = std::cell::OnceCell::new();
        let server = Server::start_with_familiars(
            &path,
            move |views| {
                let gui =
                    callback_control.get_or_init(|| control_rx.recv_timeout(IO_TIMEOUT).unwrap());
                let snapshot = gui.target_snapshot("main").unwrap();
                result_tx.send((views, snapshot)).unwrap();
            },
            |_| {},
        )
        .unwrap();
        let gui = server.familiar_control();
        control_tx.send(gui.clone()).unwrap();
        let (initial, snapshot) = result_rx.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(initial[0].runtime_binding, snapshot.binding.attachment);
        assert!(snapshot.binding.attachment.target.is_none());
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        let (bound, first) = result_rx.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(bound[0].runtime_binding, first.binding.attachment);
        producer
            .update("b", 1, ActivityState::Working, false, None)
            .unwrap();
        let deadline = Instant::now() + IO_TIMEOUT;
        let latest = loop {
            if let Ok(snapshot) = gui.target_snapshot("main")
                && snapshot.targets.len() == 2
            {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert_eq!(latest.targets.len(), 2);
        assert_eq!(latest.binding, first.binding);
        assert!(result_rx.try_recv().is_err());
        let chosen = &latest.targets[1];
        gui.attach(
            "main",
            &latest.binding,
            &FocusTarget::from(&chosen.selection),
            chosen.incarnation,
        )
        .unwrap();
        let (attached, snapshot) = result_rx.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(attached[0].runtime_binding, snapshot.binding.attachment);
        assert_eq!(
            snapshot.binding.attachment.generation,
            first.binding.attachment.generation + 1
        );
        assert_eq!(snapshot.targets[1].claimed_by.as_deref(), Some("main"));
        assert!(snapshot.targets[0].claimed_by.is_none());
        assert!(attached[0].preference.is_none());
        drop(server);
    }

    #[test]
    fn preferences_gui_queue_validates_bindings_and_remains_bounded() {
        let (sender, receiver) = mpsc::sync_channel(32);
        let control = FamiliarControl {
            sender,
            target_memo: TargetCache::default(),
        };
        let binding = PreferenceBinding {
            target: crate::activity::FocusTarget {
                source: "source".into(),
                instance: "one".into(),
                activity: "task".into(),
            },
            generation: 1,
        };
        let placement = Placement { x: 0.0, y: 100.0 };
        assert!(control.placement("main", &binding, 0, placement).is_err());
        assert!(
            control
                .placement(
                    "main",
                    &binding,
                    1,
                    Placement {
                        x: f64::NAN,
                        y: 0.0
                    }
                )
                .is_err()
        );
        let mut invalid = binding.clone();
        invalid.target.instance = "bad/id".into();
        assert!(control.placement("main", &invalid, 1, placement).is_err());
        invalid = binding.clone();
        invalid.generation = 0;
        assert!(control.placement("main", &invalid, 1, placement).is_err());
        assert!(receiver.try_recv().is_err());
        for sequence in 1..=31 {
            control
                .placement("main", &binding, sequence, placement)
                .unwrap();
        }
        control.forget_preferences("main", &binding).unwrap();
        assert!(control.forget_preferences("main", &binding).is_err());
        assert!(control.placement("main", &binding, 32, placement).is_err());
        drop(receiver);
        assert!(control.forget_preferences("main", &binding).is_err());
    }

    #[test]
    fn queued_gui_forget_cannot_delete_a_new_attachment_and_late_pending_drag_is_revoked() {
        let now = Instant::now();
        let mut store = Store::with_settings(true, FamiliarSettings::default());
        store.enable_preferences(
            preferences::Document {
                version: 1,
                records: ["a", "b"]
                    .into_iter()
                    .map(|activity| preferences::Record {
                        source: "test".into(),
                        activity: activity.into(),
                        settings: FamiliarSettings {
                            font_size: 24.0,
                            ..Default::default()
                        },
                        placement: None,
                    })
                    .collect(),
            },
            true,
        );
        store
            .register(
                ("test".into(), "one".into()),
                ["a", "b"]
                    .into_iter()
                    .map(|activity| Activity {
                        activity: activity.into(),
                        sequence: 1,
                        state: ActivityState::Working,
                        attention: false,
                        reason: None,
                        summary: None,
                    })
                    .collect(),
                now,
            )
            .unwrap();
        let controls = ControlContext {
            diagnostics: RuntimeDiagnostics::new(true),
            settings_support: true,
            targeted: TargetedControls::default(),
            multi: true,
            service_instance: "test-service".into(),
        };
        let (sender, receiver) = mpsc::sync_channel(32);
        let gui = FamiliarControl {
            sender,
            target_memo: TargetCache::default(),
        };
        let captured_a = store.presentations(now)[0]
            .preference
            .as_ref()
            .unwrap()
            .binding
            .clone();
        gui.forget_preferences("main", &captured_a).unwrap();
        store.focus_for("main", "test", Some("one"), "b").unwrap();
        let before = store.preference_document().unwrap().1.encode().unwrap();
        let views = store.presentations(now);
        assert!(
            apply_familiar_command(&mut store, receiver.try_recv().unwrap(), &controls).is_err()
        );
        assert_eq!(
            store.preference_document().unwrap().1.encode().unwrap(),
            before
        );
        assert_eq!(store.presentations(now), views);

        let captured_b = views[0].preference.as_ref().unwrap().binding.clone();
        for _ in 0..32 {
            gui.next("main").unwrap();
        }
        let pending = Placement { x: 300.0, y: 400.0 };
        assert!(gui.placement("main", &captured_b, 1, pending).is_err());
        // The GUI retained this intent locally while its queue was full.
        for _ in 0..32 {
            receiver.try_recv().unwrap();
        }
        gui.forget_preferences("main", &captured_b).unwrap();
        apply_familiar_command(&mut store, receiver.try_recv().unwrap(), &controls).unwrap();
        let forgotten = store.presentations(now);
        assert!(
            forgotten[0].preference.as_ref().unwrap().binding.generation > captured_b.generation
        );
        gui.placement("main", &captured_b, 1, pending).unwrap();
        assert!(
            apply_familiar_command(&mut store, receiver.try_recv().unwrap(), &controls).is_err()
        );
        assert_eq!(store.presentations(now), forgotten);
        let document = store.preference_document().unwrap().1;
        assert_eq!(document.records.len(), 1);
        assert_eq!(document.records[0].activity, "a");
    }

    #[test]
    fn mixed_capacity_batch_persists_existing_key_and_reports_blocked_after_save() {
        let (dir, path) = fixture();
        let preference_path = dir.path().join("preferences.json");
        let document = preferences::Document {
            version: 1,
            records: (0..128)
                .map(|n| preferences::Record {
                    source: if n == 0 {
                        "test".into()
                    } else {
                        format!("source-{n}")
                    },
                    activity: "a".into(),
                    settings: FamiliarSettings::default(),
                    placement: None,
                })
                .collect(),
        };
        fs::write(&preference_path, document.encode().unwrap()).unwrap();
        fs::set_permissions(&preference_path, fs::Permissions::from_mode(0o600)).unwrap();
        let server = Server::start_graphical_with_preferences(
            &path,
            |_| {},
            |_| {},
            None,
            || {},
            false,
            FamiliarSettings::default(),
            Some(preference_path.clone()),
        )
        .unwrap();
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        for activity in ["a", "b"] {
            producer
                .update(activity, 1, ActivityState::Working, false, None)
                .unwrap();
        }
        let opened = control(&path, json!({"version":1,"type":"familiar.open","items":[{"source":"test","activity":"a"},{"source":"test","activity":"b"}]})).unwrap();
        let ids = &opened["opened"];
        control(
            &path,
            json!({"version":1,"type":"familiar.settings","ids":ids,"patch":{"font_size":24}}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
            let status: preferences::Status =
                serde_json::from_value(info["diagnostics"]["preferences"].clone()).unwrap();
            assert!(status.valid());
            if status.durable_revision == 1
                && status.write == preferences::WriteState::Blocked
                && status.reason == Some(preferences::Reason::CapacityReached)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "capacity status lost after mixed batch: {status:?}"
            );
            thread::sleep(TICK);
        }
        let saved = preferences::Document::decode(&fs::read(&preference_path).unwrap()).unwrap();
        assert_eq!(saved.records.len(), 128);
        assert_eq!(
            saved
                .records
                .iter()
                .find(|r| r.source == "test")
                .unwrap()
                .settings
                .font_size,
            24.0
        );
        assert!(
            !saved
                .records
                .iter()
                .any(|r| r.source == "test" && r.activity == "b")
        );
        for record in saved.records.iter().filter(|r| r.source != "test") {
            assert_eq!(record.settings, FamiliarSettings::default());
        }
        let rows = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        assert!(
            rows["familiars"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["settings"]["font_size"] == 24.0)
        );
        control(
            &path,
            json!({"version":1,"type":"familiar.settings","ids":[ids[0]],"patch":{"font_size":26}}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = server.diagnostics().snapshot().preferences.unwrap();
            if status.durable_revision == 2
                && status.write == preferences::WriteState::Clean
                && status.reason.is_none()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "valid edit did not clear capacity status: {status:?}"
            );
            thread::sleep(TICK);
        }
    }

    #[test]
    fn preferences_loaded_before_open_gui_and_wire_edits_share_book_and_stop_drains() {
        let (dir, path) = fixture();
        let preference_path = dir.path().join("preferences.json");
        let defaults = FamiliarSettings::default();
        let saved = FamiliarSettings {
            font_size: 20.0,
            ..defaults
        };
        let placement = Placement {
            x: -100.0,
            y: 200.0,
        };
        let document = preferences::Document {
            version: 1,
            records: vec![preferences::Record {
                source: "test".into(),
                activity: "a".into(),
                settings: saved,
                placement: Some(placement),
            }],
        };
        fs::write(&preference_path, document.encode().unwrap()).unwrap();
        fs::set_permissions(&preference_path, fs::Permissions::from_mode(0o600)).unwrap();
        let (tx, views) = mpsc::channel();
        let server = Server::start_graphical_with_preferences(
            &path,
            move |v| {
                let _ = tx.send(v);
            },
            |_| {},
            None,
            || {},
            false,
            defaults,
            Some(preference_path.clone()),
        )
        .unwrap();
        assert!(views.recv_timeout(IO_TIMEOUT).unwrap().is_empty());
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        assert!(views.try_recv().is_err());
        let opened = control(
            &path,
            json!({"version":1,"type":"familiar.open","items":[{"source":"test","activity":"a"}]}),
        )
        .unwrap();
        let id = opened["opened"][0].as_str().unwrap();
        let view = views.recv_timeout(IO_TIMEOUT).unwrap().remove(0);
        assert_eq!(view.settings, saved);
        let preference = view.preference.unwrap();
        assert_eq!(preference.placement, Some(placement));
        assert!(preference.writable);
        let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        assert!(
            list["familiars"][0]["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!("preferences"))
        );
        server
            .familiar_control()
            .settings(
                id,
                SettingsPatch {
                    font_size: Some(24.0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            views.recv_timeout(IO_TIMEOUT).unwrap()[0]
                .settings
                .font_size,
            24.0
        );
        control(
            &path,
            json!({"version":1,"type":"familiar.settings","ids":[id],"patch":{"sprite_scale":1.0}}),
        )
        .unwrap();
        let view = views.recv_timeout(IO_TIMEOUT).unwrap().remove(0);
        assert_eq!(view.settings.font_size, 24.0);
        assert_eq!(view.settings.sprite_scale, 1.0);
        // This accepted GUI command must be drained even if stop beats the next tick.
        server
            .familiar_control()
            .placement(id, &preference.binding, 1, Placement { x: 500.0, y: 600.0 })
            .unwrap();
        drop(server);
        let document = preferences::Document::decode(&fs::read(&preference_path).unwrap()).unwrap();
        assert_eq!(document.records.len(), 1);
        assert_eq!(document.records[0].settings, view.settings);
        assert_eq!(
            document.records[0].placement,
            Some(Placement { x: 500.0, y: 600.0 })
        );
    }

    #[test]
    fn legacy_settings_constructor_never_enables_preferences() {
        let (_dir, path) = fixture();
        let server = Server::start_graphical_with_settings(
            &path,
            |_| {},
            |_| {},
            None,
            || {},
            true,
            FamiliarSettings::default(),
        )
        .unwrap();
        assert_eq!(
            server.diagnostics().snapshot().preferences,
            Some(preferences::Status::disabled())
        );
        assert!(
            control(
                &path,
                json!({"version":1,"type":"familiar.preferences.forget","ids":["main"]})
            )
            .is_err()
        );
        let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        let caps = list["familiars"][0]["capabilities"].as_array().unwrap();
        assert!(caps.contains(&json!("settings")));
        assert!(!caps.contains(&json!("preferences")));
    }

    #[test]
    fn wire_forget_keeps_live_snapshot_and_heartbeat_does_not_resave() {
        let (dir, path) = fixture();
        let preference_path = dir.path().join("preferences.json");
        let (tx, views) = mpsc::channel();
        let server = Server::start_graphical_with_preferences(
            &path,
            move |v| {
                let _ = tx.send(v);
            },
            |_| {},
            None,
            || {},
            false,
            FamiliarSettings::default(),
            Some(preference_path.clone()),
        )
        .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        control(
            &path,
            json!({"version":1,"type":"familiar.open","items":[{"source":"test","activity":"a"}]}),
        )
        .unwrap();
        let opened = views.recv_timeout(IO_TIMEOUT).unwrap().remove(0);
        control(&path, json!({"version":1,"type":"familiar.settings","ids":[opened.id],"patch":{"font_size":24}})).unwrap();
        let live = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(
            control(
                &path,
                json!({"version":1,"type":"familiar.preferences.forget","ids":[opened.id]})
            )
            .unwrap(),
            json!({"forgotten":[opened.id]})
        );
        producer.heartbeat().unwrap();
        let forgotten = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(forgotten[0].settings, live[0].settings);
        assert_eq!(forgotten[0].presentation, live[0].presentation);
        assert!(
            forgotten[0].preference.as_ref().unwrap().binding.generation
                > live[0].preference.as_ref().unwrap().binding.generation
        );
        assert!(views.try_recv().is_err());
        let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        assert_eq!(list["familiars"][0]["settings"], json!(live[0].settings));
        drop(server);
        let document = preferences::Document::decode(&fs::read(preference_path).unwrap()).unwrap();
        assert!(document.records.is_empty());
    }

    #[test]
    fn invalid_startup_settings_do_not_create_a_listener() {
        let (_dir, path) = fixture();
        assert!(
            Server::start_graphical_with_settings(
                &path,
                |_| {},
                |_| {},
                None,
                || {},
                true,
                FamiliarSettings {
                    sprite_scale: f64::NAN,
                    ..Default::default()
                },
            )
            .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn settings_gui_queue_validates_before_enqueue_and_preserves_close() {
        let (sender, receiver) = mpsc::sync_channel(32);
        let handle = FamiliarControl {
            sender,
            target_memo: TargetCache::default(),
        };
        let patch = SettingsPatch {
            font_size: Some(24.0),
            ..Default::default()
        };
        assert!(handle.settings("bad/id", patch).is_err());
        assert!(handle.settings("main", SettingsPatch::default()).is_err());
        for invalid in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(
                handle
                    .settings(
                        "main",
                        SettingsPatch {
                            font_size: Some(invalid),
                            ..Default::default()
                        },
                    )
                    .is_err()
            );
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        handle.close("main").unwrap();
        for _ in 0..31 {
            handle.settings("main", patch).unwrap();
        }
        assert!(handle.clone().settings("main", patch).is_err());
        assert!(handle.next("main").is_err());
        assert!(matches!(receiver.try_recv().unwrap(), FamiliarCommand::Close(id) if id == "main"));
        for _ in 0..31 {
            assert!(matches!(
                receiver.try_recv().unwrap(),
                FamiliarCommand::Settings { id, patch: queued } if id == "main" && queued == patch
            ));
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(receiver);
        assert!(handle.settings("main", patch).is_err());
    }

    #[test]
    fn settings_gui_queue_updates_canonical_store_and_complete_selected_snapshot() {
        let (_dir, path) = fixture();
        let (tx, views) = mpsc::channel();
        let defaults = FamiliarSettings {
            sprite_scale: 0.75,
            font_size: 18.0,
            bubble_duration: 3.0,
        };
        let server = Server::start_graphical_with_settings(
            &path,
            move |v| tx.send(v).unwrap(),
            |_| {},
            None,
            || {},
            true,
            defaults,
        )
        .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        producer
            .update("b", 1, ActivityState::Working, false, None)
            .unwrap();
        control(&path, json!({"version":1,"type":"familiar.open","items":[{"id":"second","source":"test","activity":"b"}]})).unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        producer
            .update_with_summary("b", 2, ActivityState::Failed, false, None, Some("Keep cue"))
            .unwrap();
        let before = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert!(before[1].presentation.cue.is_some());
        let list_before = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        let patch = SettingsPatch {
            font_size: Some(24.0),
            bubble_duration: Some(2.5),
            ..Default::default()
        };
        server.familiar_control().settings("second", patch).unwrap();
        let after = views.recv_timeout(IO_TIMEOUT).unwrap();
        let mut expected = before;
        expected[1].settings = patch.apply(defaults);
        assert_eq!(after, expected);
        let mut expected_list = list_before;
        expected_list["familiars"][1]["settings"] = json!(patch.apply(defaults));
        assert_eq!(
            control(&path, json!({"version":1,"type":"familiar.list"})).unwrap(),
            expected_list
        );
        producer.heartbeat().unwrap();
        assert!(views.try_recv().is_err());
    }

    #[test]
    fn settings_gui_queue_ignores_closed_and_unknown_ids_without_resurrection() {
        let (_dir, path) = fixture();
        let (tx, views) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let server = Server::start_graphical_with_settings(
            &path,
            move |v| {
                let initial = !v.is_empty();
                tx.send(v).unwrap();
                if initial {
                    gate.recv_timeout(REGISTER_TIMEOUT).unwrap();
                }
            },
            |_| {},
            None,
            || {},
            true,
            FamiliarSettings::default(),
        )
        .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        let handle = server.familiar_control();
        handle.close("main").unwrap();
        let patch = SettingsPatch {
            font_size: Some(24.0),
            ..Default::default()
        };
        handle.settings("main", patch).unwrap();
        handle.settings("missing", patch).unwrap();
        release.send(()).unwrap();
        assert!(views.recv_timeout(IO_TIMEOUT).unwrap().is_empty());
        let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        assert_eq!(list["familiars"], json!([]));
        assert!(views.try_recv().is_err());
    }

    #[test]
    fn settings_gui_queue_rejects_legacy_and_headless_workers() {
        // Include settings-enabled but headless to test both worker gates.
        for (multi, settings) in [(false, false), (true, false), (false, true)] {
            let (_dir, path) = fixture();
            let (release, gate) = mpsc::channel();
            let server = Server::start_inner_with_settings(
                &path,
                move |_| gate.recv_timeout(REGISTER_TIMEOUT).unwrap(),
                None,
                multi.then(|| Box::new(|_| {}) as Box<dyn Fn(Vec<FamiliarPresentation>) + Send>),
                TargetedControls::default(),
                true,
                settings.then(FamiliarSettings::default),
            )
            .unwrap();
            server
                .familiar_control()
                .settings(
                    "main",
                    SettingsPatch {
                        font_size: Some(24.0),
                        ..Default::default()
                    },
                )
                .unwrap();
            release.send(()).unwrap();
            let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
            assert_eq!(
                list["familiars"][0]["settings"],
                json!(FamiliarSettings::default())
            );
        }
    }

    #[test]
    fn settings_socket_roundtrip_capabilities_and_constructor_gating() {
        for mode in 0..4 {
            for navigation in [false, true] {
                let (_dir, path) = fixture();
                let (tx, views) = mpsc::channel();
                let on_views = move |v| {
                    let _ = tx.send(v);
                };
                let nav: Option<Box<GoToMouse>> = navigation.then(|| {
                    Box::new(|_| -> () { panic!("unexpected navigation") }) as Box<GoToMouse>
                });
                let defaults = FamiliarSettings {
                    font_size: 18.0,
                    ..Default::default()
                };
                let _server = match mode {
                    0 => Server::start(&path, |_| {}),
                    1 => Server::start_graphical(&path, on_views, |_| {}, nav, || {}, true),
                    _ => Server::start_graphical_with_settings(
                        &path,
                        on_views,
                        |_| {},
                        nav,
                        || {},
                        mode == 2,
                        defaults,
                    ),
                }
                .unwrap();
                let mut producer = Publisher::connect(&path, "test", "one").unwrap();
                producer
                    .update("task", 1, ActivityState::Working, false, None)
                    .unwrap();
                if mode == 3 {
                    assert_eq!(views.recv_timeout(IO_TIMEOUT).unwrap(), vec![]);
                    control(&path, json!({"version":1,"type":"familiar.open","items":[{"id":"view","source":"test","activity":"task"}]})).unwrap();
                }
                let id = if mode == 3 { "view" } else { "main" };
                let info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
                assert_eq!(info["settings"], mode >= 2);
                assert_eq!(info["graphical"], mode != 0);
                assert_eq!(info["familiar_count"], 1);
                assert_eq!(info["source_count"], 1);
                let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
                let caps = list["familiars"][0]["capabilities"].as_array().unwrap();
                assert_eq!(caps.contains(&json!("settings")), mode >= 2);
                assert_eq!(
                    caps.contains(&json!("go-to-mouse")),
                    mode != 0 && navigation
                );
                assert_eq!(
                    list["familiars"][0]["settings"]["font_size"],
                    if mode >= 2 { 18.0 } else { 12.0 }
                );
                let request = json!({"version":1,"type":"familiar.settings","ids":[id],"patch":{"font_size":24,"bubble_duration":2.5}});
                if mode < 2 {
                    assert!(
                        control(&path, request)
                            .unwrap_err()
                            .to_string()
                            .contains("settings requires live graphical settings support")
                    );
                    assert_eq!(
                        control(&path, json!({"version":1,"type":"familiar.list"})).unwrap(),
                        list
                    );
                    continue;
                }
                let mut stale = request.clone();
                stale["service_instance"] = json!("stale-service");
                assert!(
                    control(&path, stale)
                        .unwrap_err()
                        .to_string()
                        .contains("service instance changed")
                );
                assert_eq!(
                    control(&path, json!({"version":1,"type":"familiar.list"})).unwrap(),
                    list
                );
                assert_eq!(
                    control(&path, request.clone()).unwrap(),
                    json!({"updated":[id]})
                );
                let mut latest = views.recv_timeout(IO_TIMEOUT).unwrap();
                while latest
                    .iter()
                    .find(|v| v.id == id)
                    .is_none_or(|v| v.settings.font_size != 24.0)
                {
                    latest = views.recv_timeout(IO_TIMEOUT).unwrap();
                }
                let selected = latest.iter().find(|v| v.id == id).unwrap();
                assert_eq!(selected.settings.sprite_scale, defaults.sprite_scale);
                assert_eq!(selected.settings.bubble_duration, 2.5);
                // Controls on a registered producer connection remain controls.
                assert_eq!(
                    producer.exchange(request).unwrap(),
                    Some(json!({"updated":[id]}))
                );
                producer
                    .update("other", 1, ActivityState::Working, false, None)
                    .unwrap();
                control(&path, json!({"version":1,"type":"familiar.open","items":[{"id":"later","source":"test","activity":"other"}]})).unwrap();
                let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
                let later = list["familiars"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["id"] == "later")
                    .unwrap();
                assert_eq!(later["settings"], json!(defaults));
                drop(producer);
                control(&path, json!({"version":1,"type":"familiar.settings","ids":[id],"patch":{"sprite_scale":1.0}})).unwrap();
            }
        }
    }

    #[test]
    fn listener_instances_are_unique_across_same_process_listeners_and_restarts() {
        let (_dir, path) = fixture();
        let other_path = path.with_file_name("other.sock");
        let server = Server::start(&path, |_| {}).unwrap();
        let _other = Server::start(&other_path, |_| {}).unwrap();
        let request = json!({"version":1,"type":"companion.service"});
        let first = control(&path, request.clone()).unwrap();
        let other = control(&other_path, request.clone()).unwrap();
        drop(server);
        let _restarted = Server::start(&path, |_| {}).unwrap();
        let restarted = control(&path, request).unwrap();
        for info in [&first, &other, &restarted] {
            assert_eq!(info["pid"], std::process::id());
            protocol::validate_id(info["service_instance"].as_str().unwrap()).unwrap();
        }
        assert_ne!(first["service_instance"], other["service_instance"]);
        assert_ne!(first["service_instance"], restarted["service_instance"]);
        assert_ne!(other["service_instance"], restarted["service_instance"]);
        assert!(
            control(
                &path,
                json!({"version":1,"type":"companion.service",
            "service_instance":first["service_instance"]})
            )
            .unwrap_err()
            .to_string()
            .contains("service instance changed; reopen menu")
        );
    }

    #[test]
    fn stale_menu_cannot_close_reused_familiar_id_after_restart() {
        let (_dir, path) = fixture();
        let mut previous_instance = None;
        for _ in 0..2 {
            let server = Server::start_with_familiars_config(
                &path,
                |_| {},
                |_| panic!("unexpected reveal"),
                false,
            )
            .unwrap();
            let info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
            let mut producer = Publisher::connect(&path, "test", "producer-1").unwrap();
            producer
                .update("task", 1, ActivityState::Working, false, None)
                .unwrap();
            assert_eq!(
                control(
                    &path,
                    json!({"version":1,"type":"familiar.open",
                "items":[{"source":"test","activity":"task"}]})
                )
                .unwrap(),
                json!({"opened":["familiar-1"]})
            );
            if let Some(stale) = previous_instance {
                let before = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
                assert!(
                    control(
                        &path,
                        json!({"version":1,"type":"familiar.close",
                    "ids":["familiar-1"],"service_instance":stale})
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("service instance changed; reopen menu")
                );
                assert_eq!(
                    control(&path, json!({"version":1,"type":"familiar.list"})).unwrap(),
                    before
                );
            }
            previous_instance = Some(info["service_instance"].clone());
            drop(producer);
            drop(server);
        }
    }

    #[test]
    fn matching_service_preconditions_accept_producers_and_graphical_controls() {
        let (_dir, path) = fixture();
        let (reveal_tx, reveals) = mpsc::channel();
        let (quit_tx, quits) = mpsc::channel();
        let _server = Server::start_graphical(
            &path,
            |_| {},
            move |ids| reveal_tx.send(ids).unwrap(),
            None,
            move || quit_tx.send(()).unwrap(),
            false,
        )
        .unwrap();
        let info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
        let instance = &info["service_instance"];
        assert_eq!(
            control(
                &path,
                json!({"version":1,"type":"companion.service",
            "service_instance":instance})
            )
            .unwrap(),
            info
        );
        let mut producer = Publisher {
            stream: connect_private(&path).unwrap(),
        };
        producer
            .send(json!({"version":1,"type":"source.register","source":"test",
            "instance":"producer-1","activities":[],"service_instance":instance}))
            .unwrap();
        producer
            .send(
                json!({"version":1,"type":"activity.state","activity":"task",
            "sequence":1,"state":"working","service_instance":instance}),
            )
            .unwrap();
        producer.heartbeat().unwrap();
        assert_eq!(
            control(
                &path,
                json!({"version":1,"type":"familiar.open",
            "items":[{"source":"test","instance":"producer-1","activity":"task"}],
            "service_instance":instance})
            )
            .unwrap(),
            json!({"opened":["familiar-1"]})
        );
        control(
            &path,
            json!({"version":1,"type":"companion.reveal",
            "familiars":["familiar-1"],"service_instance":instance}),
        )
        .unwrap();
        assert_eq!(
            reveals.recv_timeout(IO_TIMEOUT).unwrap(),
            Some(vec![String::from("familiar-1")])
        );
        assert_eq!(
            control(
                &path,
                json!({"version":1,"type":"familiar.close",
            "ids":["familiar-1"],"service_instance":instance})
            )
            .unwrap(),
            json!({"closed":["familiar-1"]})
        );
        assert_eq!(
            control(
                &path,
                json!({"version":1,"type":"companion.quit",
            "service_instance":instance})
            )
            .unwrap(),
            json!({"quitting":true})
        );
        quits.recv_timeout(IO_TIMEOUT).unwrap();
        assert!(reveals.try_recv().is_err());
        assert!(quits.try_recv().is_err());
    }

    #[test]
    fn stale_service_preconditions_never_touch_store_callbacks_or_client_binding() {
        let packets = [
            json!({"type":"familiar.open","items":[{"source":"test","activity":"task"}]}),
            json!({"type":"familiar.close","ids":["main"]}),
            json!({"type":"familiar.attach","items":[{"id":"main","source":"test","activity":"task"}]}),
            json!({"type":"companion.focus","source":"test","activity":"other"}),
            json!({"type":"companion.next"}),
            json!({"type":"companion.reveal","familiars":["main"]}),
            json!({"type":"familiar.go-to-mouse","ids":["main"]}),
            json!({"type":"familiar.settings","ids":["main"],"patch":{"font_size":16}}),
            json!({"type":"companion.quit"}),
            json!({"type":"companion.service"}),
            json!({"type":"companion.status"}),
            json!({"type":"familiar.list"}),
            json!({"type":"companion.targets"}),
            json!({"type":"source.register","source":"new","instance":"producer-2","activities":[]}),
            json!({"type":"activity.state","activity":"task","sequence":3,"state":"working"}),
            json!({"type":"source.heartbeat"}),
            json!({"type":"source.snapshot","activities":[]}),
            json!({"type":"activity.remove","activity":"task","sequence":3}),
        ];
        for mut packet in packets {
            let now = Instant::now();
            let id = ("test".into(), "producer-1".into());
            let mut store = Store::default();
            let mut activity = Activity {
                activity: "task".into(),
                sequence: 1,
                state: ActivityState::Working,
                attention: false,
                reason: None,
                summary: Some("Keep summary".into()),
            };
            store
                .register(id.clone(), vec![activity.clone()], now)
                .unwrap();
            activity.sequence = 2;
            activity.state = ActivityState::Failed;
            store.apply(&id, Event::State { activity }, now).unwrap();
            let before = store.presentations(now);
            assert!(before[0].presentation.cue.is_some());
            let status = serde_json::to_value(store.status(0)).unwrap();
            let list = store.list(0);
            let targets = store.targets(0, None).unwrap();
            let (stream, mut peer) = UnixStream::pair().unwrap();
            stream.set_nonblocking(true).unwrap();
            peer.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
            let source = (packet["type"] != "source.register").then(|| id.clone());
            let mut client = Client {
                stream,
                input: Vec::new(),
                output: Vec::new(),
                written: 0,
                connected: now + Duration::from_secs(9),
                source: source.clone(),
                read_eof: false,
                drain_started: None,
                pending_quit: false,
                quit_flushed: None,
            };
            let controls = ControlContext {
                diagnostics: RuntimeDiagnostics::new(true),
                targeted: TargetedControls {
                    reveal: Some(Box::new(|_| panic!("stale reveal"))),
                    go_to_mouse: Some(Box::new(|_| panic!("stale navigation"))),
                    quit: Some(Box::new(|| panic!("stale quit"))),
                },
                multi: true,
                settings_support: true,
                service_instance: "current-service".into(),
            };
            packet["version"] = 1.into();
            // Case-only differences must also fail: this comparison is exact.
            packet["service_instance"] = "Current-service".into();
            writeln!(peer, "{packet}").unwrap();
            assert!(client.step_inner(
                &mut store,
                now + Duration::from_secs(9),
                &mut |_, _| panic!("stale request must not recompute presentations"),
                Some(&|| panic!("stale legacy reveal")),
                &controls,
            ));
            let mut response = String::new();
            std::io::BufRead::read_line(&mut io::BufReader::new(&mut peer), &mut response).unwrap();
            let ack: Ack = serde_json::from_str(&response).unwrap();
            assert!(!ack.ok, "{packet}");
            assert_eq!(
                ack.error.as_deref(),
                Some("service instance changed; reopen menu")
            );
            assert!(ack.data.is_none());
            assert_eq!(client.source, source);
            assert!(!client.pending_quit);
            assert!(client.quit_flushed.is_none());
            assert_eq!(store.presentations(now), before);
            assert_eq!(serde_json::to_value(store.status(0)).unwrap(), status);
            assert_eq!(store.list(0), list);
            assert_eq!(store.targets(0, None).unwrap(), targets);
            assert_eq!(
                store.expire(now + crate::activity::SOURCE_TIMEOUT),
                vec![id]
            );
            assert_eq!(serde_json::to_value(store.status(0)).unwrap()["total"], 0);
        }
    }

    #[test]
    fn graphical_quit_ack_precedes_peer_close_and_callback_is_once() {
        for bootstrap_main in [false, true] {
            let (_dir, path) = fixture();
            let (views_tx, views) = mpsc::channel();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = calls.clone();
            let (tx, quits) = mpsc::channel();
            let _server = Server::start_graphical(
                &path,
                move |v| views_tx.send(v).unwrap(),
                |_| {},
                None,
                move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tx.send(()).unwrap();
                },
                bootstrap_main,
            )
            .unwrap();
            assert_eq!(
                views.recv_timeout(IO_TIMEOUT).unwrap().len(),
                usize::from(bootstrap_main)
            );
            let info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
            protocol::validate_id(info["service_instance"].as_str().unwrap()).unwrap();
            assert_eq!(
                info,
                json!({"api_version":1,"service_instance":info["service_instance"],"pid":std::process::id(),"graphical":true,"lifecycle":true,"navigation":false,"settings":false,"preferences":false,"familiar_count":usize::from(bootstrap_main),"source_count":0,"diagnostics":_server.diagnostics().snapshot()})
            );
            let mut peer = Publisher {
                stream: connect_private(&path).unwrap(),
            };
            assert_eq!(
                peer.exchange(json!({"version":1,"type":"companion.quit"}))
                    .unwrap(),
                Some(json!({"quitting":true}))
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(quits.try_recv().is_err());
            drop(peer);
            quits.recv_timeout(IO_TIMEOUT).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            // A callback schedules UI exit; it does not stop the worker itself.
            assert_eq!(
                control(&path, json!({"version":1,"type":"companion.quit"})).unwrap(),
                json!({"quitting":true})
            );
            assert!(quits.recv_timeout(IO_TIMEOUT + TICK * 2).is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(views.try_recv().is_err());
        }
    }

    #[test]
    fn graphical_quit_wait_is_bounded_and_ignores_pipelined_requests() {
        let (_dir, path) = fixture();
        let (tx, quits) = mpsc::channel();
        let _server = Server::start_graphical(
            &path,
            |_| {},
            |_| {},
            None,
            move || tx.send(Instant::now()).unwrap(),
            false,
        )
        .unwrap();
        let mut peer = connect_private(&path).unwrap();
        let started = Instant::now();
        peer.write_all(b"{\"version\":1,\"type\":\"companion.quit\"}\n{\"version\":1,\"type\":\"companion.service\"}\n").unwrap();
        let mut response = String::new();
        std::io::BufRead::read_line(&mut io::BufReader::new(&mut peer), &mut response).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap(),
            json!({"version":1,"ok":true,"data":{"quitting":true}})
        );
        assert!(quits.try_recv().is_err());
        // An unrelated client remains usable while the quitting peer stays open.
        assert_eq!(
            control(&path, json!({"version":1,"type":"companion.service"})).unwrap()["graphical"],
            true
        );
        let called = quits.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(called.duration_since(started) >= IO_TIMEOUT);
        assert!(called.duration_since(started) < Duration::from_millis(1500));
        let mut extra = Vec::new();
        peer.read_to_end(&mut extra).unwrap();
        assert!(extra.is_empty());
        assert!(quits.recv_timeout(TICK * 2).is_err());
    }

    #[test]
    fn malformed_quit_never_schedules_shutdown_and_disconnected_quit_still_does() {
        let (_dir, path) = fixture();
        let (tx, quits) = mpsc::channel();
        let _server = Server::start_graphical(
            &path,
            |_| {},
            |_| {},
            Some(Box::new(|_| {})),
            move || tx.send(()).unwrap(),
            false,
        )
        .unwrap();
        let mut peer = connect_private(&path).unwrap();
        peer.write_all(b"{\"version\":1,\"type\":\"companion.quit\",}\n{\"version\":2,\"type\":\"companion.quit\"}\n{\"type\":\"companion.quit\"}\n").unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        peer.read_to_string(&mut response).unwrap();
        let acks: Vec<Ack> = response
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(acks.len(), 3);
        assert!(acks.iter().all(|ack| !ack.ok));
        assert!(quits.try_recv().is_err());
        assert_eq!(
            control(&path, json!({"version":1,"type":"companion.service"})).unwrap()["navigation"],
            true
        );
        let mut peer = connect_private(&path).unwrap();
        peer.write_all(b"{\"version\":1,\"type\":\"companion.quit\"}\n")
            .unwrap();
        // Acceptance still schedules shutdown even if the peer never reads its ACK.
        drop(peer);
        quits.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(quits.recv_timeout(TICK * 2).is_err());
    }

    #[test]
    fn quit_is_unsupported_without_shutdown_callback() {
        for mode in 0..4 {
            let (_dir, path) = fixture();
            let _server = match mode {
                0 => Server::start(&path, |_| {}),
                1 => Server::start_with_reveal(&path, |_| {}, || {}),
                2 => Server::start_with_familiars(&path, |_| {}, |_| {}),
                _ => Server::start_with_navigation(&path, |_| {}, |_| {}, |_| {}),
            }
            .unwrap();
            let mut producer = Publisher::connect(&path, "test", "one").unwrap();
            producer
                .update("task", 1, ActivityState::Working, false, None)
                .unwrap();
            let before = control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
            let request = json!({"version":1,"type":"companion.quit"});
            assert!(
                control(&path, request.clone())
                    .unwrap_err()
                    .to_string()
                    .contains("companion.quit unsupported")
            );
            assert!(
                producer
                    .exchange(request)
                    .unwrap_err()
                    .to_string()
                    .contains("companion.quit unsupported")
            );
            assert_eq!(
                control(&path, json!({"version":1,"type":"companion.status"})).unwrap(),
                before
            );
            producer.heartbeat().unwrap();
        }
    }

    #[test]
    fn configured_gui_workers_publish_empty_or_legacy_initial_registry() {
        for navigation in [false, true] {
            for bootstrap_main in [false, true] {
                let (_dir, path) = fixture();
                let (tx, views) = mpsc::channel();
                let on_views = move |v| {
                    tx.send(v).unwrap();
                };
                let _server = if navigation {
                    Server::start_with_navigation_config(
                        &path,
                        on_views,
                        |_| {},
                        |_| {},
                        bootstrap_main,
                    )
                } else {
                    Server::start_with_familiars_config(&path, on_views, |_| {}, bootstrap_main)
                }
                .unwrap();
                let initial = views.recv_timeout(IO_TIMEOUT).unwrap();
                assert_eq!(initial.len(), usize::from(bootstrap_main));
                if bootstrap_main {
                    assert_eq!(initial[0].id, "main");
                }
                let mut producer = Publisher::connect(&path, "test", "one").unwrap();
                producer
                    .update("first", 1, ActivityState::Working, false, None)
                    .unwrap();
                let status =
                    control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
                if bootstrap_main {
                    assert_eq!(status["focus"]["activity"], "first");
                    assert_eq!(views.recv_timeout(IO_TIMEOUT).unwrap()[0].id, "main");
                } else {
                    assert!(status["focus"].is_null());
                    assert_eq!(status["focus_available"], false);
                    assert_eq!(status["total"], 1);
                    assert!(views.try_recv().is_err());
                    let opened = control(&path, json!({"version":1,"type":"familiar.open","items":[{"source":"test","activity":"first"}]})).unwrap();
                    assert_eq!(opened, json!({"opened":["familiar-1"]}));
                    let snapshot = views.recv_timeout(IO_TIMEOUT).unwrap();
                    assert_eq!(snapshot.len(), 1);
                    assert_eq!(snapshot[0].id, "familiar-1");
                    assert_eq!(
                        snapshot[0].presentation.base.as_ref().unwrap().activity,
                        "first"
                    );
                }
            }
        }
    }

    #[test]
    fn registered_quit_does_not_touch_store_and_waits_for_ack_flush() {
        for supported in [false, true] {
            let now = Instant::now() - Duration::from_secs(9);
            let id = ("test".into(), "one".into());
            let mut store = Store::default();
            let mut activity = Activity {
                activity: "task".into(),
                sequence: 1,
                state: ActivityState::Working,
                attention: false,
                reason: None,
                summary: Some("Keep summary".into()),
            };
            store
                .register(id.clone(), vec![activity.clone()], now)
                .unwrap();
            activity.sequence = 2;
            activity.state = ActivityState::Failed;
            store.apply(&id, Event::State { activity }, now).unwrap();
            let before = store.presentations(now);
            assert!(before[0].presentation.cue.is_some());
            let status = serde_json::to_value(store.status(0)).unwrap();
            let list = store.list(0);
            let (mut stream, mut peer) = UnixStream::pair().unwrap();
            stream.set_nonblocking(true).unwrap();
            peer.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
            // Fill the server's send buffer so its first ACK write cannot finish.
            let padding = [b'x'; 4096];
            let mut filled = 0;
            loop {
                match stream.write(&padding) {
                    Ok(n) => {
                        assert!(n > 0);
                        filled += n;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("fill socket: {e}"),
                }
            }
            let mut client = Client {
                stream,
                input: Vec::new(),
                output: Vec::new(),
                written: 0,
                connected: now,
                source: Some(id.clone()),
                read_eof: false,
                drain_started: None,
                pending_quit: false,
                quit_flushed: None,
            };
            let targeted = TargetedControls {
                quit: supported.then(|| Box::new(|| {}) as Box<dyn Fn() + Send>),
                ..TargetedControls::default()
            };
            let controls = ControlContext {
                diagnostics: RuntimeDiagnostics::new(true),
                targeted,
                settings_support: false,
                multi: true,
                service_instance: "test-service".into(),
            };
            peer.write_all(b"{\"version\":1,\"type\":\"companion.quit\"}\n")
                .unwrap();
            assert!(client.step_inner(
                &mut store,
                Instant::now(),
                &mut |_, _| panic!("quit must not recompute presentations"),
                None,
                &controls,
            ));
            assert_eq!(client.pending_quit, supported);
            assert!(client.quit_flushed.is_none());
            assert!(!client.output.is_empty());
            let mut discarded = vec![0; filled];
            peer.read_exact(&mut discarded).unwrap();
            assert!(client.step_inner(
                &mut store,
                Instant::now(),
                &mut |_, _| panic!("quit must not recompute presentations"),
                None,
                &controls,
            ));
            let mut response = String::new();
            std::io::BufRead::read_line(&mut io::BufReader::new(&mut peer), &mut response).unwrap();
            let ack: Ack = serde_json::from_str(&response).unwrap();
            assert_eq!(ack.ok, supported);
            if supported {
                assert_eq!(ack.data, Some(json!({"quitting":true})));
                assert!(client.quit_flushed.is_some());
            } else {
                assert!(ack.error.unwrap().contains("companion.quit unsupported"));
            }
            assert_eq!(client.source, Some(id.clone()));
            assert_eq!(store.presentations(now), before);
            assert_eq!(serde_json::to_value(store.status(0)).unwrap(), status);
            assert_eq!(store.list(0), list);
            assert_eq!(
                store.expire(now + crate::activity::SOURCE_TIMEOUT),
                vec![id]
            );
        }
    }

    #[test]
    fn service_info_reports_worker_capabilities_before_and_after_registration() {
        for mode in 0..4 {
            let (_dir, path) = fixture();
            let _server = match mode {
                0 => Server::start(&path, |_| {}),
                1 => Server::start_with_reveal(&path, |_| {}, || {}),
                2 => Server::start_with_familiars(&path, |_| {}, |_| {}),
                _ => Server::start_with_navigation(&path, |_| {}, |_| {}, |_| {}),
            }
            .unwrap();
            let request = json!({"version":1,"type":"companion.service"});
            let info = control(&path, request.clone()).unwrap();
            protocol::validate_id(info["service_instance"].as_str().unwrap()).unwrap();
            let mut expected = json!({"api_version":1,"service_instance":info["service_instance"],"pid":std::process::id(),"graphical":mode >= 2,"lifecycle":mode >= 2,"navigation":mode == 3,"settings":false,"preferences":false,"familiar_count":1,"source_count":0,"diagnostics":_server.diagnostics().snapshot()});
            assert_eq!(info, expected);
            let mut peer = Publisher {
                stream: connect_private(&path).unwrap(),
            };
            assert!(
                peer.exchange(json!({"version":2,"type":"companion.service"}))
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported protocol version")
            );
            assert_eq!(
                peer.exchange(request.clone()).unwrap(),
                Some(expected.clone())
            );
            let mut producer = Publisher::connect(&path, "test", "one").unwrap();
            expected["source_count"] = json!(1);
            assert_eq!(producer.exchange(request).unwrap(), Some(expected));
            producer.heartbeat().unwrap();
        }
    }

    #[test]
    fn service_diagnostics_live_handle_preserves_generation_and_capabilities() {
        use crate::{
            diagnostics::{ShortcutReason, ShortcutState, ShortcutStatus},
            hotkey::Shortcut,
        };
        let (_dir, path) = fixture();
        let server = Server::start_with_navigation(&path, |_| {}, |_| {}, |_| {}).unwrap();
        let handle = server.diagnostics();
        let request = json!({"version":1,"type":"companion.service"});
        let initial = control(&path, request.clone()).unwrap();
        assert_eq!(
            initial["diagnostics"]["hotkeys"]["option_i"]["reason"],
            "not_reported"
        );
        for (state, reason) in [
            (
                ShortcutState::Uninitialized,
                Some(ShortcutReason::RegistrationPending),
            ),
            (ShortcutState::Registered, None),
        ] {
            handle.set_hotkey(Shortcut::Reveal, ShortcutStatus { state, reason });
            let info = control(&path, request.clone()).unwrap();
            let mut expected = initial.clone();
            expected["diagnostics"] = json!(handle.snapshot());
            assert_eq!(info, expected);
            assert_eq!(info["navigation"], true);
            assert_eq!(
                info["diagnostics"]["hotkeys"]["option_m"]["reason"],
                "not_reported"
            );
        }
    }

    #[test]
    fn service_counts_registered_identities_not_connections_and_unavailable_familiars() {
        let (_dir, path) = fixture();
        let _server = Server::start_with_familiars_config(&path, |_| {}, |_| {}, false).unwrap();
        let request = json!({"version":1,"type":"companion.service"});
        let mut unregistered = Publisher {
            stream: connect_private(&path).unwrap(),
        };
        assert_eq!(
            unregistered.exchange(request.clone()).unwrap().unwrap()["source_count"],
            0
        );
        let mut first = Publisher::connect(&path, "test", "one").unwrap();
        let _second = Publisher::connect(&path, "test", "two").unwrap();
        first
            .update("task", 1, ActivityState::Working, false, None)
            .unwrap();
        control(&path, json!({"version":1,"type":"familiar.open","items":[{"source":"test","instance":"one","activity":"task"}]})).unwrap();
        let info = control(&path, request.clone()).unwrap();
        assert_eq!(info["source_count"], 2);
        assert_eq!(info["familiar_count"], 1);
        first.remove("task", 2).unwrap();
        let info = control(&path, request.clone()).unwrap();
        assert_eq!(info["familiar_count"], 1);
        assert_eq!(info["source_count"], 2);
        control(
            &path,
            json!({"version":1,"type":"familiar.close","ids":["familiar-1"]}),
        )
        .unwrap();
        assert_eq!(control(&path, request).unwrap()["familiar_count"], 0);
    }

    #[test]
    fn service_complete_ack_fits_with_maximally_escaped_executable() {
        let (_dir, path) = fixture();
        let _server = Server::start(&path, |_| {}).unwrap();
        let mut info = control(&path, json!({"version":1,"type":"companion.service"})).unwrap();
        let mut snapshot = RuntimeDiagnostics::new(false).snapshot();
        snapshot.executable = Some("\"\\".repeat(512));
        snapshot.build.target = "t".repeat(256);
        snapshot.build.profile = "p".repeat(256);
        snapshot.hotkeys.option_i = crate::diagnostics::ShortcutStatus {
            state: crate::diagnostics::ShortcutState::Unavailable,
            reason: Some(crate::diagnostics::ShortcutReason::RegistrationFailed),
        };
        snapshot.hotkeys.option_m = crate::diagnostics::ShortcutStatus {
            state: crate::diagnostics::ShortcutState::Unavailable,
            reason: Some(crate::diagnostics::ShortcutReason::PointerInitializationFailed),
        };
        let mut preferences = preferences::Status::disabled();
        preferences.mode = preferences::Mode::Writable;
        preferences.load = preferences::LoadState::Loaded;
        preferences.write = preferences::WriteState::Clean;
        preferences.reason = None;
        preferences.accepted_revision = u64::MAX;
        preferences.durable_revision = u64::MAX;
        preferences.records = preferences::MAX_RECORDS;
        preferences.path = Some(format!("/{}", "\"".repeat(1023)));
        assert!(preferences.valid());
        snapshot.preferences = Some(preferences);
        info["diagnostics"] = json!(snapshot);
        info["service_instance"] = json!("s".repeat(96));
        info["familiar_count"] = json!(protocol::MAX_FAMILIARS);
        info["source_count"] = json!(MAX_SOURCES);
        let ack = json!({"version":1,"ok":true,"data":info});
        assert!(serde_json::to_vec(&ack).unwrap().len() < MAX_LINE);
    }

    #[test]
    fn service_query_does_not_touch_store_cues_or_source_liveness() {
        let now = Instant::now();
        let id = ("test".into(), "one".into());
        let mut store = Store::default();
        let mut activity = Activity {
            activity: "task".into(),
            sequence: 1,
            state: ActivityState::Working,
            attention: false,
            reason: None,
            summary: Some("Keep summary".into()),
        };
        store
            .register(id.clone(), vec![activity.clone()], now)
            .unwrap();
        activity.sequence = 2;
        activity.state = ActivityState::Failed;
        store.apply(&id, Event::State { activity }, now).unwrap();
        let before = store.presentations(now);
        assert!(before[0].presentation.cue.is_some());
        let status = serde_json::to_value(store.status(0)).unwrap();
        let list = store.list(0);
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            written: 0,
            connected: now,
            source: Some(id.clone()),
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        peer.write_all(b"{\"version\":1,\"type\":\"companion.service\"}\n")
            .unwrap();
        assert!(client.step_inner(
            &mut store,
            now + Duration::from_secs(9),
            &mut |_, _| panic!("service query must not recompute presentations"),
            None,
            &ControlContext {
                diagnostics: RuntimeDiagnostics::new(true),
                targeted: TargetedControls::default(),
                multi: true,
                settings_support: false,
                service_instance: "test-service".into(),
            }
        ));
        let mut response = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut peer), &mut response)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap(),
            json!({"version":1,"ok":true,"data":{"api_version":1,"service_instance":"test-service","pid":std::process::id(),"graphical":true,"lifecycle":true,"navigation":false,"settings":false,"preferences":false,"familiar_count":1,"source_count":1,"diagnostics":RuntimeDiagnostics::new(true).snapshot()}})
        );
        assert_eq!(client.source, Some(id.clone()));
        assert_eq!(store.presentations(now), before);
        assert_eq!(serde_json::to_value(store.status(0)).unwrap(), status);
        assert_eq!(store.list(0), list);
        assert_eq!(
            store.expire(now + crate::activity::SOURCE_TIMEOUT),
            vec![id]
        );
    }

    #[test]
    fn prepare_parent_preserves_data_and_rejects_unsafe_paths() {
        let (dir, path) = fixture();
        fs::write(&path, "keep existing data").unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        prepare_socket_parent(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"keep existing data");
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert!(control(&path, json!({"version":1,"type":"companion.service"})).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep existing data");
        let leaf = dir.path().join("private").join("startup.log");
        prepare_socket_parent(&leaf).unwrap();
        assert!(!leaf.exists());
        assert_eq!(
            fs::metadata(leaf.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert!(prepare_socket_parent(&dir.path().join("missing/nested/events.sock")).is_err());
        assert!(!dir.path().join("missing").exists());
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(leaf.parent().unwrap(), &link).unwrap();
        assert!(prepare_socket_parent(&link.join("events.sock")).is_err());
        let socket_link = dir.path().join("alias.sock");
        std::os::unix::fs::symlink(&path, &socket_link).unwrap();
        prepare_socket_parent(&socket_link).unwrap();
        assert!(
            control(
                &socket_link,
                json!({"version":1,"type":"companion.service"})
            )
            .is_err()
        );
        assert!(
            fs::symlink_metadata(&socket_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let socket = dir.path().join("live.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let socket_inode = fs::metadata(&socket).unwrap().ino();
        prepare_socket_parent(&socket).unwrap();
        assert_eq!(fs::metadata(&socket).unwrap().ino(), socket_inode);
        assert!(prepare_socket_parent(&path.join("events.sock")).is_err());
        assert!(prepare_socket_parent(Path::new(&"x".repeat(101))).is_err());
        assert!(prepare_socket_parent(Path::new("bad\0path")).is_err());
        assert!(prepare_socket_parent(Path::new("/dev/null")).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_socket_parent(&path).is_err());
        assert_eq!(fs::metadata(dir.path()).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::read(&path).unwrap(), b"keep existing data");
        assert!(control(&path, json!({"version":1,"type":"companion.service"})).is_err());
    }

    fn until(
        rx: &Receiver<Presentation>,
        predicate: impl Fn(&Presentation) -> bool,
    ) -> Presentation {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let view = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if predicate(&view) {
                return view;
            }
        }
    }

    #[test]
    fn navigation_socket_dispatch_is_atomic_and_does_not_change_views_or_bindings() {
        let (_dir, path) = fixture();
        let (tx, views) = mpsc::channel();
        let (motion_tx, motions) = mpsc::sync_channel(16);
        let _server = Server::start_with_navigation(
            &path,
            move |v| {
                tx.send(v).unwrap();
            },
            |_| {},
            move |ids| {
                motion_tx.try_send(ids).unwrap();
            },
        )
        .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        producer
            .update("b", 1, ActivityState::Working, false, None)
            .unwrap();
        control(&path, json!({"version":1,"type":"familiar.open","items":[{"id":"second","source":"test","activity":"b"}]})).unwrap();
        views.recv_timeout(IO_TIMEOUT).unwrap();
        producer
            .update_with_summary(
                "a",
                2,
                ActivityState::Failed,
                false,
                None,
                Some("Keep summary and cue"),
            )
            .unwrap();
        let before_views = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert!(before_views[0].presentation.cue.is_some());
        let list_request = json!({"version":1,"type":"familiar.list"});
        let before_list = control(&path, list_request.clone()).unwrap();
        for row in before_list["familiars"].as_array().unwrap() {
            let caps = row["capabilities"].as_array().unwrap();
            assert_eq!(
                caps,
                &vec![
                    json!("info"),
                    json!("attach"),
                    json!("close"),
                    json!("go-to-mouse")
                ]
            );
        }
        let status_request = json!({"version":1,"type":"companion.status"});
        let before_status = control(&path, status_request.clone()).unwrap();
        let request = json!({"version":1,"type":"familiar.go-to-mouse","ids":["second","main"]});
        assert_eq!(
            control(&path, request.clone()).unwrap(),
            json!({"moving":["second","main"]})
        );
        assert_eq!(
            motions.recv_timeout(IO_TIMEOUT).unwrap(),
            vec!["second", "main"]
        );
        // Registered producers can issue the same control without changing source binding.
        assert_eq!(
            producer.exchange(request).unwrap(),
            Some(json!({"moving":["second","main"]}))
        );
        assert_eq!(
            motions.recv_timeout(IO_TIMEOUT).unwrap(),
            vec!["second", "main"]
        );
        assert!(motions.try_recv().is_err());
        let mut peer = Publisher {
            stream: connect_private(&path).unwrap(),
        };
        for packet in [
            json!({"version":1,"type":"familiar.go-to-mouse","ids":["main","missing"]}),
            json!({"version":1,"type":"familiar.go-to-mouse","ids":["main","main"]}),
            json!({"version":1,"type":"familiar.go-to-mouse"}),
            json!({"version":1,"type":"familiar.go-to-mouse","ids":[]}),
        ] {
            assert!(peer.exchange(packet).is_err());
            assert!(motions.try_recv().is_err());
        }
        assert_eq!(control(&path, list_request).unwrap(), before_list);
        assert_eq!(control(&path, status_request).unwrap(), before_status);
        assert!(views.try_recv().is_err());
        producer.heartbeat().unwrap();
    }

    #[test]
    fn navigation_is_rejected_and_not_advertised_without_callback() {
        for mode in 0..3 {
            let (_dir, path) = fixture();
            let _server = match mode {
                0 => Server::start(&path, |_| {}).unwrap(),
                1 => Server::start_with_reveal(&path, |_| {}, || {}).unwrap(),
                _ => Server::start_with_familiars(&path, |_| {}, |_| {}).unwrap(),
            };
            let request = json!({"version":1,"type":"familiar.go-to-mouse","ids":["main"]});
            assert!(
                control(&path, request)
                    .unwrap_err()
                    .to_string()
                    .contains("go-to-mouse requires graphical navigation support")
            );
            let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
            assert_eq!(
                list["familiars"][0]["capabilities"],
                if mode == 2 {
                    json!(["info", "attach", "close"])
                } else {
                    json!([])
                }
            );
        }
    }

    #[test]
    fn navigation_control_does_not_refresh_producer_liveness() {
        let now = Instant::now();
        let id = ("test".into(), "one".into());
        let mut store = Store::default();
        store.register(id.clone(), vec![], now).unwrap();
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            written: 0,
            connected: now,
            source: Some(id.clone()),
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let targeted = TargetedControls {
            reveal: None,
            go_to_mouse: Some(Box::new(move |ids| tx.try_send(ids).unwrap())),
            quit: None,
        };
        peer.write_all(b"{\"version\":1,\"type\":\"familiar.go-to-mouse\",\"ids\":[\"main\"]}\n")
            .unwrap();
        assert!(client.step_inner(
            &mut store,
            now + Duration::from_secs(9),
            &mut |_, _| {},
            None,
            &ControlContext {
                diagnostics: RuntimeDiagnostics::new(true),
                targeted,
                settings_support: false,
                multi: true,
                service_instance: "test-service".into(),
            }
        ));
        assert_eq!(rx.try_recv().unwrap(), vec!["main"]);
        assert_eq!(client.source, Some(id.clone()));
        assert_eq!(
            store.expire(now + crate::activity::SOURCE_TIMEOUT),
            vec![id]
        );
    }

    #[test]
    fn focus_queue_is_bounded_and_nonblocking() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let focus = FocusControl {
            sender,
            target_memo: TargetCache::default(),
        };
        focus.next().unwrap();
        assert!(focus.clone().next().is_err());
    }

    #[test]
    fn familiar_queue_is_bounded_and_validates_ids() {
        let (sender, _receiver) = mpsc::sync_channel(32);
        let handle = FamiliarControl {
            sender,
            target_memo: TargetCache::default(),
        };
        assert!(handle.next("bad/id").is_err());
        for _ in 0..32 {
            handle.next("main").unwrap();
        }
        assert!(handle.close("main").is_err());
        assert!(handle.clone().next("main").is_err());
    }

    #[test]
    fn multi_familiar_socket_snapshots_lifecycle_and_targeted_reveal() {
        let (_dir, path) = fixture();
        let (tx, views) = mpsc::channel();
        let (rtx, reveals) = mpsc::channel();
        let server = Server::start_with_familiars(
            &path,
            move |v| {
                let _ = tx.send(v);
            },
            move |ids| {
                let _ = rtx.send(ids);
            },
        )
        .unwrap();
        let initial = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(
            initial,
            vec![FamiliarPresentation {
                runtime_binding: AttachmentBinding {
                    target: None,
                    generation: 1
                },
                preference: None,
                settings: FamiliarSettings::default(),
                id: "main".into(),
                preset: "djinn".into(),
                presentation: Presentation::default()
            }]
        );
        let mut producer = Publisher::connect(&path, "test", "one").unwrap();
        producer
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        let main = views.recv_timeout(IO_TIMEOUT).unwrap();
        producer
            .update("b", 1, ActivityState::Succeeded, false, None)
            .unwrap();
        assert!(views.try_recv().is_err());
        let opened = control(&path, json!({"version":1,"type":"familiar.open","items":[{"id":"second","source":"test","activity":"b"}]})).unwrap();
        assert_eq!(opened, json!({"opened":["second"]}));
        let both = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[0], main[0]);
        assert!(both[1].presentation.cue.is_none());
        let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
        let rows = list["familiars"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row["capabilities"], json!(["info", "attach", "close"]));
        }
        producer
            .update_with_summary(
                "b",
                2,
                ActivityState::Failed,
                false,
                None,
                Some("Only second"),
            )
            .unwrap();
        let changed = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(changed[0], both[0]);
        assert!(changed[1].presentation.cue.is_some());
        control(
            &path,
            json!({"version":1,"type":"companion.reveal","familiars":["second"]}),
        )
        .unwrap();
        assert_eq!(
            reveals.recv_timeout(IO_TIMEOUT).unwrap(),
            Some(vec!["second".into()])
        );
        control(&path, json!({"version":1,"type":"companion.reveal"})).unwrap();
        assert_eq!(reveals.recv_timeout(IO_TIMEOUT).unwrap(), None);
        assert!(
            control(
                &path,
                json!({"version":1,"type":"companion.reveal","familiars":["missing"]})
            )
            .is_err()
        );
        assert!(reveals.try_recv().is_err());
        assert!(views.try_recv().is_err());
        assert!(control(&path, json!({"version":1,"type":"companion.focus","familiar":"second","source":"test","activity":"a"})).is_err());
        let status = control(
            &path,
            json!({"version":1,"type":"companion.status","familiar":"second"}),
        )
        .unwrap();
        assert_eq!(status["familiar_id"], "second");
        assert_eq!(status["focus"]["activity"], "b");
        control(
            &path,
            json!({"version":1,"type":"familiar.close","ids":["main"]}),
        )
        .unwrap();
        let remaining = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "second");
        producer.heartbeat().unwrap();
        producer
            .update("a", 2, ActivityState::Succeeded, false, None)
            .unwrap();
        assert!(views.try_recv().is_err());
        server.familiar_control().next("second").unwrap();
        let cycled = views.recv_timeout(IO_TIMEOUT).unwrap();
        assert_eq!(cycled[0].presentation.base.as_ref().unwrap().activity, "a");
        assert!(cycled[0].presentation.cue.is_none());
        server.familiar_control().close("second").unwrap();
        assert!(views.recv_timeout(IO_TIMEOUT).unwrap().is_empty());
        producer.heartbeat().unwrap();
        producer
            .update("b", 3, ActivityState::Working, false, None)
            .unwrap();
        assert!(views.try_recv().is_err());
        let status = control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
        assert_eq!(status["focus"], serde_json::Value::Null);
        assert_eq!(status["total"], 2);
    }

    #[test]
    fn headless_and_legacy_gui_reject_new_lifecycle_controls_but_allow_discovery() {
        for legacy_gui in [false, true] {
            let (_dir, path) = fixture();
            let _server = if legacy_gui {
                Server::start_with_reveal(&path, |_| {}, || {}).unwrap()
            } else {
                Server::start(&path, |_| {}).unwrap()
            };
            for request in [
                json!({"version":1,"type":"familiar.open","items":[{"source":"a","activity":"b"}]}),
                json!({"version":1,"type":"familiar.attach","items":[{"id":"main","source":"a","activity":"b"}]}),
                json!({"version":1,"type":"familiar.close","ids":["main"]}),
            ] {
                assert!(
                    control(&path, request)
                        .unwrap_err()
                        .to_string()
                        .contains("no graphical familiar")
                );
            }
            let list = control(&path, json!({"version":1,"type":"familiar.list"})).unwrap();
            assert_eq!(list["familiars"][0]["id"], "main");
            assert_eq!(list["familiars"][0]["attachment"], serde_json::Value::Null);
            assert_eq!(list["familiars"][0]["capabilities"], json!([]));
            assert_eq!(
                control(&path, json!({"version":1,"type":"companion.targets"})).unwrap()["total"],
                0
            );
            assert!(
                control(
                    &path,
                    json!({"version":1,"type":"companion.reveal","familiars":["main"]})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn reveal_roundtrips_without_registration_or_presentation_changes() {
        let (_dir, path) = fixture();
        let (views_tx, views) = mpsc::channel();
        let (reveal_tx, reveals) = mpsc::sync_channel(16);
        let server = Server::start_with_reveal(
            &path,
            move |p| views_tx.send(p).unwrap(),
            move || reveal_tx.try_send(()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            views.recv_timeout(IO_TIMEOUT).unwrap(),
            Presentation::default()
        );
        let request = json!({"version":1,"type":"companion.reveal"});
        let empty = control(&path, request.clone()).unwrap();
        assert_eq!(empty["total"], 0);
        assert_eq!(empty["focus"], serde_json::Value::Null);
        reveals.recv_timeout(IO_TIMEOUT).unwrap();
        assert!(views.try_recv().is_err());

        let mut publisher = Publisher::connect(&path, "test", "one").unwrap();
        publisher
            .update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        until(&views, |p| p.base.is_some());
        publisher
            .update("b", 1, ActivityState::Working, false, None)
            .unwrap();
        control(
            &path,
            json!({"version":1,"type":"companion.focus","source":"test","activity":"b"}),
        )
        .unwrap();
        until(&views, |p| {
            p.base.as_ref().is_some_and(|s| s.activity == "b")
        });
        publisher
            .update_with_summary(
                "b",
                2,
                ActivityState::Succeeded,
                false,
                None,
                Some("Selected conversation"),
            )
            .unwrap();
        let before = until(&views, |p| p.cue.is_some());
        let status = control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
        for _ in 0..3 {
            assert_eq!(control(&path, request.clone()).unwrap(), status);
            reveals.recv_timeout(IO_TIMEOUT).unwrap();
            assert_eq!(
                publisher.exchange(request.clone()).unwrap().unwrap(),
                status
            );
            reveals.recv_timeout(IO_TIMEOUT).unwrap();
        }
        assert_eq!(before.base.as_ref().unwrap().activity, "b");
        assert_eq!(
            before.cue.as_ref().unwrap().selection.summary.as_deref(),
            Some("Selected conversation")
        );
        assert!(views.try_recv().is_err());
        assert!(reveals.try_recv().is_err());
        // The ordinary producer path still works on the same connection.
        publisher.heartbeat().unwrap();
        publisher
            .update("b", 3, ActivityState::Working, false, None)
            .unwrap();
        let after = until(&views, |p| {
            p.base
                .as_ref()
                .is_some_and(|s| s.state == ActivityState::Working)
        });
        assert_eq!(after.cue, before.cue);
        drop(server);
    }

    #[test]
    fn headless_reveal_is_rejected_clearly() {
        let (_dir, path) = fixture();
        let server = Server::start(&path, |_| {}).unwrap();
        let request = json!({"version":1,"type":"companion.reveal"});
        assert!(
            control(&path, request.clone())
                .unwrap_err()
                .to_string()
                .contains("no graphical familiar")
        );
        let mut publisher = Publisher::connect(&path, "test", "one").unwrap();
        assert!(
            publisher
                .exchange(request)
                .unwrap_err()
                .to_string()
                .contains("no graphical familiar")
        );
        publisher.heartbeat().unwrap();
        publisher
            .update("task", 1, ActivityState::Working, false, None)
            .unwrap();
        drop(server);
    }

    #[test]
    fn malformed_reveal_and_invalid_version_do_not_invoke_callback() {
        let (_dir, path) = fixture();
        let (tx, rx) = mpsc::sync_channel(4);
        let server =
            Server::start_with_reveal(&path, |_| {}, move || tx.try_send(()).unwrap()).unwrap();
        let mut peer = UnixStream::connect(&path).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        peer.write_all(b"{\"version\":1,\"type\":\"companion.reveal\",}\n{\"version\":2,\"type\":\"companion.reveal\"}\n{\"type\":\"companion.reveal\"}\n").unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        peer.read_to_string(&mut response).unwrap();
        let acks: Vec<Ack> = response
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(acks.len(), 3);
        assert!(acks.iter().all(|ack| !ack.ok));
        assert!(
            acks[1]
                .error
                .as_ref()
                .unwrap()
                .contains("unsupported protocol version")
        );
        assert!(rx.try_recv().is_err());
        control(&path, json!({"version":1,"type":"companion.reveal"})).unwrap();
        rx.recv_timeout(IO_TIMEOUT).unwrap();
        assert!(rx.try_recv().is_err());
        drop(server);
    }

    #[test]
    fn registered_controls_do_not_refresh_liveness_or_claim_sources() {
        let now = Instant::now();
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let id = ("test".into(), "one".into());
        let mut store = Store::default();
        store
            .register(
                id.clone(),
                vec![Activity {
                    activity: "task".into(),
                    sequence: 1,
                    state: ActivityState::Working,
                    attention: false,
                    reason: None,
                    summary: None,
                }],
                now,
            )
            .unwrap();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            written: 0,
            connected: now,
            source: Some(id.clone()),
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        peer.write_all(b"{\"version\":1,\"type\":\"companion.focus\",\"source\":\"test\",\"activity\":\"task\"}\n{\"version\":1,\"type\":\"companion.next\"}\n{\"version\":1,\"type\":\"companion.status\"}\n{\"version\":1,\"type\":\"companion.reveal\"}\n").unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let reveal = || {
            calls.fetch_add(1, Ordering::Relaxed);
        };
        assert!(client.step(
            &mut store,
            now + Duration::from_secs(9),
            &mut |_, _| {},
            Some(&reveal)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(client.source, Some(id.clone()));
        assert_eq!(
            store.expire(now + crate::activity::SOURCE_TIMEOUT),
            vec![id]
        );
    }

    #[test]
    fn controls_roundtrip_without_claiming_sources_and_gui_cycle() {
        let (_dir, path) = fixture();
        let server = Server::start(&path, |_| {}).unwrap();
        let status = || control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
        assert_eq!(status()["focus"], serde_json::Value::Null);
        assert!(control(&path, json!({"version":1,"type":"companion.next"})).is_err());
        let mut a = Publisher::connect(&path, "producer", "one").unwrap();
        a.update("a", 1, ActivityState::Working, false, None)
            .unwrap();
        a.update("b", 1, ActivityState::Succeeded, false, None)
            .unwrap();
        assert_eq!(status()["focus"]["activity"], "a");
        let focused = control(
            &path,
            json!({"version":1,"type":"companion.focus","source":"producer","activity":"b"}),
        )
        .unwrap();
        assert_eq!(focused["focus"]["activity"], "b");
        assert_eq!(focused["focus_available"], true);
        let next = control(&path, json!({"version":1,"type":"companion.next"})).unwrap();
        assert_eq!(next["focus"]["activity"], "a");
        let mut b = Publisher::connect(&path, "producer", "two").unwrap();
        b.update("a", 1, ActivityState::Waiting, true, None)
            .unwrap();
        assert!(
            control(
                &path,
                json!({"version":1,"type":"companion.focus","source":"producer","activity":"a"})
            )
            .is_err()
        );
        control(&path, json!({"version":1,"type":"companion.focus","source":"producer","instance":"one","activity":"a"})).unwrap();
        let handle = server.focus_control();
        handle.clone().next().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while status()["focus"]["activity"] != "b" {
            assert!(Instant::now() < deadline);
            thread::sleep(TICK);
        }
        a.remove("b", 2).unwrap();
        let missing = status();
        assert_eq!(missing["focus"]["activity"], "b");
        assert_eq!(missing["focus_available"], false);
        control(&path, json!({"version":1,"type":"companion.next"})).unwrap();
        assert_eq!(status()["focus"]["instance"], "one");
        let empty = control(
            &path,
            json!({"version":1,"type":"companion.status","offset":usize::MAX}),
        )
        .unwrap();
        assert_eq!(empty["activities"], json!([]));
        assert_eq!(empty["next_offset"], serde_json::Value::Null);
        drop(server);
        assert!(handle.next().is_err());
    }

    #[test]
    fn maximum_status_is_paginated_and_fits_wire_limit() {
        let (_dir, path) = fixture();
        let server = Server::start(&path, |_| {}).unwrap();
        let mut publishers = Vec::new();
        for source in 0..MAX_SOURCES {
            let name = format!("{source:096}");
            let mut p = Publisher::connect(&path, &name, &"i".repeat(96)).unwrap();
            // Pipeline bounded frames so populating 512 records stays within liveness.
            let mut batch = Vec::new();
            for i in 0..protocol::MAX_ACTIVITIES {
                let event = json!({"version":1,"type":"activity.state","activity":format!("{i:096}"),"sequence":1,"state":"succeeded","reason":"\\".repeat(128),"summary":"\"".repeat(160)});
                batch.extend(serde_json::to_vec(&event).unwrap());
                batch.push(b'\n');
            }
            p.stream.write_all(&batch).unwrap();
            {
                use std::io::BufRead;
                let mut reader = io::BufReader::new(&mut p.stream);
                for _ in 0..protocol::MAX_ACTIVITIES {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert!(serde_json::from_str::<Ack>(&line).unwrap().ok);
                }
            }
            publishers.push(p);
        }
        let mut offset = 0;
        loop {
            let data = control(
                &path,
                json!({"version":1,"type":"companion.status","offset":offset}),
            )
            .unwrap();
            assert_eq!(data["total"], 512);
            assert_eq!(
                data["activities"].as_array().unwrap().len(),
                crate::activity::STATUS_PAGE_SIZE
            );
            assert!(
                serde_json::to_vec(&json!({"version":1,"ok":true,"data":data}))
                    .unwrap()
                    .len()
                    < MAX_LINE
            );
            let Some(next) = data["next_offset"].as_u64() else {
                break;
            };
            offset = next as usize;
        }
        assert_eq!(offset, 508);
        let registered = publishers[0]
            .exchange(json!({"version":1,"type":"companion.next"}))
            .unwrap()
            .unwrap();
        assert_eq!(registered["focus"]["activity"], format!("{:096}", 1));
        drop(server);
    }

    #[test]
    fn summary_updates_notify_only_for_selected_records_and_survive_disconnect() {
        let (_dir, path) = fixture();
        let (tx, rx) = mpsc::channel();
        let server = Server::start(&path, move |p| tx.send(p).unwrap()).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Presentation::default()
        );
        let mut publisher = Publisher::connect(&path, "test", "one").unwrap();
        publisher
            .update_with_summary(
                "a",
                1,
                ActivityState::Working,
                false,
                None,
                Some("First summary"),
            )
            .unwrap();
        let first = until(&rx, |p| p.base.is_some());
        assert_eq!(
            first.following.unwrap().summary.as_deref(),
            Some("First summary")
        );
        publisher
            .update_with_summary(
                "b",
                1,
                ActivityState::Working,
                false,
                None,
                Some("Background summary"),
            )
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
        assert!(
            publisher
                .update_with_summary(
                    "a",
                    2,
                    ActivityState::Working,
                    false,
                    None,
                    Some("\ninvalid")
                )
                .is_err()
        );
        publisher
            .update_with_summary(
                "a",
                2,
                ActivityState::Working,
                false,
                None,
                Some("Revised summary"),
            )
            .unwrap();
        let revised = until(&rx, |p| {
            p.base
                .as_ref()
                .is_some_and(|s| s.summary.as_deref() == Some("Revised summary"))
        });
        assert!(revised.cue.is_none());
        let status = control(&path, json!({"version":1,"type":"companion.status"})).unwrap();
        assert_eq!(status["activities"][0]["summary"], "Revised summary");
        assert_eq!(
            status["focus"],
            json!({"source":"test","instance":"one","activity":"a"})
        );
        drop(publisher);
        let missing = until(&rx, |p| p.base.is_none());
        assert!(missing.cue.is_none());
        let following = missing.following.unwrap();
        assert!(!following.available);
        assert_eq!(following.summary.as_deref(), Some("Revised summary"));
        drop(server);
    }

    #[test]
    fn publisher_roundtrip_callbacks_and_lifecycle() {
        let (_dir, path) = fixture();
        let (tx, rx) = mpsc::channel();
        let server = Server::start(&path, move |p| {
            tx.send(p).unwrap();
        })
        .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Presentation::default()
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let mut publisher = Publisher::connect(&path, "test", "one").unwrap();
        publisher
            .update("task", 1, ActivityState::Working, false, None)
            .unwrap();
        until(&rx, |p| {
            p.base
                .as_ref()
                .is_some_and(|s| s.state == ActivityState::Working)
        });
        publisher.heartbeat().unwrap();
        publisher
            .update("task", 2, ActivityState::Working, false, None)
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
        assert!(
            publisher
                .update("task", 2, ActivityState::Failed, false, None)
                .is_err()
        );
        publisher
            .update("task", 3, ActivityState::Failed, false, None)
            .unwrap();
        until(&rx, |p| p.cue.is_some());
        drop(publisher);
        let disconnected = until(&rx, |p| p.base.is_none());
        assert!(disconnected.cue.is_none());
        assert!(!disconnected.following.unwrap().available);
        let start = Instant::now();
        drop(server);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!path.exists());
    }

    #[test]
    fn collisions_and_unsafe_paths_are_preserved() {
        let (dir, path) = fixture();
        fs::write(&path, "do not delete").unwrap();
        assert!(Server::start(&path, |_| {}).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"do not delete");
        fs::remove_file(&path).unwrap();
        let server = Server::start(&path, |_| {}).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        assert!(Server::start(&path, |_| {}).is_err());
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        let publisher = Publisher::connect(&path, "a", "one").unwrap();
        assert!(Publisher::connect(&path, "a", "one").is_err());
        drop(publisher);
        drop(server);
        let target = dir.path().join("target");
        fs::write(&target, "safe").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(Server::start(&path, |_| {}).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn stale_socket_cleanup_and_inode_safe_drop() {
        let (_dir, path) = fixture();
        drop(UnixListener::bind(&path).unwrap());
        let server = Server::start(&path, |_| {}).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        drop(server);
        assert!(path.exists());
        drop(replacement);
    }

    #[test]
    fn private_parent_and_leaf_creation() {
        let (dir, path) = fixture();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Server::start(&path, |_| {}).is_err());
        assert_eq!(fs::metadata(dir.path()).unwrap().mode() & 0o777, 0o755);
        let leaf = dir.path().join("private").join("events.sock");
        let server = Server::start(&leaf, |_| {}).unwrap();
        assert_eq!(
            fs::metadata(leaf.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        drop(server);
        assert!(
            Server::start(
                &dir.path()
                    .join("missing")
                    .join("nested")
                    .join("events.sock"),
                |_| {}
            )
            .is_err()
        );
        assert!(check_path(Path::new(&"x".repeat(101))).is_err());
    }

    #[test]
    fn malformed_and_oversized_clients_do_not_kill_service() {
        let (_dir, path) = fixture();
        let server = Server::start(&path, |_| {}).unwrap();
        let mut bad = UnixStream::connect(&path).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        bad.write_all(b"not-json\n").unwrap();
        let mut byte = [0];
        let mut response = Vec::new();
        while bad.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            response.push(byte[0]);
        }
        let ack: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(ack["ok"], false);
        bad.write_all(&vec![b'x'; MAX_LINE + 1]).unwrap();
        thread::sleep(Duration::from_millis(100));
        let mut publisher = Publisher::connect(&path, "good", "one").unwrap();
        publisher.heartbeat().unwrap();
        drop(server);
    }

    #[test]
    fn unavailable_publisher_fails_promptly() {
        let (_dir, path) = fixture();
        let now = Instant::now();
        assert!(Publisher::connect(&path, "a", "one").is_err());
        assert!(now.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn half_closed_pipeline_drains_all_frames_and_ordered_acks() {
        for trailing_partial in [false, true] {
            let (_dir, path) = fixture();
            let (tx, rx) = mpsc::channel();
            let server = Server::start(&path, move |p| {
                tx.send(p).unwrap();
            })
            .unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                Presentation::default()
            );
            let mut peer = UnixStream::connect(&path).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            peer.set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut batch = b"{\"version\":1,\"type\":\"source.register\",\"source\":\"pipeline\",\"instance\":\"one\",\"activities\":[]}\n".to_vec();
            let mut expected = vec![true];
            for sequence in 1..=12 {
                let state = json!({"version":1,"type":"activity.state","activity":"task","sequence":sequence,"state":"working","reason":sequence.to_string()});
                batch.extend(serde_json::to_vec(&state).unwrap());
                batch.push(b'\n');
                batch.extend_from_slice(b"{\"version\":1,\"type\":\"source.heartbeat\"}\n");
                // A duplicate produces an error ACK, making ordering observable.
                batch.extend(serde_json::to_vec(&state).unwrap());
                batch.push(b'\n');
                expected.extend([true, true, false]);
            }
            if trailing_partial {
                batch.extend_from_slice(b"{\"version\":1,");
            }
            assert!(batch.len() < MAX_LINE);
            peer.write_all(&batch).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            let mut response = String::new();
            peer.read_to_string(&mut response).unwrap();
            assert!(response.ends_with('\n'));
            let acks: Vec<Ack> = response
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(acks.len(), expected.len());
            for (ack, expected_ok) in acks.into_iter().zip(expected) {
                assert_eq!(ack.version, 1);
                assert_eq!(ack.ok, expected_ok);
                if !expected_ok {
                    assert!(ack.error.unwrap().contains("sequence"));
                }
            }
            let mut updates = 0;
            loop {
                let view = rx.recv_timeout(Duration::from_secs(1)).unwrap();
                if view.base.is_none() {
                    assert!(view.cue.is_none());
                    assert!(!view.following.unwrap().available);
                    break;
                }
                updates += 1;
                assert_eq!(view.base.unwrap().reason, Some(updates.to_string()));
            }
            assert_eq!(updates, 12);
            drop(server);
        }
    }

    #[test]
    fn eof_preserves_buffered_frames_and_partial_output_cursor() {
        let now = Instant::now();
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let id: SourceId = ("test".into(), "one".into());
        let mut store = Store::default();
        store.register(id.clone(), vec![], now).unwrap();
        let mut client = Client {
            stream,
            input: b"{\"type\":\"source.heartbeat\",\"version\":1}\n".repeat(MESSAGES_PER_TICK + 1),
            output: b"already-sent\ntail\n".to_vec(),
            written: b"already-sent\n".len(),
            connected: now,
            source: Some(id),
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        assert!(client.step(&mut store, now, &mut |_, _| {}, None));
        assert!(client.read_eof);
        assert_eq!(client.input.iter().filter(|b| **b == b'\n').count(), 1);
        assert!(!client.step(&mut store, now + TICK, &mut |_, _| {}, None));
        drop(client);
        let mut response = String::new();
        peer.read_to_string(&mut response).unwrap();
        let mut lines = response.lines();
        assert_eq!(lines.next(), Some("tail"));
        let acks: Vec<Ack> = lines
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(acks.len(), MESSAGES_PER_TICK + 1);
        assert!(acks.iter().all(|ack| ack.ok && ack.version == 1));
    }

    #[test]
    fn eof_drain_with_nonreading_peer_has_a_fixed_deadline() {
        let now = Instant::now();
        let (mut stream, peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        // Fill the kernel send buffer so flushing the queued ACK would block.
        let chunk = [b'x'; 4096];
        let mut filled = 0;
        let mut fill_size = chunk.len();
        loop {
            match stream.write(&chunk[..fill_size]) {
                Ok(n) => {
                    assert!(n > 0);
                    filled += n;
                    assert!(filled <= 8 * 1024 * 1024);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && fill_size > 1 => fill_size = 1,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("fill socket send buffer: {e}"),
            }
        }
        peer.shutdown(Shutdown::Write).unwrap();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: b"{\"ok\":true,\"version\":1}\n".to_vec(),
            written: 0,
            connected: now,
            source: Some(("test".into(), "one".into())),
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        let mut store = Store::default();
        let queued = client.output.clone();
        assert!(client.step(&mut store, now, &mut |_, _| {}, None));
        assert_eq!(client.output, queued);
        assert_eq!(client.written, 0);
        assert!(client.step(
            &mut store,
            now + EOF_DRAIN_TIMEOUT - Duration::from_nanos(1),
            &mut |_, _| {},
            None
        ));
        assert_eq!(client.drain_started, Some(now));
        assert!(!client.step(&mut store, now + EOF_DRAIN_TIMEOUT, &mut |_, _| {}, None));
    }

    #[test]
    fn registration_deadline_is_exact() {
        let now = Instant::now();
        let (stream, _peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            written: 0,
            connected: now,
            source: None,
            read_eof: false,
            drain_started: None,
            pending_quit: false,
            quit_flushed: None,
        };
        let mut store = Store::default();
        assert!(client.step(
            &mut store,
            now + REGISTER_TIMEOUT - Duration::from_nanos(1),
            &mut |_, _| {},
            None
        ));
        assert!(!client.step(&mut store, now + REGISTER_TIMEOUT, &mut |_, _| {}, None));
    }

    #[test]
    fn ack_rejects_duplicate_keys_at_every_depth_without_echoing_peer_keys() {
        for response in [
            r#"{"version":0,"version":1,"ok":true}"#,
            r#"{"version":1,"ok":false,"ok":true}"#,
            r#"{"version":1,"ok":true,"data":{},"data":{}}"#,
            r#"{"version":1,"ok":true,"data":{"pid":0,"pid":123}}"#,
            r#"{"version":1,"ok":true,"data":{"diagnostics":{"executable":"bad\npath","executable":"/safe"}}}"#,
            r#"{"version":1,"ok":true,"data":{"diagnostics":{"build":{"target":"bad","target":"safe"}}}}"#,
            r#"{"version":1,"ok":true,"data":{"diagnostics":{"hotkeys":{"option_i":{"state":"disabled","state":"registered"}}}}}"#,
            r#"{"version":1,"ok":true,"data":{"diagnostics":{"hotkeys":{"option_m":{"reason":"headless","reason":null}}}}}"#,
            r#"{"version":1,"ok":true,"data":{"rows":[{"secret-peer-key":0,"secret-peer-key":1}]}}"#,
            r#"{"version":1,"ok":true,"unknown":[[ {"key":1,"k\u0065y":2} ]]}"#,
        ] {
            let error = parse_ack(response.as_bytes()).err().unwrap();
            let message = format!("{error:#}");
            assert!(message.contains("duplicate JSON object key"));
            assert!(!message.contains("secret-peer-key"));
            assert!(!message.contains("bad\\npath"));
            assert!(!message.contains("/safe"));
        }
    }

    #[test]
    fn ack_accepts_unique_optional_fields_and_independent_objects_but_stays_bounded() {
        let response = br#"{"version":1,"ok":true,"data":{"rows":[{"key":1},{"key":2}],"nested":{"key":null},"key":true,"numbers":[-1,1.25,18446744073709551615],"text":"safe"},"unknown":{"key":false}}"#;
        let ack = parse_ack(response).unwrap();
        assert_eq!(ack.version, 1);
        assert!(ack.ok);
        let expected: serde_json::Value = serde_json::from_slice(response).unwrap();
        assert_eq!(ack.data, Some(expected["data"].clone()));
        assert!(parse_ack(&vec![b' '; MAX_LINE + 1]).is_err());
        let deep = format!(
            "{{\"version\":1,\"ok\":true,\"data\":{}0{}}}",
            "[".repeat(128),
            "]".repeat(128)
        );
        assert!(parse_ack(deep.as_bytes()).is_err());
        assert!(parse_ack(br#"{"version":1,"ok":true} {}"#).is_err());
    }

    #[test]
    fn publisher_rejects_late_duplicate_ack_as_invalid_not_offline() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        peer.write_all(b"{\"version\":1,\"ok\":true}\n{\"version\":1,\"ok\":true,\"data\":{\"pid\":0,\"pid\":123}}\n").unwrap();
        let mut publisher = Publisher { stream };
        publisher.heartbeat().unwrap();
        let error = publisher
            .exchange(json!({"version":1,"type":"companion.service"}))
            .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate JSON object key"));
        assert!(!error.chain().any(|cause| cause.is::<io::Error>()));
    }

    #[test]
    fn publisher_validates_acknowledgments() {
        for response in [
            b"{\"version\":2,\"ok\":true}\n".as_slice(),
            b"{\"version\":1,\"ok\":false,\"error\":\"denied\"}\n".as_slice(),
            b"not-json\n".as_slice(),
        ] {
            let (stream, mut peer) = UnixStream::pair().unwrap();
            stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
            peer.write_all(response).unwrap();
            let mut publisher = Publisher { stream };
            assert!(publisher.heartbeat().is_err());
        }
    }
}
