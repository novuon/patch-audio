//! Modules that need a key: which key unlocks which modules. This build
//! lists none (`modules.json` has no `licences`), so every module here is
//! unlocked.

use serde::{Deserialize, Serialize};

/// A key the catalogue asks for, and the modules it unlocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceDef {
    pub id: String,
    pub name: String,
    /// The modules it unlocks.
    pub modules: Vec<String>,
    /// Not available yet: `notify_url` says when it is.
    #[serde(default)]
    pub coming_soon: bool,
    #[serde(default)]
    pub notify_url: String,
}

impl LicenceDef {
    /// Where to read about it.
    pub fn offer_url(&self) -> &str {
        &self.notify_url
    }

    /// The welcome's step for a chosen module waiting for its key.
    pub fn welcome(&self) -> (String, String) {
        (format!("{}: needs a key", self.name), format!("{} starts once its key is here.", self.name))
    }

    /// Why the engine refuses one of its modules.
    pub fn refusal(&self) -> String {
        format!("{} needs a key", self.name)
    }

    /// `omarchy-patch modules add` for modules (`names`) waiting for this key.
    pub fn chosen(&self, names: &str) -> String {
        format!("{names} chosen: it starts once its {} key is here", self.name)
    }
}

/// The key a module needs, if any.
pub fn for_module<'a>(c: &'a crate::modules::Catalogue, module: &str) -> Option<&'a LicenceDef> {
    c.licences.iter().find(|l| l.modules.iter().any(|m| m == module))
}

/// Whether this machine may use `module`: every module the catalogue does
/// not tie to a key.
pub fn module_unlocked(module: &str) -> bool {
    for_module(crate::modules::builtin(), module).is_none()
}
