mod build_support;

use std::{collections::BTreeSet, env, error::Error, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    for path in build_support::ROOT_FILES
        .into_iter()
        .chain(build_support::INPUT_DIRS)
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let mut configuration = Vec::new();
    for name in [
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "CARGO_ENCODED_RUSTFLAGS",
    ] {
        let value = match env::var(name) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) if name == "CARGO_ENCODED_RUSTFLAGS" => String::new(),
            Err(_) => return Err(format!("missing or non-UTF-8 Cargo {name}").into()),
        };
        configuration.push((name.to_owned(), value));
    }
    let mut watches = BTreeSet::new();
    // Cargo tracks its own cfg/feature/output variables. Profile overrides are
    // parent environment inputs, including overrides first introduced next build.
    for (name, value) in env::vars_os() {
        let label = name.to_string_lossy();
        if ["CARGO_FEATURE_", "CARGO_CFG_", "CARGO_PROFILE_"]
            .iter()
            .any(|prefix| label.starts_with(prefix))
        {
            let name = name
                .into_string()
                .map_err(|_| "non-UTF-8 Cargo input name")?;
            let value = value
                .into_string()
                .map_err(|_| "non-UTF-8 Cargo input value")?;
            if name.starts_with("CARGO_PROFILE_") {
                watches.insert(name.clone());
            }
            configuration.push((name, value));
        }
    }
    for profile in ["DEV", "RELEASE", "TEST", "BENCH"] {
        for scope in ["", "BUILD_OVERRIDE_"] {
            for option in [
                "OPT_LEVEL",
                "DEBUG",
                "SPLIT_DEBUGINFO",
                "STRIP",
                "DEBUG_ASSERTIONS",
                "OVERFLOW_CHECKS",
                "LTO",
                "PANIC",
                "INCREMENTAL",
                "CODEGEN_UNITS",
                "RPATH",
            ] {
                watches.insert(format!("CARGO_PROFILE_{profile}_{scope}{option}"));
            }
        }
    }
    for name in ["RUSTC", "RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        watches.insert(name.to_owned());
    }
    for name in watches {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let compiler = Command::new(env::var_os("RUSTC").ok_or("Cargo did not set RUSTC")?)
        .arg("-vV")
        .output()?;
    if !compiler.status.success() || compiler.stdout.is_empty() {
        return Err("could not obtain compiler identity from RUSTC -vV".into());
    }
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let source = build_support::source_fingerprint(&root)?;
    let build = build_support::build_fingerprint(&source, &configuration, &compiler.stdout);
    println!("cargo:rustc-env=NUMEN_SOURCE_FINGERPRINT={source}");
    println!("cargo:rustc-env=NUMEN_BUILD_FINGERPRINT={build}");
    // Only these bounded public labels are exposed; flags and compiler output stay hashed.
    for (name, value) in &configuration {
        if name == "TARGET" || name == "PROFILE" {
            if value.is_empty()
                || value.len() > 256
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            {
                return Err(format!("invalid Cargo {name} label").into());
            }
            println!("cargo:rustc-env=NUMEN_BUILD_{name}={value}");
        }
    }
    Ok(())
}
