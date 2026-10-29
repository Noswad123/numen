#[path = "../src/build_info.rs"]
mod build_info;
#[path = "../build_support.rs"]
mod build_support;

use build_info::{BuildInfo, Comparison, compare, current};
use build_support::{build_fingerprint, source_fingerprint};
use std::{fs, path::Path, process::Command};

struct Root(tempfile::TempDir);

impl Root {
    fn new(reverse: bool) -> Self {
        let root = Self(tempfile::tempdir().unwrap());
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("assets")).unwrap();
        let mut files = vec![
            ("Cargo.toml", "manifest"),
            ("Cargo.lock", "lock"),
            ("build.rs", "builder"),
            ("build_support.rs", "support"),
            ("src/main.rs", "main"),
            ("src/other.rs", "other"),
            ("assets/icon", "icon"),
        ];
        if reverse {
            files.reverse();
        }
        for (name, contents) in files {
            root.write(name, contents);
        }
        root
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.path().join(name), contents).unwrap();
    }

    fn fingerprint(&self) -> String {
        source_fingerprint(self.path()).unwrap()
    }
}

#[test]
fn tree_copy_creation_order_and_exclusions_are_invariant() {
    let first = Root::new(false);
    let second = Root::new(true);
    let expected = first.fingerprint();
    assert_eq!(expected, second.fingerprint());
    for name in ["tests", "target", ".git", "img"] {
        fs::create_dir(first.path().join(name)).unwrap();
        first.write(&format!("{name}/ignored"), "not an input");
    }
    first.write("README.md", "documentation");
    first.write("djinn-artifacts.zip", "archive");
    assert_eq!(expected, first.fingerprint());
    assert!(expected.starts_with(build_support::SOURCE_SCHEME));
    assert_eq!(expected.len(), build_support::SOURCE_SCHEME.len() + 32);
}

#[test]
fn every_root_file_and_recursive_input_affects_source() {
    for name in build_support::ROOT_FILES
        .into_iter()
        .chain(["src/main.rs", "assets/icon"])
    {
        let root = Root::new(false);
        let before = root.fingerprint();
        root.write(name, "changed bytes");
        assert_ne!(before, root.fingerprint(), "{name}");
    }
    let root = Root::new(false);
    let before = root.fingerprint();
    fs::create_dir(root.path().join("src/nested")).unwrap();
    root.write("src/nested/new.rs", "new");
    assert_ne!(before, root.fingerprint());
    fs::remove_file(root.path().join("src/nested/new.rs")).unwrap();
    assert_eq!(before, root.fingerprint());
    fs::rename(
        root.path().join("assets/icon"),
        root.path().join("assets/renamed"),
    )
    .unwrap();
    assert_ne!(before, root.fingerprint());
    fs::rename(
        root.path().join("assets/renamed"),
        root.path().join("assets/icon"),
    )
    .unwrap();
    fs::remove_file(root.path().join("src/other.rs")).unwrap();
    assert_ne!(before, root.fingerprint());
}

