use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_LINE: usize = 8 * 1024;
pub const MAX_SOURCES: usize = 16;
pub const MAX_ACTIVITIES: usize = 32;

#[derive(clap::ValueEnum, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum ActivityState {
    Idle,
    Working,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl ActivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 96,
        "identifier must contain 1..96 ASCII characters"
    );
    ensure!(
        id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        "identifier must contain only ASCII letters, digits, '.', '_', ':', '-'"
    );
    Ok(())
}

pub fn validate_summary(summary: &str) -> Result<()> {
    ensure!(
        !summary.trim().is_empty()
            && summary.len() <= 160
            && !summary.chars().any(|c| c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        "summary must contain nonempty trimmed content, be at most 160 bytes, and contain no control or bidi formatting characters"
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Activity {
    pub activity: String,
    pub sequence: u64,
    pub state: ActivityState,
    #[serde(default)]
    pub attention: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Activity {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.activity)?;
        ensure!(self.sequence > 0, "sequence must be positive");
        ensure!(
            !self.attention || self.state == ActivityState::Waiting,
            "attention is allowed only for waiting activities"
        );
        if let Some(reason) = &self.reason {
            ensure!(
                reason.len() <= 128
                    && !reason.chars().any(|c| c.is_control()
                        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "reason must be at most 128 bytes and contain no control characters"
            );
        }
        if let Some(summary) = &self.summary {
            validate_summary(summary)?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "source.register")]
    Register {
        source: String,
        instance: String,
        activities: Vec<Activity>,
    },
    #[serde(rename = "activity.state")]
    State {
        #[serde(flatten)]
        activity: Activity,
    },
    #[serde(rename = "source.heartbeat")]
    Heartbeat,
    #[serde(rename = "source.snapshot")]
    Snapshot { activities: Vec<Activity> },
    #[serde(rename = "activity.remove")]
    Remove { activity: String, sequence: u64 },
    #[serde(rename = "companion.status")]
    Status {
        #[serde(default)]
        offset: usize,
    },
    #[serde(rename = "companion.focus")]
    Focus {
        source: String,
        #[serde(default)]
        instance: Option<String>,
        activity: String,
    },
    #[serde(rename = "companion.next")]
    Next,
}

impl Request {
    pub fn parse(line: &[u8]) -> Result<Self> {
        ensure!(line.len() <= MAX_LINE, "message exceeds 8 KiB");
        let request: Self = serde_json::from_slice(line)?;
        ensure!(
            request.version == VERSION,
            "unsupported protocol version {}; expected 1",
            request.version
        );
        match &request.event {
            Event::Register {
                source,
                instance,
                activities,
            } => {
                validate_id(source)?;
                validate_id(instance)?;
                validate_snapshot(activities)?;
            }
            Event::Snapshot { activities } => validate_snapshot(activities)?,
            Event::State { activity } => activity.validate()?,
            Event::Remove { activity, sequence } => {
                validate_id(activity)?;
                ensure!(*sequence > 0, "sequence must be positive");
            }
            Event::Heartbeat => {}
            Event::Status { .. } | Event::Next => {}
            Event::Focus {
                source,
                instance,
                activity,
            } => {
                validate_id(source)?;
                if let Some(instance) = instance {
                    validate_id(instance)?;
                }
                validate_id(activity)?;
            }
        }
        Ok(request)
    }
}

fn validate_snapshot(activities: &[Activity]) -> Result<()> {
    ensure!(
        activities.len() <= MAX_ACTIVITIES,
        "snapshot exceeds 32 activities"
    );
    let mut ids = std::collections::BTreeSet::new();
    for activity in activities {
        activity.validate()?;
        if !ids.insert(&activity.activity) {
            bail!("duplicate activity in snapshot");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_optional_bounded_utf8_and_safe() {
        for summary in ["", "   ", "\n", "bad\ttext", "\u{7f}", "\u{85}"] {
            assert!(validate_summary(summary).is_err(), "{summary:?}");
        }
        for c in ('\u{202a}'..='\u{202e}').chain('\u{2066}'..='\u{2069}') {
            assert!(validate_summary(&format!("a{c}b")).is_err());
        }
        assert!(validate_summary(&"\u{e9}".repeat(80)).is_ok());
        assert!(validate_summary(&"\u{e9}".repeat(81)).is_err());
        assert!(validate_summary(&"x".repeat(160)).is_ok());
        assert!(validate_summary(&"x".repeat(161)).is_err());
        assert!(validate_summary(" a summary ").is_ok());
        let packet = serde_json::json!({"version":1,"type":"activity.state",
            "activity":"task","sequence":1,"state":"working"});
        for summary in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("A summary")),
        ] {
            let mut packet = packet.clone();
            if let Some(value) = &summary {
                packet["summary"] = value.clone();
            }
            let Event::State { activity } = Request::parse(&serde_json::to_vec(&packet).unwrap())
                .unwrap()
                .event
            else {
                panic!("expected state");
            };
            assert_eq!(
                activity.summary.as_deref(),
                summary.as_ref().and_then(|s| s.as_str())
            );
            assert_eq!(
                serde_json::to_value(&activity)
                    .unwrap()
                    .get("summary")
                    .is_some(),
                activity.summary.is_some()
            );
        }
        let mut invalid = packet;
        invalid["summary"] = serde_json::json!(" ");
        assert!(Request::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }

    #[test]
    fn validates_wire_and_optional_fields() {
        assert!(
            Request::parse(br#"{"type":"source.heartbeat","version":1,"future":true}"#).is_ok()
        );
        for line in [
            r#"{"type":"companion.status","version":1,"offset":-1}"#,
            r#"{"type":"companion.status","version":2}"#,
            r#"{"type":"companion.next"}"#,
            r#"{"type":"companion.focus","version":1,"source":"a","instance":"bad/id","activity":"a"}"#,
            r#"{"type":"companion.focus","version":1,"source":"a","activity":""}"#,
            r#"{"type":"source.heartbeat","version":2}"#,
            r#"{"type":"future","version":1}"#,
            r#"{"type":"activity.state","version":1,"activity":"a","sequence":0,"state":"idle"}"#,
            r#"{"type":"activity.state","version":1,"activity":"a","sequence":1,"state":"working","attention":true}"#,
            r#"{"type":"activity.remove","version":1,"activity":"../a","sequence":1}"#,
        ] {
            assert!(Request::parse(line.as_bytes()).is_err(), "{line}");
        }
        let request = Request::parse(br#"{"type":"activity.state","version":1,"activity":"a","sequence":1,"state":"waiting","attention":true}"#).unwrap();
        assert!(matches!(request.event, Event::State { .. }));
        assert!(matches!(
            Request::parse(br#"{"type":"companion.status","version":1}"#)
                .unwrap()
                .event,
            Event::Status { offset: 0 }
        ));
        assert!(matches!(
            Request::parse(
                br#"{"type":"companion.focus","version":1,"source":"a","activity":"b"}"#
            )
            .unwrap()
            .event,
            Event::Focus { instance: None, .. }
        ));
        assert!(validate_id(&"a".repeat(97)).is_err());
        assert!(Request::parse(&vec![b' '; MAX_LINE + 1]).is_err());
        let mut a = Activity {
            activity: "a".into(),
            sequence: 1,
            state: ActivityState::Idle,
            attention: false,
            reason: Some("bad\nreason".into()),
            summary: None,
        };
        assert!(a.validate().is_err());
        a.reason = Some("x".repeat(129));
        assert!(a.validate().is_err());
    }
}
