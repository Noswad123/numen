use std::{fs, io, path::Path};

pub const SOURCE_SCHEME: &str = "numen-inputs-v1-fnv1a128:";
pub const BUILD_SCHEME: &str = "numen-build-v1-fnv1a128:";
pub const ROOT_FILES: [&str; 4] = ["Cargo.toml", "Cargo.lock", "build.rs", "build_support.rs"];
pub const INPUT_DIRS: [&str; 2] = ["src", "assets"];

// FNV-1a-128 is a versioned diagnostic checksum, not a security boundary.
struct Fingerprint(u128);

impl Fingerprint {
    fn new(domain: &str) -> Self {
        let mut hash = Self(0x6c62272e07bb014262b821756295c58d);
        hash.field(domain.as_bytes());
        hash
    }

    fn field(&mut self, bytes: &[u8]) {
        for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            self.0 ^= u128::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000000001000000000000000000013b);
        }
    }

    fn finish(self, scheme: &str) -> String {
        format!("{scheme}{:032x}", self.0)
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn collect(root: &Path, relative: &str, files: &mut Vec<String>) -> io::Result<()> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!("symlink input: {relative}")));
    }
    if metadata.is_file() {
        files.push(relative.to_owned());
    } else if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| invalid("input names must be UTF-8"))?;
            collect(root, &format!("{relative}/{name}"), files)?;
        }
    } else {
        return Err(invalid(format!("non-regular input: {relative}")));
    }
    Ok(())
}

pub fn source_fingerprint(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    for name in ROOT_FILES {
        if !fs::symlink_metadata(root.join(name))?.file_type().is_file() {
            return Err(invalid(format!("expected regular file: {name}")));
        }
        files.push(name.to_owned());
    }
    for name in INPUT_DIRS {
        if !fs::symlink_metadata(root.join(name))?.file_type().is_dir() {
            return Err(invalid(format!("expected directory: {name}")));
        }
        collect(root, name, &mut files)?;
    }
    files.sort();
    let mut hash = Fingerprint::new(SOURCE_SCHEME);
    for name in files {
        hash.field(name.as_bytes());
        hash.field(&fs::read(root.join(name))?);
    }
    Ok(hash.finish(SOURCE_SCHEME))
}

// A match identifies recorded inputs, not identical executables: external Cargo
// config, SDKs, linkers, wrappers, and dependency contents are not fully captured.
pub fn build_fingerprint(
    source: &str,
    configuration: &[(String, String)],
    compiler_identity: &[u8],
) -> String {
    let mut configuration = configuration.to_vec();
    configuration.sort();
    let mut hash = Fingerprint::new(BUILD_SCHEME);
    hash.field(source.as_bytes());
    hash.field(&(configuration.len() as u64).to_le_bytes());
    for (name, value) in configuration {
        hash.field(name.as_bytes());
        hash.field(value.as_bytes());
    }
    hash.field(compiler_identity);
    hash.finish(BUILD_SCHEME)
}
