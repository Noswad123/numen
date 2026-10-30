use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::activity::FocusTarget;
use anyhow::{Context, Result, bail, ensure};

const CAPACITY: usize = 4;
const MAX_RECENT: usize = 64;
const COOLDOWN: Duration = Duration::from_secs(2);
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(25);

/// Availability is a platform capability, not a PATH probe on the GUI thread.
pub const fn supported() -> bool {
    cfg!(target_os = "macos")
}

fn executable(path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("djinn"))
        .find_map(|candidate| {
            let absolute = candidate.canonicalize().ok()?;
            let metadata = absolute.metadata().ok()?;
            (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(absolute)
        })
}

fn argv(target: &FocusTarget) -> [OsString; 6] {
    [
        "session".into(),
        "float".into(),
        "--instance".into(),
        target.instance.clone().into(),
        "--session".into(),
        target.activity.clone().into(),
    ]
}

trait ChildAck {
    fn acknowledged(&mut self) -> Result<bool>;
    fn cancel(&mut self);
}

struct LaunchJob {
    child: std::process::Child,
    reaped: bool,
}

impl LaunchJob {
    fn spawn(command: &mut Command) -> Result<Self> {
        let child = command
            .process_group(0)
            .spawn()
            .context("start Djinn command")?;
        Ok(Self {
            child,
            reaped: false,
        })
    }
}

impl ChildAck for LaunchJob {
    fn acknowledged(&mut self) -> Result<bool> {
        match self.child.try_wait()? {
            Some(status) => {
                self.reaped = true;
                ensure!(status.success(), "Djinn command rejected");
                Ok(true)
            }
            None => Ok(false),
        }
    }
    fn cancel(&mut self) {
        if !self.reaped {
            // Only spawn() constructs this job, with PGID equal to its child's
            // PID. Signal before reaping: the PID cannot yet be reused for an
            // unrelated group. Descendants inherit this group unless they detach.
            let group = self.child.id() as libc::pid_t;
            debug_assert!(group > 1);
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
            self.reaped = self.child.wait().is_ok();
        }
    }
}

impl Drop for LaunchJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn await_ack(child: &mut impl ChildAck, stop: &AtomicBool, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            child.cancel();
            bail!("Djinn command cancelled or timed out");
        }
        match child.acknowledged() {
            Ok(true) => return Ok(()),
            Ok(false) => thread::sleep(POLL),
            Err(error) => {
                child.cancel();
                return Err(error);
            }
        }
    }
}

type Key = (String, String);
type Pending = Arc<Mutex<BTreeMap<Key, Option<Instant>>>>;

/// Admission is nonblocking. One worker serializes commands, never retries, and
/// only observes the CLI exit acknowledgment, not application/window success.
pub(crate) struct Dispatcher {
    sender: SyncSender<FocusTarget>,
    pending: Pending,
    stop: Arc<AtomicBool>,
    finished: mpsc::Receiver<()>,
}

