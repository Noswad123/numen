//! Print by default; installation is explicit and never rewrites existing TOML.
//! Replacing an existing catalog briefly removes its name, retaining both a
//! durable snapshot and the live original inode. Publication/recovery never
//! clobbers a recreated name; this is preservation-first, not an atomic CAS.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

const MAX_CATALOG: u64 = 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(clap::Args)]
pub struct ActionOptions {
    /// Install explicitly instead of printing a suggested action.
    #[arg(long)]
    pub install: bool,
    /// Catalog path for --install (parent must already exist; ignored for preview).
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Serialize)]
struct Action {
    id: &'static str,
    title: &'static str,
    kind: &'static str,
    command: Vec<String>,
}

#[derive(Serialize)]
struct Catalog {
    action: Vec<Action>,
}

fn stanza(executable: &Path, socket: &Path) -> Result<String> {
    ensure!(
        executable.is_absolute() && socket.is_absolute(),
        "Wisp command paths must be absolute"
    );
    let text = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .context("Wisp action paths must be valid UTF-8")
    };
    toml::to_string(&Catalog {
        action: vec![Action {
            id: "numen",
            title: "Numen",
            kind: "summon",
            command: vec![
                text(executable)?,
                "--socket".into(),
                text(socket)?,
                "menu".into(),
            ],
        }],
    })
    .context("serialize Wisp action")
}

pub fn run(options: ActionOptions, socket: Option<&Path>) -> Result<()> {
    let executable = std::env::current_exe().context("locate Numen executable")?;
    let socket = match socket {
        Some(path) => path.to_owned(),
        None => crate::ipc::default_socket_path()?,
    };
    let socket = std::path::absolute(socket).context("resolve absolute socket path")?;
    let entry = stanza(&executable, &socket)?;
    if !options.install {
        print!("{entry}");
        return Ok(());
    }
    let path = match options.file {
        Some(path) => resolve_catalog(path)?,
        None => resolve_catalog(configured_catalog()?)?,
    };
    match install(&path, &entry)? {
        Install::Unchanged => println!("Numen action already installed in {}", path.display()),
        Install::Written(backup) => {
            println!("Installed Numen action in {}", path.display());
            if let Some(backup) = backup {
                println!("Snapshot backup: {}", backup.snapshot.display());
                println!(
                    "Live inode backup (retains late editor writes): {}",
                    backup.displaced.display()
                );
            }
        }
    }
    Ok(())
}

fn resolve_catalog(path: PathBuf) -> Result<PathBuf> {
    let text = path
        .to_str()
        .context("catalog path must be UTF-8; pass --file PATH")?;
    ensure!(
        !text.contains('$'),
        "unresolved environment variable in catalog path; pass a resolved --file PATH (no shell evaluation)"
    );
    let path = if text == "~" || text.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .context("HOME is unset; pass an absolute --file PATH")?;
        ensure!(
            Path::new(&home).is_absolute(),
            "HOME must be absolute; pass an absolute --file PATH"
        );
        PathBuf::from(home).join(text.strip_prefix("~/").unwrap_or(""))
    } else {
        ensure!(
            !text.starts_with('~'),
            "unsupported home expansion; pass a resolved --file PATH"
        );
        path
    };
    std::path::absolute(path).context("resolve catalog path")
}