#[test]
fn missing_required_inputs_fail() {
    for name in build_support::ROOT_FILES
        .into_iter()
        .chain(build_support::INPUT_DIRS)
    {
        let root = Root::new(false);
        let path = root.path().join(name);
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        assert!(source_fingerprint(root.path()).is_err(), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn symlinks_and_unreadable_inputs_fail() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for name in ["Cargo.toml", "src", "src/link", "assets/link"] {
        let root = Root::new(false);
        let path = root.path().join(name);
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        symlink(root.path().join("Cargo.lock"), path).unwrap();
        assert!(source_fingerprint(root.path()).is_err(), "{name}");
    }
    let root = Root::new(false);
    let path = root.path().join("src/main.rs");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    // Privileged test runners can still read mode-000 files.
    if fs::read(&path).is_err() {
        assert!(source_fingerprint(root.path()).is_err());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

// macOS filesystems reject invalid UTF-8 names at creation time.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_input_names_fail() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let root = Root::new(false);
    let name = OsString::from_vec(vec![0xff]);
    fs::write(root.path().join("assets").join(name), b"bytes").unwrap();
    assert!(source_fingerprint(root.path()).is_err());
}

#[test]
fn configuration_and_compiler_are_hashed_but_order_is_not() {
    let source = Root::new(false).fingerprint();
    let config: Vec<_> = [
        ("TARGET", "aarch64-apple-darwin"),
        ("PROFILE", "debug"),
        ("OPT_LEVEL", "0"),
        ("DEBUG", "true"),
        ("CARGO_FEATURE_ALPHA", "1"),
        ("CARGO_FEATURE_BETA", "1"),
        ("CARGO_ENCODED_RUSTFLAGS", "--cfg\x1ftest_flag"),
        ("CARGO_CFG_PANIC", "unwind"),
        ("CARGO_CFG_DEBUG_ASSERTIONS", ""),
        ("CARGO_PROFILE_RELEASE_PANIC", "unwind"),
        ("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let expected = build_fingerprint(&source, &config, b"rustc identity");
    let mut reversed = config.clone();
    reversed.reverse();
    assert_eq!(
        expected,
        build_fingerprint(&source, &reversed, b"rustc identity")
    );
    for index in 0..config.len() {
        let mut changed = config.clone();
        changed[index].1.push('x');
        assert_ne!(
            expected,
            build_fingerprint(&source, &changed, b"rustc identity")
        );
    }
    for index in 4..6 {
        let mut changed = config.clone();
        changed.remove(index);
        assert_ne!(
            expected,
            build_fingerprint(&source, &changed, b"rustc identity")
        );
    }
    let mut changed = config.clone();
    changed.push(("CARGO_FEATURE_NEW".into(), "1".into()));
    assert_ne!(
        expected,
        build_fingerprint(&source, &changed, b"rustc identity")
    );
    assert_ne!(
        expected,
        build_fingerprint(&source, &config, b"different compiler")
    );
    assert_ne!(
        expected,
        build_fingerprint("different source", &config, b"rustc identity")
    );
}

#[test]
fn real_cargo_profile_overrides_change_embedded_fingerprint() {
    let root = Root::new(false);
    root.write(
        "Cargo.toml",
        "[package]\nname = 'profile-fingerprint-fixture'\nversion = '0.1.0'\nedition = '2024'\n\n[workspace]\n",
    );
    root.write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = 'profile-fingerprint-fixture'\nversion = '0.1.0'\n",
    );
    root.write("build.rs", include_str!("../build.rs"));
    root.write("build_support.rs", include_str!("../build_support.rs"));
    root.write(
        "src/main.rs",
        r#"fn main() {
    println!("{}\n{}\n{}\n{}",
        env!("NUMEN_BUILD_FINGERPRINT"), env!("NUMEN_SOURCE_FINGERPRINT"),
        cfg!(panic = "abort"), cfg!(debug_assertions));
}
"#,
    );
    let target = current().target;
    let target_dir = root.path().join("private-target");
    let binary = target_dir.join(&target).join("release").join(format!(
        "profile-fingerprint-fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let build = |panic: Option<&str>, assertions: Option<&str>| {
        let mut command = Command::new(env!("CARGO"));
        command.current_dir(root.path()).args([
            "build",
            "--offline",
            "--locked",
            "--quiet",
            "--release",
            "--target",
            &target,
        ]);
        command.arg("--target-dir").arg(&target_dir);
        // Do not inherit the outer build's cfg/profile/flags into this tiny crate.
        // SDKROOT and the compiler/toolchain environment remain command-scoped.
        for (name, _) in std::env::vars_os() {
            let label = name.to_string_lossy();
            if ["CARGO_CFG_", "CARGO_FEATURE_", "CARGO_PROFILE_"]
                .iter()
                .any(|prefix| label.starts_with(prefix))
                || label == "RUSTFLAGS"
                || label == "CARGO_ENCODED_RUSTFLAGS"
            {
                command.env_remove(name);
            }
        }
        if let Some(panic) = panic {
            command.env("CARGO_PROFILE_RELEASE_PANIC", panic);
        }
        if let Some(assertions) = assertions {
            command.env("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS", assertions);
        }
        let output = command.output().expect("run fixture Cargo build");
        assert!(
            output.status.success(),
            "fixture Cargo build failed: {}",
            output.status
        );
        let output = Command::new(&binary).output().expect("run fixture binary");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let baseline = build(None, None);
    assert_eq!(&baseline[2..], ["false", "false"]);
    let panic = build(Some("abort"), None);
    assert_eq!(&panic[2..], ["true", "false"]);
    let assertions = build(None, Some("true"));
    assert_eq!(&assertions[2..], ["false", "true"]);
    for changed in [&panic, &assertions] {
        assert_eq!(baseline[1], changed[1], "source inputs must be unchanged");
        assert_ne!(
            baseline[0], changed[0],
            "embedded build identity must change"
        );
    }
    assert_eq!(
        baseline,
        build(None, None),
        "removing overrides must restore identity"
    );
}

#[test]
fn runtime_comparison_requires_known_valid_build_identity_not_version() {
    let info = current();
    assert_eq!(compare(None), Comparison::Unknown);
    assert_eq!(compare(Some(&info)), Comparison::Match);
    let mut service = info.clone();
    service.version = "different version".into();
    service.source_fingerprint = "not used for build comparison".into();
    assert_eq!(compare(Some(&service)), Comparison::Match);
    service = info.clone();
    let last = service.fingerprint.pop().unwrap();
    service
        .fingerprint
        .push(if last == '0' { '1' } else { '0' });
    assert_eq!(service.version, info.version);
    assert_eq!(compare(Some(&service)), Comparison::Different);
    for invalid in [
        "",
        "numen-build-v2-fnv1a128:00000000000000000000000000000000",
        "numen-build-v1-fnv1a128:ABCDEF00000000000000000000000000",
        "numen-build-v1-fnv1a128:0000000000000000000000000000000",
        "numen-build-v1-fnv1a128:000000000000000000000000000000000",
        "numen-build-v1-fnv1a128:gggggggggggggggggggggggggggggggg",
    ] {
        service.fingerprint = invalid.into();
        assert_eq!(compare(Some(&service)), Comparison::Unknown, "{invalid}");
    }
    let encoded = serde_json::to_string(&info).unwrap();
    assert_eq!(serde_json::from_str::<BuildInfo>(&encoded).unwrap(), info);
    for (comparison, name) in [
        (Comparison::Match, "match"),
        (Comparison::Different, "different"),
        (Comparison::Unknown, "unknown"),
    ] {
        assert_eq!(serde_json::to_value(&comparison).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<Comparison>(serde_json::json!(name)).unwrap(),
            comparison
        );
    }
}
