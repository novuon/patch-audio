//! Persisted preparation and explicit controls for the unified DJ workspace.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreparedSet {
    pub version: u32,
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub notes: String,
    pub collections: Vec<i64>,
    /// Stable library identities, with paths retained for missing-file recovery.
    pub tracks: Vec<PreparedTrack>,
    pub minutes: Option<f64>,
    pub arc: String,
    pub bpm: Option<(f64, f64)>,
    pub mode: crate::aivis::Mode,
    pub deleted: bool,
}
impl Default for PreparedSet {
    fn default() -> Self {
        Self {
            version: 1,
            id: String::new(),
            revision: 0,
            name: "Untitled set".into(),
            notes: String::new(),
            collections: vec![],
            tracks: vec![],
            minutes: None,
            arc: "journey".into(),
            bpm: None,
            mode: crate::aivis::Mode::Suggest,
            deleted: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreparedTrack {
    pub id: Option<i64>,
    #[serde(default)]
    pub track_uuid: Option<String>,
    pub path: String,
    #[serde(default)]
    pub pinned: bool,
}
impl PreparedSet {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported saved-set version".into());
        }
        if self.name.trim().is_empty() || self.name.len() > 512 {
            return Err("Give the set a name (up to 512 bytes)".into());
        }
        if self.collections.len() > 100 || self.collections.iter().any(|id| *id <= 0) {
            return Err("Invalid collection selection".into());
        }
        if self.tracks.len() > 2000
            || self
                .tracks
                .iter()
                .any(|t| t.path.is_empty() || t.path.contains("://"))
        {
            return Err("Sets require local tracks (up to 2000)".into());
        }
        if self
            .minutes
            .is_some_and(|v| !v.is_finite() || !(10.0..=600.0).contains(&v))
        {
            return Err("Set duration must be 10–600 minutes, or open-ended".into());
        }
        if !["journey", "warmup", "peak"].contains(&self.arc.as_str()) {
            return Err("Choose journey, warmup or peak".into());
        }
        if self.bpm.is_some_and(|(a, b)| {
            !a.is_finite() || !b.is_finite() || a < 40.0 || b > 300.0 || a > b
        }) {
            return Err("Tempo corridor must be ordered within 40–300 BPM".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SessionAction {
    Start { set: PreparedSet },
    /// Temporary accelerated sessions; saved set settings stay unchanged.
    StartTest { set: PreparedSet, bars: u32 },
    /// Change transition pacing for the current set without changing sources.
    FastMode { on: bool },
    Pause,
    Resume,
    End,
    StopNow,
    Sources { collections: Vec<i64> },
    QueueMove { from: usize, to: usize },
    QueueRemove { index: usize, expected: String },
    ChangeNext,
    Direction { direction: crate::intent::Direction },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fast_mode_toggle_is_a_human_session_action_without_start_settings() {
        for on in [true, false] {
            let request: SessionAction = serde_json::from_value(serde_json::json!({"action":"fast_mode", "on":on})).unwrap();
            assert_eq!(request, SessionAction::FastMode { on });
            let cmd = crate::command::Command::DjSession { request };
            assert!(crate::command::validate(cmd.clone(), crate::command::Origin::User, &Default::default()).is_ok());
            assert!(crate::command::validate(cmd, crate::command::Origin::Ai, &Default::default()).is_err());
        }
    }

    #[test]
    fn test_sessions_validate_the_minimum_and_remain_user_controlled() {
        for bars in [0, 8, 16, 32] {
            let cmd = crate::command::Command::DjSession {
                request: SessionAction::StartTest { set: PreparedSet::default(), bars },
            };
            assert_eq!(crate::command::validate(cmd.clone(), crate::command::Origin::User, &Default::default()).is_ok(), [8, 16].contains(&bars));
            assert!(crate::command::validate(cmd, crate::command::Origin::Ai, &Default::default()).is_err());
        }
        let old: PreparedSet = serde_json::from_str(r#"{"name":"Existing set"}"#).unwrap();
        assert!(old.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_settings_and_ai_session_control() {
        let mut draft = PreparedSet {
            minutes: Some(f64::NAN),
            ..Default::default()
        };
        assert!(draft.validate().is_err());
        draft.minutes = None;
        draft.bpm = Some((130.0, 120.0));
        assert!(draft.validate().is_err());
        draft.bpm = None;
        let cmd = crate::command::Command::DjSession {
            request: SessionAction::Start { set: draft },
        };
        assert!(
            crate::command::validate(cmd, crate::command::Origin::Ai, &Default::default()).is_err()
        );
    }
}
