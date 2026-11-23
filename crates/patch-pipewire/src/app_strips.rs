//! User preferences for application stream strips. Stream IDs distinguish
//! concurrent playback connections, including those sharing a client.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Preference {
    #[serde(default)]
    pub default_name: String,
    #[serde(default)]
    pub custom_name: String,
    #[serde(default)]
    pub keep_when_silent: bool,
}

impl Preference {
    pub fn display_name(&self) -> &str {
        if self.custom_name.is_empty() {
            &self.default_name
        } else {
            &self.custom_name
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Store {
    #[serde(default)]
    entries: BTreeMap<String, Preference>,
}

impl Store {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<&Preference> {
        self.entries.get(id)
    }

    pub fn kept(&self) -> impl Iterator<Item = (&str, &Preference)> {
        self.entries
            .iter()
            .filter(|(_, p)| p.keep_when_silent)
            .map(|(id, p)| (id.as_str(), p))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Hand one stream's preference to the stream that replaced it (a
    /// browser reopening its output). Never overwrites the new stream's own.
    pub fn reassign(&mut self, from: &str, to: &str) -> Result<bool, String> {
        if self.entries.contains_key(to) {
            return Ok(false);
        }
        let Some(preference) = self.entries.remove(from) else { return Ok(false) };
        self.entries.insert(to.to_string(), preference);
        save(self).map(|_| true)
    }

    /// Move a former application-wide preference to its first live client.
    /// It must never create a second idle copy or rename every browser window.
    pub fn migrate(&mut self, replacements: &[(String, String)]) -> Result<(), String> {
        if self.migrate_entries(replacements) { save(self) } else { Ok(()) }
    }

    fn migrate_entries(&mut self, replacements: &[(String, String)]) -> bool {
        let mut changed = false;
        for (old, new) in replacements {
            if let Some(preference) = self.entries.remove(old) {
                self.entries.entry(new.clone()).or_insert(preference);
                changed = true;
            }
        }
        changed
    }

    pub fn set(
        &mut self,
        id: &str,
        default_name: &str,
        custom_name: &str,
        keep: bool,
    ) -> Result<(), String> {
        let default_name = default_name.trim();
        let custom_name = custom_name.trim();
        if default_name.is_empty() && !self.entries.contains_key(id) {
            return Err("the application name is not available yet".into());
        }
        let old_default = self.entries.get(id).map(|p| p.default_name.clone());
        if custom_name.is_empty() && !keep {
            self.entries.remove(id);
            return save(self);
        }
        self.entries.insert(
            id.to_string(),
            Preference {
                default_name: if default_name.is_empty() {
                    old_default.unwrap_or_else(|| "Application".into())
                } else {
                    default_name.to_string()
                },
                custom_name: custom_name.to_string(),
                keep_when_silent: keep,
            },
        );
        save(self)
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("omarchy-patch/application-strips.json"))
}

fn save(store: &Store) -> Result<(), String> {
    let path = path().ok_or("no configuration directory")?;
    let parent = path.parent().ok_or("no configuration directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.new");
    std::fs::write(&temp, bytes).map_err(|e| format!("write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_application_preference_moves_to_only_the_first_client() {
        let old = "application:name:chromium:Chromium";
        let first = format!("{old}|client:83|pid:42");
        let second = format!("{old}|client:301|pid:42");
        let mut store = Store { entries: BTreeMap::from([(old.into(), Preference {
            default_name: "Chromium".into(), custom_name: "X".into(), keep_when_silent: true,
        })]) };
        let replacements = [(old.into(), first.clone()), (old.into(), second.clone())];
        assert!(store.migrate_entries(&replacements));
        assert!(store.get(old).is_none());
        assert_eq!(store.get(&first).unwrap().display_name(), "X");
        assert!(store.get(&second).is_none());
        assert_eq!(store.kept().count(), 1);
        assert!(!store.migrate_entries(&replacements));
    }

    #[test]
    fn legacy_client_preference_moves_to_only_one_stream() {
        let old = "application:name:chromium:Chromium|client:83|pid:42";
        let first = format!("{old}|stream:100");
        let second = format!("{old}|stream:101");
        let mut store = Store { entries: BTreeMap::from([(old.into(), Preference {
            default_name: "Chromium".into(), custom_name: "Music".into(), keep_when_silent: true,
        })]) };
        assert!(store.migrate_entries(&[(old.into(), first.clone()), (old.into(), second.clone())]));
        assert!(store.get(old).is_none());
        assert_eq!(store.get(&first).unwrap().display_name(), "Music");
        assert!(store.get(&second).is_none());
        assert_eq!(store.kept().count(), 1);
    }

    #[test]
    fn preference_uses_custom_name_and_defaults_to_the_application_name() {
        let mut p = Preference {
            default_name: "Chromium".into(),
            custom_name: String::new(),
            keep_when_silent: true,
        };
        assert_eq!(p.display_name(), "Chromium");
        p.custom_name = "Music browser".into();
        assert_eq!(p.display_name(), "Music browser");
    }

    #[test]
    fn only_kept_entries_are_returned_for_idle_slots() {
        let store = Store {
            entries: BTreeMap::from([
                (
                    "application:id:chromium".into(),
                    Preference {
                        default_name: "Chromium".into(),
                        custom_name: String::new(),
                        keep_when_silent: true,
                    },
                ),
                (
                    "application:id:firefox".into(),
                    Preference {
                        default_name: "Firefox".into(),
                        custom_name: String::new(),
                        keep_when_silent: false,
                    },
                ),
            ]),
        };
        assert_eq!(
            store.kept().map(|(id, _)| id).collect::<Vec<_>>(),
            ["application:id:chromium"]
        );
    }
}