fn configured_catalog() -> Result<PathBuf> {
    let mut child = Command::new("wisp")
        .args(["actions", "path"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("run `wisp actions path`; alternatively pass --file PATH")?;
    let result = (|| -> Result<PathBuf> {
        let mut output = child.stdout.take().context("capture Wisp catalog path")?;
        let fd = output.as_raw_fd();
        // A nonblocking pipe plus deadline also bounds a tool that never exits.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "configure bounded Wisp output read"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = Vec::new();
        let mut eof = false;
        loop {
            let mut buffer = [0; 1024];
            match output.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    bytes.extend_from_slice(&buffer[..n]);
                    ensure!(
                        bytes.len() <= 16 * 1024,
                        "Wisp catalog path output is too large; pass --file PATH"
                    );
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error).context("read Wisp catalog path"),
            }
            if let Some(status) = child.try_wait()? {
                ensure!(
                    status.success(),
                    "wisp actions path failed ({status}); pass --file PATH"
                );
                if eof {
                    break;
                }
            }
            ensure!(
                Instant::now() < deadline,
                "wisp actions path timed out; pass --file PATH"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let text =
            String::from_utf8(bytes).context("Wisp catalog path is not UTF-8; pass --file PATH")?;
        let text = text.trim_end_matches(['\r', '\n']);
        ensure!(
            !text.is_empty() && !text.contains(['\r', '\n']),
            "invalid Wisp catalog path output; pass --file PATH"
        );
        Ok(text.into())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

fn guard(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "catalog must be a regular file, not a symlink/special file; pass the resolved target with --file PATH"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "catalog belongs to another user; pass your own catalog with --file PATH"
    );
    ensure!(
        metadata.len() <= MAX_CATALOG,
        "catalog exceeds 1 MiB read limit"
    );
    Ok(())
}

struct Baseline {
    bytes: Vec<u8>,
    metadata: Metadata,
}

fn read_catalog(path: &Path) -> Result<Option<Baseline>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => guard(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect catalog"),
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .context("open catalog without following symlinks; pass a resolved --file PATH")?;
    let metadata = file.metadata()?;
    guard(&metadata)?;
    let mut bytes = Vec::new();
    (&mut file).take(MAX_CATALOG + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CATALOG,
        "catalog exceeds 1 MiB read limit"
    );
    let baseline = Baseline { bytes, metadata };
    let current = fs::symlink_metadata(path)?;
    ensure!(
        same_metadata(&baseline.metadata, &current),
        "catalog changed while reading; retry"
    );
    Ok(Some(baseline))
}

fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    same_displaced_metadata(a, b) && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
}

// Moving the original inode changes ctime on some filesystems. Everything else
// must still match, and its actual bytes are checked separately after the move.
fn same_displaced_metadata(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
}

fn append(original: &[u8], entry: &str) -> Result<Option<Vec<u8>>> {
    let text = std::str::from_utf8(original).context("catalog is not valid UTF-8")?;
    let value: toml::Value =
        toml::from_str(text).context("catalog is not valid TOML; resolve it manually")?;
    let root = value
        .as_table()
        .context("catalog root must be a TOML table")?;
    let desired: toml::Value = toml::from_str(entry)?;
    let expected = &desired["action"][0];
    let mut matches = Vec::new();
    if let Some(actions) = root.get("action") {
        for action in actions
            .as_array()
            .context("catalog `action` must be an array of tables")?
        {
            let table = action
                .as_table()
                .context("catalog `action` entries must be tables")?;
            if table.get("id").and_then(toml::Value::as_str) == Some("numen") {
                matches.push(action);
            }
        }
    }
    ensure!(
        matches.len() <= 1,
        "catalog contains duplicate numen IDs; resolve manually (no --replace is supported)"
    );
    if let Some(existing) = matches.first() {
        ensure!(
            *existing == expected,
            "catalog already has a different numen action; resolve it manually (no --replace is supported)"
        );
        return Ok(None);
    }
    let mut bytes = original.to_vec();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(entry.as_bytes());
    // Appending after arbitrary valid TOML must still produce the intended root action.
    let combined: toml::Value = toml::from_str(std::str::from_utf8(&bytes)?)?;
    ensure!(
        combined["action"]
            .as_array()
            .is_some_and(|items| items.last() == Some(expected)),
        "cannot safely append root action to catalog"
    );
    Ok(Some(bytes))
}

struct Scratch {
    path: PathBuf,
    file: File,
    keep: bool,
}

impl Scratch {
    fn create(parent: &Path, label: &str) -> Result<Self> {
        for _ in 0..32 {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path = parent.join(format!(
                ".numen-{label}-{}-{stamp}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        keep: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).context("create unique catalog backup/staging file");
                }
            }
        }
        bail!("could not allocate unique catalog backup/staging file")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep
            && let (Ok(owned), Ok(current)) =
                (self.file.metadata(), fs::symlink_metadata(&self.path))
            && owned.dev() == current.dev()
            && owned.ino() == current.ino()
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Backups {
    snapshot: PathBuf,
    displaced: PathBuf,
}

enum Install {
    Unchanged,
    Written(Option<Backups>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallStep {
    SnapshotSynced,
    BeforeDisplace,
    DisplacedSynced,
    BeforePublish,
    PublishedSynced,
}

fn preserve_permissions(file: &File, metadata: &Metadata) -> Result<()> {
    // Keep the group before restoring mode: chown can clear special mode bits.
    if file.metadata()?.gid() != metadata.gid() {
        ensure!(
            unsafe { libc::fchown(file.as_raw_fd(), libc::uid_t::MAX, metadata.gid()) } == 0,
            "cannot preserve catalog group: {}",
            std::io::Error::last_os_error()
        );
    }
    file.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
    Ok(())
}

fn check_baseline(path: &Path, baseline: Option<&Baseline>) -> Result<()> {
    let current = read_catalog(path)?;
    ensure!(
        match (baseline, current.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => same_metadata(&a.metadata, &b.metadata) && a.bytes == b.bytes,
            _ => false,
        },
        "catalog changed concurrently; no replacement was made, retry after reviewing it"
    );
    Ok(())
}

fn install(path: &Path, entry: &str) -> Result<Install> {
    install_with_hook(path, entry, |_, _| Ok(()))
}

fn restore_displaced(path: &Path, displaced: &Path, directory: &File) -> String {
    let metadata = match fs::symlink_metadata(displaced) {
        Ok(metadata) => metadata,
        Err(error) => {
            return format!(
                "cannot inspect displaced node: {error}; recover manually from {}",
                displaced.display()
            );
        }
    };
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return format!(
            "displaced node is not a safe user-owned regular file; not followed, deleted, or restored; recover manually from {}",
            displaced.display()
        );
    }
    match fs::hard_link(displaced, path) {
        Ok(()) => match directory.sync_all() {
            Ok(()) => "restored displaced catalog without overwriting any current path".into(),
            Err(error) => format!("restored displaced catalog, but directory sync failed: {error}"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
            "catalog path already exists and was left untouched; displaced data remains in its live inode backup".into(),
        Err(error) => format!("no-clobber restore failed: {error}; recover manually from {}", displaced.display()),
    }
}

fn install_with_hook(
    path: &Path,
    entry: &str,
    mut hook: impl FnMut(InstallStep, Option<&Path>) -> Result<()>,
) -> Result<Install> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(
        parent.is_dir(),
        "catalog parent {} must already exist; create it explicitly with mkdir, then retry --file PATH",
        parent.display()
    );
    let baseline = read_catalog(path)?;
    let Some(bytes) = append(baseline.as_ref().map_or(&[], |b| b.bytes.as_slice()), entry)? else {
        return Ok(Install::Unchanged);
    };
    ensure!(
        bytes.len() as u64 <= MAX_CATALOG,
        "appending Numen would exceed the 1 MiB catalog limit"
    );
    let directory = File::open(parent).context("open catalog directory for syncing")?;
    let mut snapshot_path = None;
    let mut displaced_path = None;
    let mut moved = false;
    let result = (|| -> Result<()> {
        if let Some(baseline) = &baseline {
            let mut backup = Scratch::create(parent, "backup")?;
            backup.file.write_all(&baseline.bytes)?;
            preserve_permissions(&backup.file, &baseline.metadata)?;
            backup.file.sync_all()?;
            backup.keep = true;
            snapshot_path = Some(backup.path.clone());
            // The snapshot's directory entry must be durable BEFORE moving any
            // original inode, not just after publishing the replacement.
            directory
                .sync_all()
                .context("sync snapshot backup directory before displacement")?;
            hook(InstallStep::SnapshotSynced, Some(&backup.path))?;
        }
        let mut staged = Scratch::create(parent, "stage")?;
        staged.file.write_all(&bytes)?;
        if let Some(baseline) = &baseline {
            preserve_permissions(&staged.file, &baseline.metadata)?;
        }
        staged.file.sync_all()?;
        check_baseline(path, baseline.as_ref())?;
        if let Some(baseline) = &baseline {
            let mut displaced = Scratch::create(parent, "displaced")?;
            hook(InstallStep::BeforeDisplace, Some(&displaced.path))?;
            let placeholder = fs::symlink_metadata(&displaced.path)?;
            let owned = displaced.file.metadata()?;
            ensure!(
                placeholder.dev() == owned.dev()
                    && placeholder.ino() == owned.ino()
                    && placeholder.len() == 0,
                "reserved displacement placeholder changed; refusing to overwrite it"
            );
            // Disarm BEFORE rename. Drop must never unlink the external inode
            // that is about to replace our empty reservation, even on errors.
            displaced.keep = true;
            displaced_path = Some(displaced.path.clone());
            // This is not an atomic compare-and-swap: the catalog name is briefly
            // absent. Preserve the ACTUAL node moved, then publish with no clobber.
            fs::rename(path, &displaced.path)
                .context("retain current catalog in live inode backup")?;
            moved = true;
            directory
                .sync_all()
                .context("sync displaced catalog directory")?;
            hook(InstallStep::DisplacedSynced, Some(&displaced.path))?;
            let actual = read_catalog(&displaced.path)?
                .context("displaced catalog disappeared; recover manually")?;
            ensure!(
                same_displaced_metadata(&baseline.metadata, &actual.metadata)
                    && baseline.bytes == actual.bytes,
                "catalog changed between final check and displacement; refusing to publish"
            );
        }
        hook(InstallStep::BeforePublish, displaced_path.as_deref())?;
        fs::hard_link(&staged.path, path)
            .context("publish catalog without clobbering an existing path")?;
        directory
            .sync_all()
            .context("sync published catalog directory")?;
        hook(InstallStep::PublishedSynced, displaced_path.as_deref())?;
        Ok(())
    })();
    if let Err(error) = result {
        let recovery = if moved {
            restore_displaced(
                path,
                displaced_path.as_deref().expect("moved path recorded"),
                &directory,
            )
        } else {
            "catalog was not displaced".into()
        };
        let snapshot = snapshot_path
            .as_deref()
            .map_or_else(|| "none".into(), |p| p.display().to_string());
        let displaced = displaced_path
            .as_deref()
            .map_or_else(|| "none".into(), |p| p.display().to_string());
        return Err(error).with_context(|| format!("install failed; snapshot backup: {snapshot}; live inode backup/reservation: {displaced}; {recovery}"));
    }
    Ok(Install::Written(snapshot_path.zip(displaced_path).map(
        |(snapshot, displaced)| Backups {
            snapshot,
            displaced,
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn entry() -> String {
        stanza(
            Path::new("/opt/Numen App/numen"),
            Path::new("/tmp/private/events.sock"),
        )
        .unwrap()
    }

    #[test]
    fn argv_roundtrips_quoting_without_shell_or_key() {
        let binary = Path::new("/opt/space \"quote\"/new\nline\\numen");
        let socket = Path::new("/tmp/a b/\"events\".sock");
        let text = stanza(binary, socket).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        let action = &value["action"][0];
        assert_eq!(
            action["command"].as_array().unwrap(),
            &vec![
                toml::Value::String(binary.to_str().unwrap().into()),
                "--socket".into(),
                socket.to_str().unwrap().into(),
                "menu".into(),
            ]
        );
        assert!(action.get("key").is_none());
        assert_eq!(action["kind"].as_str(), Some("summon"));
    }

    #[test]
    fn preserves_bytes_comments_permissions_and_backup_then_noops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        let original = b"# hand written\n[[action]]\nid = 'other' # keep this\ncommand = ['hello']";
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let Install::Written(Some(backup)) = install(&path, &entry()).unwrap() else {
            panic!("expected backup")
        };
        assert_eq!(fs::read(&backup.snapshot).unwrap(), original);
        assert_eq!(fs::read(&backup.displaced).unwrap(), original);
        assert_eq!(
            fs::metadata(&backup.snapshot).unwrap().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(&backup.displaced).unwrap().mode() & 0o777,
            0o640
        );
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(original));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert!(matches!(
            install(&path, &entry()).unwrap(),
            Install::Unchanged
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn rejects_conflicts_duplicates_malformed_and_wrong_action_type() {
        for original in [
            "[[action]]\nid='numen'\ncommand=['wrong']".into(),
            format!("{}\n{}", entry(), entry()),
            "broken = [".into(),
            "action = 'wrong'".into(),
            "action = [1]".into(),
        ] {
            assert!(append(original.as_bytes(), &entry()).is_err());
        }
        assert!(append(&[0xff], &entry()).is_err());
    }

    #[test]
    fn refuses_symlink_special_oversize_and_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"# untouched").unwrap();
        let path = dir.path().join("actions.toml");
        symlink(&target, &path).unwrap();
        assert!(install(&path, &entry()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"# untouched");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(install(&path, &entry()).is_err());
        fs::remove_dir(&path).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(install(&path, &entry()).is_err());
        fs::remove_file(&path).unwrap();
        File::create(&path)
            .unwrap()
            .set_len(MAX_CATALOG + 1)
            .unwrap();
        assert!(install(&path, &entry()).is_err());
        assert!(install(&dir.path().join("missing/actions.toml"), &entry()).is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn new_file_is_private_and_changed_baseline_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        assert!(matches!(
            install(&path, &entry()).unwrap(),
            Install::Written(None)
        ));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let baseline = read_catalog(&path).unwrap().unwrap();
        fs::write(&path, b"# concurrent edit").unwrap();
        assert!(check_baseline(&path, Some(&baseline)).is_err());
        assert!(check_baseline(&path, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"# concurrent edit");
    }

    #[test]
    fn preserves_atomic_edit_between_final_check_and_displacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        fs::write(&path, b"# baseline\n").unwrap();
        let mut snapshot = None;
        let mut displaced = None;
        let error = install_with_hook(&path, &entry(), |step, saved| {
            if step == InstallStep::SnapshotSynced {
                snapshot = saved.map(Path::to_owned);
            }
            if step == InstallStep::BeforeDisplace {
                displaced = saved.map(Path::to_owned);
                let incoming = dir.path().join("external-new-version");
                fs::write(&incoming, b"# external atomic edit\n")?;
                fs::rename(incoming, &path)?;
            }
            Ok(())
        })
        .err()
        .expect("must detect the displaced version changed");
        let snapshot = snapshot.unwrap();
        let displaced = displaced.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"# external atomic edit\n");
        assert_eq!(fs::read(&displaced).unwrap(), b"# external atomic edit\n");
        assert_eq!(fs::read(&snapshot).unwrap(), b"# baseline\n");
        let message = format!("{error:#}");
        assert!(message.contains("between final check and displacement"));
        assert!(message.contains(snapshot.to_str().unwrap()));
        assert!(message.contains(displaced.to_str().unwrap()));
        assert!(message.contains("restored displaced catalog"));
    }

    #[test]
    fn external_creation_after_displacement_is_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        fs::write(&path, b"# original\n").unwrap();
        let original = fs::metadata(&path).unwrap();
        let mut displaced = None;
        let error = install_with_hook(&path, &entry(), |step, saved| {
            if step == InstallStep::DisplacedSynced {
                assert!(!path.exists());
                displaced = saved.map(Path::to_owned);
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?
                    .write_all(b"# external creation\n")?;
            }
            Ok(())
        })
        .err()
        .expect("publication must fail without clobbering");
        let displaced = displaced.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"# external creation\n");
        assert_eq!(fs::read(&displaced).unwrap(), b"# original\n");
        assert_eq!(fs::metadata(&displaced).unwrap().ino(), original.ino());
        assert!(format!("{error:#}").contains("already exists and was left untouched"));
    }

    #[test]
    fn late_editor_writes_remain_reachable_in_live_inode_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        fs::write(&path, b"# original\n").unwrap();
        let mut editor = OpenOptions::new().write(true).open(&path).unwrap();
        let Install::Written(Some(backups)) = install(&path, &entry()).unwrap() else {
            panic!("expected both backups");
        };
        let installed = fs::read(&path).unwrap();
        let late = b"# editor saved through the old descriptor\n";
        editor.write_all(late).unwrap();
        editor.set_len(late.len() as u64).unwrap();
        editor.sync_all().unwrap();
        assert_eq!(fs::read(&backups.displaced).unwrap(), late);
        assert_eq!(fs::read(&backups.snapshot).unwrap(), b"# original\n");
        assert_eq!(fs::read(&path).unwrap(), installed);
        assert_eq!(
            editor.metadata().unwrap().ino(),
            fs::metadata(&backups.displaced).unwrap().ino()
        );
        assert_ne!(
            editor.metadata().unwrap().ino(),
            fs::metadata(&path).unwrap().ino()
        );
    }

    #[test]
    fn commit_hooks_follow_directory_sync_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        fs::write(&path, b"# original\n").unwrap();
        let original = fs::metadata(&path).unwrap();
        let mut steps = Vec::new();
        install_with_hook(&path, &entry(), |step, saved| {
            steps.push(step);
            match step {
                InstallStep::SnapshotSynced => {
                    assert_eq!(fs::read(saved.unwrap())?, b"# original\n");
                    assert_eq!(fs::metadata(&path)?.ino(), original.ino());
                }
                InstallStep::BeforeDisplace => {
                    assert_eq!(steps[0], InstallStep::SnapshotSynced);
                    assert_eq!(fs::metadata(&path)?.ino(), original.ino());
                }
                InstallStep::DisplacedSynced | InstallStep::BeforePublish => {
                    assert!(!path.exists());
                    assert_eq!(fs::metadata(saved.unwrap())?.ino(), original.ino());
                }
                InstallStep::PublishedSynced => assert!(path.is_file()),
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            steps,
            vec![
                InstallStep::SnapshotSynced,
                InstallStep::BeforeDisplace,
                InstallStep::DisplacedSynced,
                InstallStep::BeforePublish,
                InstallStep::PublishedSynced
            ]
        );
    }

    #[test]
    fn injected_failures_recover_without_deleting_displaced_data() {
        for failure in [
            InstallStep::SnapshotSynced,
            InstallStep::BeforeDisplace,
            InstallStep::DisplacedSynced,
            InstallStep::BeforePublish,
            InstallStep::PublishedSynced,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("actions.toml");
            fs::write(&path, b"# original\n").unwrap();
            let original = fs::metadata(&path).unwrap();
            let mut snapshot = None;
            let mut displaced = None;
            let error = install_with_hook(&path, &entry(), |step, saved| {
                if step == InstallStep::SnapshotSynced {
                    snapshot = saved.map(Path::to_owned);
                }
                if step == InstallStep::DisplacedSynced {
                    displaced = saved.map(Path::to_owned);
                }
                ensure!(step != failure, "injected failure at {step:?}");
                Ok(())
            })
            .err()
            .expect("failure must propagate");
            assert!(format!("{error:#}").contains("injected failure"));
            assert_eq!(fs::read(snapshot.unwrap()).unwrap(), b"# original\n");
            if failure != InstallStep::PublishedSynced {
                assert_eq!(fs::read(&path).unwrap(), b"# original\n");
                assert_eq!(fs::metadata(&path).unwrap().ino(), original.ino());
            } else {
                assert!(fs::read(&path).unwrap().ends_with(entry().as_bytes()));
            }
            if let Some(displaced) = displaced {
                assert_eq!(fs::read(&displaced).unwrap(), b"# original\n");
                assert_eq!(fs::metadata(&displaced).unwrap().ino(), original.ino());
            }
        }
    }

    #[test]
    fn unsafe_displaced_symlink_is_retained_without_following_or_restoring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        let target = dir.path().join("target");
        fs::write(&path, b"# original\n").unwrap();
        fs::write(&target, b"# never touch\n").unwrap();
        let mut displaced = None;
        let error = install_with_hook(&path, &entry(), |step, saved| {
            if step == InstallStep::BeforeDisplace {
                displaced = saved.map(Path::to_owned);
                fs::remove_file(&path)?;
                symlink(&target, &path)?;
            }
            Ok(())
        })
        .err()
        .expect("unsafe displaced node must stop installation");
        let displaced = displaced.unwrap();
        assert!(
            fs::symlink_metadata(&displaced)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&displaced).unwrap(), target);
        assert_eq!(fs::read(&target).unwrap(), b"# never touch\n");
        assert!(fs::symlink_metadata(&path).is_err());
        assert!(format!("{error:#}").contains("not followed, deleted, or restored"));
        assert!(format!("{error:#}").contains(displaced.to_str().unwrap()));
    }

    #[test]
    fn scratch_cleanup_does_not_unlink_a_replaced_path() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(dir.path(), "stage").unwrap();
        let path = scratch.path.clone();
        let external = dir.path().join("external");
        fs::write(&external, b"external data").unwrap();
        fs::rename(&external, &path).unwrap();
        drop(scratch);
        assert_eq!(fs::read(&path).unwrap(), b"external data");
    }

    #[test]
    fn new_catalog_publication_also_refuses_interleaved_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.toml");
        assert!(
            install_with_hook(&path, &entry(), |step, _| {
                if step == InstallStep::BeforePublish {
                    fs::write(&path, b"# external creation\n")?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"# external creation\n");
    }

    #[test]
    fn rejects_unresolved_catalog_paths_without_environment_changes() {
        assert!(resolve_catalog("$XDG_CONFIG_HOME/wisp/actions.toml".into()).is_err());
        assert!(resolve_catalog("~someone/actions.toml".into()).is_err());
        assert!(
            resolve_catalog("relative/actions.toml".into())
                .unwrap()
                .is_absolute()
        );
    }
}
