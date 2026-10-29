use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_LINE: usize = 8 * 1024;
pub const MAX_SOURCES: usize = 16;
pub const MAX_ACTIVITIES: usize = 32;
pub const MAX_FAMILIARS: usize = 16;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct FamiliarSettings {
    pub sprite_scale: f64,
    pub font_size: f64,
    pub bubble_duration: f64,
}

impl Default for FamiliarSettings {
    fn default() -> Self {
        Self {
            sprite_scale: 0.5,
            font_size: 12.0,
            bubble_duration: 5.0,
        }
    }
}

impl FamiliarSettings {
    pub fn validate(&self) -> Result<()> {
        for (name, value, min, max) in [
            ("sprite_scale", self.sprite_scale, 0.25, 2.0),
            ("font_size", self.font_size, 8.0, 32.0),
            ("bubble_duration", self.bubble_duration, 0.25, 30.0),
        ] {
            ensure!(
                value.is_finite() && (min..=max).contains(&value),
                "{name} must be finite and within {min}..={max}"
            );
        }
        Ok(())
    }
}

fn setting_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<f64>, D::Error> {
    f64::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct SettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprite_scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bubble_duration: Option<f64>,
}

impl<'de> Deserialize<'de> for SettingsPatch {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            #[serde(default, deserialize_with = "setting_value")]
            sprite_scale: Option<f64>,
            #[serde(default, deserialize_with = "setting_value")]
            font_size: Option<f64>,
            #[serde(default, deserialize_with = "setting_value")]
            bubble_duration: Option<f64>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let patch = Self {
            sprite_scale: fields.sprite_scale,
            font_size: fields.font_size,
            bubble_duration: fields.bubble_duration,
        };
        patch.validate().map_err(serde::de::Error::custom)?;
        Ok(patch)
    }
}

impl SettingsPatch {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.sprite_scale.is_some()
                || self.font_size.is_some()
                || self.bubble_duration.is_some(),
            "settings patch must not be empty"
        );
        self.apply(FamiliarSettings::default()).validate()
    }

    pub fn apply(&self, settings: FamiliarSettings) -> FamiliarSettings {
        FamiliarSettings {
            sprite_scale: self.sprite_scale.unwrap_or(settings.sprite_scale),
            font_size: self.font_size.unwrap_or(settings.font_size),
            bubble_duration: self.bubble_duration.unwrap_or(settings.bubble_duration),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TargetRequest {
    pub source: String,
    #[serde(default)]
    pub instance: Option<String>,
    pub activity: String,
}

impl TargetRequest {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.source)?;
        validate_id(&self.activity)?;
        if let Some(instance) = &self.instance {
            validate_id(instance)?;
        }
        Ok(())
    }
}

fn default_preset() -> String {
    "djinn".into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(flatten)]
    pub target: TargetRequest,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AttachRequest {
    pub id: String,
    #[serde(flatten)]
    pub target: TargetRequest,
}