impl Dispatcher {
    pub(crate) fn production(stop: Arc<AtomicBool>) -> Result<Self> {
        // Resolve once on the launcher thread, never from producer metadata.
        Self::start(stop, {
            let mut resolved = None;
            move |target, stop| {
                ensure!(supported(), "chat launch unsupported on this platform");
                let executable = resolved
                    .get_or_insert_with(|| {
                        std::env::var_os("PATH").and_then(|path| executable(&path))
                    })
                    .as_ref()
                    .context("trusted Djinn executable unavailable")?;
                let mut child = LaunchJob::spawn(
                    Command::new(executable)
                        .args(argv(target))
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null()),
                )?;
                await_ack(&mut child, stop, ACK_TIMEOUT)
            }
        })
    }

    pub(crate) fn start(
        stop: Arc<AtomicBool>,
        mut launch: impl FnMut(&FocusTarget, &AtomicBool) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<FocusTarget>(CAPACITY);
        let pending = Pending::default();
        let worker_pending = pending.clone();
        let worker_stop = stop.clone();
        let (finished, completion) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("numen-chat".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    let target = match receiver.recv_timeout(POLL) {
                        Ok(target) => target,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if launch(&target, &worker_stop).is_err() {
                        // Do not expose target identifiers, child output, or OS errors.
                        tracing::warn!(target: "numen::chat", "Chat launch failed, cancelled, or timed out");
                    }
                    worker_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert((target.instance, target.activity), Some(Instant::now()));
                }
                let _ = finished.try_send(());
            })?;
        Ok(Self {
            sender,
            pending,
            stop,
            finished: completion,
        })
    }

    pub(crate) fn enqueue(&self, target: &FocusTarget) -> Result<()> {
        ensure!(!self.stop.load(Ordering::Acquire), "chat launcher stopping");
        let mut pending = self
            .pending
            .try_lock()
            .map_err(|_| anyhow::anyhow!("chat launcher busy"))?;
        let now = Instant::now();
        pending.retain(|_, completed| completed.is_none_or(|at| now.duration_since(at) < COOLDOWN));
        let key = (target.instance.clone(), target.activity.clone());
        ensure!(
            !pending.contains_key(&key),
            "chat launch pending or cooling down"
        );
        ensure!(
            pending.len() < MAX_RECENT,
            "chat launch cooldown capacity reached"
        );
        self.sender
            .try_send(target.clone())
            .context("chat launch queue full or stopped")?;
        pending.insert(key, None);
        Ok(())
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Even a stalled OS spawn/reap must not hold up IPC/GUI shutdown.
        let _ = self.finished.recv_timeout(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(activity: &str) -> FocusTarget {
        FocusTarget {
            source: "djinn".into(),
            instance: "one".into(),
            activity: activity.into(),
        }
    }

    #[test]
    fn fixed_literal_argv_and_absolute_only_resolution() {
        let mut malicious = target("$(touch secret); --session=x");
        malicious.instance = "--help; open /Applications/Anything.app".into();
        assert_eq!(
            argv(&malicious),
            [
                "session",
                "float",
                "--instance",
                "--help; open /Applications/Anything.app",
                "--session",
                "$(touch secret); --session=x"
            ]
            .map(OsString::from)
        );
        assert!(executable(OsStr::new(":.:relative/bin")).is_none());
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("djinn");
        std::fs::write(&file, b"not executed").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(executable(directory.path().as_os_str()).is_none());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = std::env::join_paths([std::path::Path::new("."), directory.path()]).unwrap();
        assert_eq!(executable(&paths), Some(file.canonicalize().unwrap()));
    }

    struct FakeChild {
        cancelled: bool,
        polls: usize,
        acknowledge: bool,
    }
    impl ChildAck for FakeChild {
        fn acknowledged(&mut self) -> Result<bool> {
            self.polls += 1;
            Ok(self.acknowledge)
        }
        fn cancel(&mut self) {
            self.cancelled = true;
        }
    }

    #[test]
    #[ignore = "subprocess fixture used only by the process-group cancellation test"]
    fn process_group_helper() {
        match std::env::var("NUMEN_CHAT_TEST_HELPER").as_deref() {
            Ok("parent") => {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "chat::tests::process_group_helper",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("NUMEN_CHAT_TEST_HELPER", "descendant")
                    .stdin(Stdio::null())
                    .spawn()
                    .unwrap();
                let _ = child.wait();
            }
            Ok("descendant") => {
                use std::io::Write;
                println!("numen-chat-descendant-ready");
                std::io::stdout().flush().unwrap();
                thread::sleep(Duration::from_secs(30));
            }
            _ => {}
        }
    }

    #[test]
    fn timeout_and_shutdown_kill_owned_group_including_descendant() {
        use std::io::{BufRead, BufReader};

        for shutdown in [false, true] {
            let mut job = LaunchJob::spawn(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "chat::tests::process_group_helper",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("NUMEN_CHAT_TEST_HELPER", "parent")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            let pid = job.child.id() as libc::pid_t;
            // This job cannot signal Numen's own group or another existing job.
            assert_eq!(unsafe { libc::getpgid(pid) }, pid);
            assert_ne!(unsafe { libc::getpgrp() }, pid);
            let output = job.child.stdout.take().unwrap();
            let (sender, events) = mpsc::channel();
            let reader = thread::spawn(move || {
                for line in BufReader::new(output).lines() {
                    if line.unwrap() == "numen-chat-descendant-ready" {
                        let _ = sender.send("ready");
                    }
                }
                let _ = sender.send("eof");
            });
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)).unwrap(),
                "ready"
            );
            let stop = AtomicBool::new(shutdown);
            let timeout = if shutdown {
                ACK_TIMEOUT
            } else {
                Duration::from_millis(30)
            };
            assert!(await_ack(&mut job, &stop, timeout).is_err());
            assert!(job.reaped);
            assert!(!job.child.wait().unwrap().success());
            // Both the parent and descendant hold this pipe. EOF proves that
            // cancellation reached the descendant, not just the Rust parent.
            assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), "eof");
            reader.join().unwrap();
            job.cancel(); // Repeated cleanup must not signal a possibly reused PID.
        }
    }

    #[test]
    fn acknowledgment_timeout_and_shutdown_cancel_without_retry() {
        let stop = AtomicBool::new(false);
        let mut child = FakeChild {
            cancelled: false,
            polls: 0,
            acknowledge: false,
        };
        assert!(await_ack(&mut child, &stop, Duration::ZERO).is_err());
        assert!(child.cancelled);
        assert_eq!(child.polls, 0);
        child.cancelled = false;
        assert!(await_ack(&mut child, &stop, Duration::from_millis(30)).is_err());
        assert!(child.cancelled);
        assert!(child.polls > 0);
        child.cancelled = false;
        child.polls = 0;
        child.acknowledge = true;
        await_ack(&mut child, &stop, ACK_TIMEOUT).unwrap();
        assert_eq!(child.polls, 1);
        assert!(!child.cancelled);
        stop.store(true, Ordering::Release);
        assert!(await_ack(&mut child, &stop, ACK_TIMEOUT).is_err());
        assert!(child.cancelled);
        assert_eq!(child.polls, 1);
    }

    #[test]
    fn bounded_serial_queue_dedupe_cooldown_and_shutdown_discard() {
        let stop = Arc::new(AtomicBool::new(false));
        let (started, observed) = mpsc::channel();
        let dispatcher = Dispatcher::start(stop.clone(), move |target, stop| {
            started.send(target.clone()).unwrap();
            while !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        })
        .unwrap();
        dispatcher.enqueue(&target("first")).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            target("first")
        );
        assert!(dispatcher.enqueue(&target("first")).is_err());
        for n in 0..CAPACITY {
            dispatcher.enqueue(&target(&format!("queued-{n}"))).unwrap();
        }
        assert!(dispatcher.enqueue(&target("overflow")).is_err());
        assert!(
            !dispatcher
                .pending
                .lock()
                .unwrap()
                .contains_key(&("one".into(), "overflow".into()))
        );
        {
            let mut pending = dispatcher.pending.lock().unwrap();
            pending.insert(("one".into(), "cooling".into()), Some(Instant::now()));
        }
        assert!(
            dispatcher
                .enqueue(&target("cooling"))
                .unwrap_err()
                .to_string()
                .contains("cooling down")
        );
        dispatcher.pending.lock().unwrap().insert(
            ("one".into(), "cooling".into()),
            Some(Instant::now() - COOLDOWN),
        );
        assert!(
            dispatcher
                .enqueue(&target("cooling"))
                .unwrap_err()
                .to_string()
                .contains("queue full")
        );
        assert!(
            !dispatcher
                .pending
                .lock()
                .unwrap()
                .contains_key(&("one".into(), "cooling".into()))
        );
        let start = Instant::now();
        drop(dispatcher);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(observed.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
