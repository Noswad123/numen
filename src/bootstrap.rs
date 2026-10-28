//! Opt-in startup for menu commands. Discovery alone never changes the filesystem.
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::ipc;

const START_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LOG: u64 = 1024 * 1024;

#[derive(Deserialize, Clone, Debug)]
pub struct ServiceInfo {
    pub api_version: u32,
    pub service_instance: String,
    pub pid: u32,
    pub graphical: bool,
    pub lifecycle: bool,
    pub navigation: bool,
}

fn absent(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
        .any(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
        })
}

pub fn probe(path: &Path) -> Result<Option<ServiceInfo>> {
    let data = match ipc::control(
        path,
        serde_json::json!({"version":1,"type":"companion.service"}),
    ) {
        Ok(data) => data,
        Err(error) if absent(&error) => return Ok(None),
        Err(error) => {
            return Err(error)
                .context("cannot discover Numen service; refusing to start a replacement; review the error, restart an updated Numen manually, or choose another --socket");
        }
    };
    let info: ServiceInfo = serde_json::from_value(data)
        .context("invalid companion.service metadata; update/restart Numen manually")?;
    ensure!(
        info.api_version == 1,
        "unsupported Numen service API {}; update/restart Numen manually",
        info.api_version
    );
    ensure!(info.pid != 0, "invalid Numen service PID");
    crate::protocol::validate_id(&info.service_instance)
        .context("invalid Numen service instance; update/restart Numen manually")?;
    Ok(Some(info))
}

fn graphical(info: ServiceInfo) -> Result<ServiceInfo> {
    ensure!(
        info.graphical && info.lifecycle,
        "existing Numen service (PID {}) is headless or lacks graphical lifecycle support; stop it manually and run an updated Numen, or choose another --socket; it will not be replaced",
        info.pid
    );
    tracing::debug!(
        pid = info.pid,
        navigation = info.navigation,
        "Graphical Numen service available"
    );
    Ok(info)
}

fn private_log(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open private service log {}", path.display()))?;
    let metadata = file.metadata()?;
    // Inspect the opened descriptor, never chmod/truncate a foreign path target.
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1,
        "service log must be a private regular file owned by the current user: {}",
        path.display()
    );
    let current = fs::symlink_metadata(path)?;
    ensure!(
        current.dev() == metadata.dev() && current.ino() == metadata.ino(),
        "service log changed while opening"
    );
    if metadata.len() > MAX_LOG {
        file.set_len(0)
            .context("reset oversized private service log")?;
    }
    Ok(file)
}

