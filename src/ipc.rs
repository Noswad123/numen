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
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::json;

use crate::{
    activity::{Presentation, SourceId, Store},
    protocol::{self, Activity, ActivityState, Event, MAX_LINE, MAX_SOURCES, Request},
};

const TICK: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(2);
const EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 16 * 1024;
const MESSAGES_PER_TICK: usize = 8;
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
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    _socket: SocketOwner,
    focus: FocusControl,
}

#[derive(Clone)]
pub struct FocusControl {
    sender: SyncSender<()>,
}

impl FocusControl {
    pub fn next(&self) -> Result<()> {
        self.sender.try_send(()).context("queue focus cycle")
    }
}

impl Server {
    pub fn focus_control(&self) -> FocusControl {
        self.focus.clone()
    }

    /// The callback runs on the service worker and must return promptly (for
    /// example, enqueue a GUI event). Drop wakes and joins that worker.
    pub fn start(path: &Path, on_change: impl Fn(Presentation) + Send + 'static) -> Result<Self> {
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
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::sync_channel(32);
        let worker = thread::Builder::new()
            .name("numen-ipc".into())
            .spawn(move || run(listener, worker_stop, receiver, on_change))?;
        Ok(Self {
            stop,
            worker: Some(worker),
            _socket: owner,
            focus: FocusControl { sender },
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
}

impl Client {
    fn step(
        &mut self,
        store: &mut Store,
        now: Instant,
        changed: &mut impl FnMut(&mut Store, Instant),
    ) -> bool {
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
            let result = Request::parse(&self.input[..end]).and_then(|request| {
                let event = match request.event {
                    Event::Status { offset } => {
                        return Ok(Some(serde_json::to_value(store.status(offset))?));
                    }
                    Event::Focus {
                        source,
                        instance,
                        activity,
                    } => {
                        store.focus(&source, instance.as_deref(), &activity)?;
                        return Ok(Some(serde_json::to_value(store.status(0))?));
                    }
                    Event::Next => {
                        store.cycle()?;
                        return Ok(Some(serde_json::to_value(store.status(0))?));
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
            changed(store, now);
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
    focus: Receiver<()>,
    on_change: impl Fn(Presentation),
) {
    let mut store = Store::default();
    let mut previous = Presentation::default();
    on_change(previous.clone());
    let mut changed = |store: &mut Store, now| {
        let view = store.presentation(now);
        if view != previous {
            previous = view.clone();
            on_change(view);
        }
    };
    let mut clients: Vec<Client> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        let expired = store.expire(now);
        clients.retain(|c| !c.source.as_ref().is_some_and(|id| expired.contains(id)));
        for _ in 0..MESSAGES_PER_TICK {
            if focus.try_recv().is_err() {
                break;
            }
            let _ = store.cycle();
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
                        });
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        clients.retain_mut(|client| {
            let alive = client.step(&mut store, now, &mut changed);
            if !alive {
                if let Some(id) = &client.source {
                    store.disconnect(id);
                }
                changed(&mut store, now);
            }
            alive
        });
        thread::park_timeout(TICK);
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
            Event::Status { .. } | Event::Focus { .. } | Event::Next
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
        let ack: Ack = serde_json::from_slice(&response).context("invalid Numen acknowledgment")?;
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
    fn focus_queue_is_bounded_and_nonblocking() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let focus = FocusControl { sender };
        focus.next().unwrap();
        assert!(focus.clone().next().is_err());
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
        };
        peer.write_all(b"{\"version\":1,\"type\":\"companion.focus\",\"source\":\"test\",\"activity\":\"task\"}\n{\"version\":1,\"type\":\"companion.next\"}\n{\"version\":1,\"type\":\"companion.status\"}\n").unwrap();
        assert!(client.step(&mut store, now + Duration::from_secs(9), &mut |_, _| {}));
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
        };
        assert!(client.step(&mut store, now, &mut |_, _| {}));
        assert!(client.read_eof);
        assert_eq!(client.input.iter().filter(|b| **b == b'\n').count(), 1);
        assert!(!client.step(&mut store, now + TICK, &mut |_, _| {}));
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
        };
        let mut store = Store::default();
        let queued = client.output.clone();
        assert!(client.step(&mut store, now, &mut |_, _| {}));
        assert_eq!(client.output, queued);
        assert_eq!(client.written, 0);
        assert!(client.step(
            &mut store,
            now + EOF_DRAIN_TIMEOUT - Duration::from_nanos(1),
            &mut |_, _| {}
        ));
        assert_eq!(client.drain_started, Some(now));
        assert!(!client.step(&mut store, now + EOF_DRAIN_TIMEOUT, &mut |_, _| {}));
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
        };
        let mut store = Store::default();
        assert!(client.step(
            &mut store,
            now + REGISTER_TIMEOUT - Duration::from_nanos(1),
            &mut |_, _| {}
        ));
        assert!(!client.step(&mut store, now + REGISTER_TIMEOUT, &mut |_, _| {}));
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