pub fn validate_ids(ids: &[String]) -> Result<()> {
    ensure!(
        (1..=MAX_FAMILIARS).contains(&ids.len()),
        "expected 1..16 familiar IDs"
    );
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        validate_id(id)?;
        ensure!(seen.insert(id), "duplicate familiar ID");
    }
    Ok(())
}

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
    #[serde(default)]
    pub service_instance: Option<String>,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "companion.quit")]
    Quit,
    #[serde(rename = "companion.service")]
    Service,
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
        #[serde(default)]
        familiar: Option<String>,
    },
    #[serde(rename = "companion.focus")]
    Focus {
        source: String,
        #[serde(default)]
        instance: Option<String>,
        activity: String,
        #[serde(default)]
        familiar: Option<String>,
    },
    #[serde(rename = "companion.next")]
    Next {
        #[serde(default)]
        familiar: Option<String>,
    },
    #[serde(rename = "companion.reveal")]
    Reveal {
        #[serde(default)]
        familiars: Option<Vec<String>>,
    },
    #[serde(rename = "familiar.list")]
    FamiliarList {
        #[serde(default)]
        offset: usize,
    },
    #[serde(rename = "companion.targets")]
    Targets {
        #[serde(default)]
        offset: usize,
        #[serde(default)]
        familiar: Option<String>,
    },
    #[serde(rename = "familiar.open")]
    Open { items: Vec<OpenRequest> },
    #[serde(rename = "familiar.attach")]
    Attach { items: Vec<AttachRequest> },
    #[serde(rename = "familiar.close")]
    Close { ids: Vec<String> },
    #[serde(rename = "familiar.go-to-mouse")]
    GoToMouse { ids: Vec<String> },
    #[serde(rename = "familiar.settings")]
    Settings {
        ids: Vec<String>,
        patch: SettingsPatch,
    },
    #[serde(rename = "familiar.preferences.forget")]
    ForgetPreferences { ids: Vec<String> },
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
        if let Some(instance) = &request.service_instance {
            validate_id(instance)?;
        }
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
            Event::Heartbeat | Event::Service | Event::Quit => {}
            Event::Status { familiar, .. }
            | Event::Next { familiar }
            | Event::Targets { familiar, .. } => {
                if let Some(id) = familiar {
                    validate_id(id)?;
                }
            }
            Event::Reveal { familiars } => {
                if let Some(ids) = familiars {
                    validate_ids(ids)?;
                }
            }
            Event::FamiliarList { .. } => {}
            Event::Settings { ids, patch } => {
                validate_ids(ids)?;
                patch.validate()?;
            }
            Event::Close { ids } | Event::GoToMouse { ids } | Event::ForgetPreferences { ids } => {
                validate_ids(ids)?
            }
            Event::Open { items } => {
                ensure!(
                    (1..=MAX_FAMILIARS).contains(&items.len()),
                    "expected 1..16 items"
                );
                for item in items {
                    if let Some(id) = &item.id {
                        validate_id(id)?;
                    }
                    ensure!(item.preset == "djinn", "unknown preset");
                    item.target.validate()?;
                }
            }
            Event::Attach { items } => {
                validate_ids(&items.iter().map(|i| i.id.clone()).collect::<Vec<_>>())?;
                for item in items {
                    item.target.validate()?;
                }
            }
            Event::Focus {
                source,
                instance,
                activity,
                familiar,
            } => {
                if let Some(id) = familiar {
                    validate_id(id)?;
                }
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
    fn forget_preferences_validates_unique_bounded_ids() {
        for ids in [
            serde_json::json!(["main"]),
            serde_json::json!((0..16).map(|n| format!("f{n}")).collect::<Vec<_>>()),
        ] {
            let packet =
                serde_json::json!({"version":1,"type":"familiar.preferences.forget","ids":ids});
            assert!(matches!(
                Request::parse(&serde_json::to_vec(&packet).unwrap())
                    .unwrap()
                    .event,
                Event::ForgetPreferences { .. }
            ));
        }
        for ids in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!(["main", "main"]),
            serde_json::json!(["bad/id"]),
            serde_json::json!((0..17).map(|n| format!("f{n}")).collect::<Vec<_>>()),
        ] {
            let packet =
                serde_json::json!({"version":1,"type":"familiar.preferences.forget","ids":ids});
            assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_err());
        }
    }

    #[test]
    fn settings_patch_rejects_null_unknown_empty_and_invalid_bounds() {
        for patch in [
            serde_json::json!({}),
            serde_json::json!({"font_size":null}),
            serde_json::json!({"font_size":12,"sprite_scale":null}),
            serde_json::json!({"font_size":12,"unknown":1}),
            serde_json::json!({"font_size":7.99}),
            serde_json::json!({"sprite_scale":2.01}),
            serde_json::json!({"bubble_duration":30.01}),
            serde_json::json!({"bubble_duration":"5"}),
        ] {
            assert!(serde_json::from_value::<SettingsPatch>(patch).is_err());
        }
        for mask in 1..8 {
            let mut patch = serde_json::json!({});
            for (bit, key, value) in [
                (1, "sprite_scale", 0.25),
                (2, "font_size", 32.0),
                (4, "bubble_duration", 30.0),
            ] {
                if mask & bit != 0 {
                    patch[key] = value.into();
                }
            }
            let packet = serde_json::json!({"version":1,"type":"familiar.settings","ids":["main"],"patch":patch});
            assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_ok());
            for ids in [
                serde_json::json!([]),
                serde_json::json!(["main", "main"]),
                serde_json::json!(["bad/id"]),
                serde_json::json!(vec!["a"; 17]),
            ] {
                let mut invalid = packet.clone();
                invalid["ids"] = ids;
                assert!(Request::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
            }
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                SettingsPatch {
                    font_size: Some(value),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        FamiliarSettings::default().validate().unwrap();
        FamiliarSettings {
            sprite_scale: 2.0,
            font_size: 8.0,
            bubble_duration: 0.25,
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn service_precondition_is_optional_validated_and_distinct_from_producer_instance() {
        for instance in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("service-1")),
        ] {
            let mut packet = serde_json::json!({"version":1,"type":"source.register",
                "source":"test","instance":"producer-1","activities":[]});
            if let Some(value) = instance {
                packet["service_instance"] = value;
            }
            let request = Request::parse(&serde_json::to_vec(&packet).unwrap()).unwrap();
            assert_eq!(
                request.service_instance.as_deref(),
                packet["service_instance"].as_str()
            );
            let Event::Register { instance, .. } = request.event else {
                panic!("expected registration");
            };
            assert_eq!(instance, "producer-1");
        }
        for value in [
            serde_json::json!(""),
            serde_json::json!("bad/id"),
            serde_json::json!("\u{e9}"),
            serde_json::json!("x".repeat(97)),
            serde_json::json!(123),
        ] {
            let packet = serde_json::json!({"version":1,"type":"companion.service","service_instance":value});
            assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_err());
        }
        let packet = serde_json::json!({"version":1,"type":"companion.quit","service_instance":"x".repeat(96)});
        assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_ok());
    }

    #[test]
    fn quit_requires_supported_version_and_no_fields() {
        assert!(matches!(
            Request::parse(br#"{"version":1,"type":"companion.quit"}"#)
                .unwrap()
                .event,
            Event::Quit
        ));
        for packet in [
            br#"{"version":2,"type":"companion.quit"}"#.as_slice(),
            br#"{"type":"companion.quit"}"#.as_slice(),
            br#"{"version":1,"type":"companion.quit",}"#.as_slice(),
        ] {
            assert!(Request::parse(packet).is_err());
        }
    }

    #[test]
    fn service_requires_supported_version_and_no_fields() {
        assert!(matches!(
            Request::parse(br#"{"version":1,"type":"companion.service"}"#)
                .unwrap()
                .event,
            Event::Service
        ));
        for packet in [
            br#"{"version":2,"type":"companion.service"}"#.as_slice(),
            br#"{"type":"companion.service"}"#.as_slice(),
        ] {
            assert!(Request::parse(packet).is_err());
        }
    }

    #[test]
    fn go_to_mouse_validates_required_unique_bounded_ids() {
        for ids in [
            vec!["main".to_owned()],
            (0..16).map(|n| format!("f{n}")).collect(),
        ] {
            let packet = serde_json::json!({"version":1,"type":"familiar.go-to-mouse","ids":ids});
            let Event::GoToMouse { ids: parsed } =
                Request::parse(&serde_json::to_vec(&packet).unwrap())
                    .unwrap()
                    .event
            else {
                panic!("expected go-to-mouse");
            };
            assert_eq!(parsed, ids);
        }
        for ids in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!(["main", "main"]),
            serde_json::json!(["bad/id"]),
            serde_json::json!([""]),
            serde_json::json!(["x".repeat(97)]),
            serde_json::json!((0..17).map(|n| format!("f{n}")).collect::<Vec<_>>()),
        ] {
            let packet = serde_json::json!({"version":1,"type":"familiar.go-to-mouse","ids":ids});
            assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_err());
        }
        for packet in [
            serde_json::json!({"version":1,"type":"familiar.go-to-mouse"}),
            serde_json::json!({"version":2,"type":"familiar.go-to-mouse","ids":["main"]}),
        ] {
            assert!(Request::parse(&serde_json::to_vec(&packet).unwrap()).is_err());
        }
    }

    #[test]
    fn familiar_controls_validate_all_ids_and_batch_bounds() {
        for packet in [
            serde_json::json!({"type":"companion.status","familiar":"bad/id"}),
            serde_json::json!({"type":"companion.next","familiar":""}),
            serde_json::json!({"type":"companion.targets","familiar":"bad/id"}),
            serde_json::json!({"type":"companion.focus","familiar":"bad/id","source":"a","activity":"b"}),
            serde_json::json!({"type":"companion.reveal","familiars":[]}),
            serde_json::json!({"type":"companion.reveal","familiars":["main","main"]}),
            serde_json::json!({"type":"familiar.close","ids":["bad/id"]}),
            serde_json::json!({"type":"familiar.open","items":[]}),
            serde_json::json!({"type":"familiar.open","items":[{"source":"a","activity":"b","preset":"wisp"}]}),
            serde_json::json!({"type":"familiar.attach","items":[{"id":"main","source":"a","instance":"bad/id","activity":"b"}]}),
            serde_json::json!({"type":"familiar.list","offset":-1}),
            serde_json::json!({"type":"companion.targets","offset":-1}),
            serde_json::json!({"type":"familiar.open","items":vec![serde_json::json!({"source":"a","activity":"b"});17]}),
            serde_json::json!({"type":"familiar.attach","items":vec![serde_json::json!({"id":"main","source":"a","activity":"b"});17]}),
        ] {
            let mut packet = packet;
            packet["version"] = 1.into();
            assert!(
                Request::parse(&serde_json::to_vec(&packet).unwrap()).is_err(),
                "{packet}"
            );
        }
        let request = Request::parse(br#"{"version":1,"type":"familiar.open","items":[{"source":"a","activity":"b"}],"future":true}"#).unwrap();
        let Event::Open { items } = request.event else {
            panic!("expected open");
        };
        assert_eq!(items[0].preset, "djinn");
        assert!(items[0].id.is_none());
    }

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
        assert!(matches!(
            Request::parse(br#"{"type":"companion.reveal","version":1}"#)
                .unwrap()
                .event,
            Event::Reveal { familiars: None }
        ));
        assert!(
            Request::parse(br#"{"type":"source.heartbeat","version":1,"future":true}"#).is_ok()
        );
        for line in [
            r#"{"type":"companion.reveal","version":2}"#,
            r#"{"type":"companion.reveal"}"#,
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
            Event::Status {
                offset: 0,
                familiar: None
            }
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