fn service_command(executable: &Path, path: &Path, log: File) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("--socket")
        .arg(path)
        .args(["run", "--empty"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .env("RUST_LOG", "numen=info,wgpu=warn");
    // Only an async-signal-safe syscall is allowed between fork and exec. Do not
    // set process_group first: becoming a group leader would make setsid fail.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
}

/// Readiness means the control listener is ready, not that GPU initialization
/// has completed. Subsequent menu operations must still report service errors.
pub fn ensure_graphical(path: &Path) -> Result<ServiceInfo> {
    ensure_with(
        path,
        || std::env::current_exe().context("locate Numen executable"),
        START_TIMEOUT,
    )
}

fn ensure_with(
    path: &Path,
    executable: impl FnOnce() -> Result<std::path::PathBuf>,
    timeout: Duration,
) -> Result<ServiceInfo> {
    if let Some(info) = probe(path)? {
        return graphical(info);
    }
    ipc::prepare_socket_parent(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let log_path = parent.join("service.log");
    let log = private_log(&log_path)?;
    let mut child = service_command(&executable()?, path, log)
        .spawn()
        .with_context(|| format!("start empty Numen service; log: {}", log_path.display()))?;
    let result = wait_for_service(path, &mut child, timeout);
    match result {
        Ok(info) => {
            // The listener may belong to a concurrent menu's child. Never signal
            // its PID; reap only the child handle created by this invocation.
            thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(info)
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
                .with_context(|| format!("Numen startup failed; inspect {}", log_path.display()))
        }
    }
}

fn wait_for_service(path: &Path, child: &mut Child, timeout: Duration) -> Result<ServiceInfo> {
    let deadline = Instant::now() + timeout;
    let mut exited = None;
    loop {
        if let Some(info) = probe(path)? {
            return graphical(info);
        }
        if exited.is_none() {
            exited = child.try_wait().context("check Numen child startup")?;
        }
        // Even if our bind lost a race, allow the winning service to become ready.
        if Instant::now() >= deadline {
            if let Some(status) = exited {
                bail!("Numen child exited with {status} before a graphical service became ready");
            }
            bail!(
                "Numen service did not become ready within {} ms",
                timeout.as_millis()
            );
        }
        thread::sleep(
            Duration::from_millis(40).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::{
            fs::{PermissionsExt, symlink},
            net::UnixListener,
        },
    };

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("events.sock");
        (dir, socket)
    }

    fn mock(path: &Path, data: serde_json::Value) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap()["type"],
                "companion.service"
            );
            writeln!(
                stream,
                "{}",
                serde_json::json!({"version":1,"ok":true,"data":data})
            )
            .unwrap();
            // Keep the peer alive while control adjusts its per-read timeout.
            // Some Unix implementations reject setsockopt after peer teardown.
            let _ = std::io::Read::read(&mut stream, &mut [0]);
        })
    }

    fn metadata() -> serde_json::Value {
        serde_json::json!({"api_version":1,"service_instance":"test-service-1","pid":123,"graphical":true,"lifecycle":true,"navigation":true})
    }

    #[test]
    fn reuse_gui_without_resolving_executable_or_writing_log() {
        for navigation in [false, true] {
            let (_dir, socket) = fixture();
            let mut info = metadata();
            info["navigation"] = navigation.into();
            let server = mock(&socket, info);
            let ready = ensure_with(&socket, || panic!("must not spawn"), START_TIMEOUT).unwrap();
            assert_eq!(ready.pid, 123);
            assert_eq!(ready.service_instance, "test-service-1");
            assert_eq!(ready.navigation, navigation);
            server.join().unwrap();
            assert!(!socket.with_file_name("service.log").exists());
        }
    }

    #[test]
    fn headless_legacy_and_malformed_never_spawn() {
        for data in [
            {
                let mut v = metadata();
                v["graphical"] = false.into();
                v
            },
            {
                let mut v = metadata();
                v["lifecycle"] = false.into();
                v
            },
            {
                let mut v = metadata();
                v["api_version"] = 2.into();
                v
            },
            {
                let mut v = metadata();
                v["navigation"] = "yes".into();
                v
            },
            {
                let mut v = metadata();
                v.as_object_mut().unwrap().remove("service_instance");
                v
            },
            {
                let mut v = metadata();
                v["service_instance"] = "".into();
                v
            },
            {
                let mut v = metadata();
                v["service_instance"] = "bad/id".into();
                v
            },
            {
                let mut v = metadata();
                v["service_instance"] = "x".repeat(97).into();
                v
            },
            serde_json::json!({}),
        ] {
            let (_dir, socket) = fixture();
            let server = mock(&socket, data);
            assert!(ensure_with(&socket, || panic!("must not spawn"), START_TIMEOUT).is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn absence_is_only_refused_or_missing_io() {
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::ConnectionRefused] {
            assert!(absent(
                &anyhow::Error::new(io::Error::from(kind)).context("outer")
            ));
        }
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::InvalidData,
        ] {
            assert!(!absent(&io::Error::from(kind).into()));
        }
        assert!(!absent(&anyhow::anyhow!("not found")));
        let (_dir, socket) = fixture();
        assert!(probe(&socket).unwrap().is_none());
        fs::write(&socket, b"not a socket").unwrap();
        assert!(probe(&socket).is_err());
    }

    #[test]
    fn probe_does_not_create_directories_and_rejects_unsafe_parent() {
        let (dir, socket) = fixture();
        let absent_parent = dir.path().join("missing");
        assert!(probe(&absent_parent.join("events.sock")).unwrap().is_none());
        assert!(!absent_parent.exists());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(probe(&socket).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn private_log_guards_and_cap() {
        let (dir, _) = fixture();
        let path = dir.path().join("service.log");
        let file = private_log(&path).unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        file.set_len(MAX_LOG + 1).unwrap();
        assert_eq!(private_log(&path).unwrap().metadata().unwrap().len(), 0);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_log(&path).is_err());
        fs::remove_file(&path).unwrap();
        symlink(dir.path().join("target"), &path).unwrap();
        assert!(private_log(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(private_log(&path).is_err());
        fs::remove_dir(&path).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(private_log(&path).is_err());
    }

    #[test]
    fn command_has_empty_args_and_bounded_failure_uses_injected_executable() {
        let (dir, socket) = fixture();
        let log = private_log(&dir.path().join("service.log")).unwrap();
        let command = service_command(Path::new("/usr/bin/false"), &socket, log);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--socket"),
                socket.as_os_str(),
                std::ffi::OsStr::new("run"),
                std::ffi::OsStr::new("--empty")
            ]
        );
        let start = Instant::now();
        let error = ensure_with(
            &socket,
            || Ok("/usr/bin/false".into()),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("service.log"));
        let message = format!("{error:#}");
        assert!(
            message.contains("child exited") || message.contains("did not become ready"),
            "{error:#}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
