use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub fingerprint: String,
    pub source_fingerprint: String,
    pub target: String,
    pub profile: String,
}

pub fn current() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        fingerprint: env!("NUMEN_BUILD_FINGERPRINT").to_owned(),
        source_fingerprint: env!("NUMEN_SOURCE_FINGERPRINT").to_owned(),
        target: env!("NUMEN_BUILD_TARGET").to_owned(),
        profile: env!("NUMEN_BUILD_PROFILE").to_owned(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Match,
    Different,
    Unknown,
}

pub fn compare(service: Option<&BuildInfo>) -> Comparison {
    fn known(value: &str) -> bool {
        value
            .strip_prefix("numen-build-v1-fnv1a128:")
            .is_some_and(|hex| {
                hex.len() == 32
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }
    let Some(service) = service else {
        return Comparison::Unknown;
    };
    let client = current();
    if !known(&client.fingerprint) || !known(&service.fingerprint) {
        Comparison::Unknown
    } else if client.fingerprint == service.fingerprint {
        Comparison::Match
    } else {
        Comparison::Different
    }
}
