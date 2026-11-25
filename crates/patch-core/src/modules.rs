//! The module catalogue: what can be installed besides Patch core, what each
//! module needs, which modules each install profile ticks, and (in a
//! release's manifest) what each module downloads.
//!
//! One file, `modules.json`, is the source for the installer, Settings and
//! `omarchy-patch modules`. A release publishes the same schema with the
//! payloads filled in (`manifest.json`, signed). Moving a module into a
//! profile's defaults is a change to that file only.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The catalogue this build carries (no payloads: those come from the
/// release's manifest).
pub const BUILTIN_JSON: &str = include_str!("../modules.json");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalogue {
    pub schema: u32,
    /// The release this manifest describes ("" for the built-in catalogue).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub core: Core,
    pub modules: Vec<Module>,
    /// The modules that need a key, and the keys this release accepts.
    #[serde(default)]
    pub licences: Vec<crate::licence::LicenceDef>,
    pub profiles: Vec<ProfileDef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Core {
    pub name: String,
    pub about: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
    pub name: String,
    pub about: String,
    /// Where menus list it: audio, dj, studio, look.
    pub group: String,
    /// Backed by a switch in `setup.toml` of the same id (shows its Units).
    #[serde(default)]
    pub switch: bool,
    /// Unfinished: never ticked by default outside the profiles that name it.
    #[serde(default)]
    pub preview: bool,
    /// Modules it cannot work without (added with it).
    #[serde(default)]
    pub needs: Vec<String>,
    /// Modules it installs along with it once it may run (Auto Mix brings
    /// its stem separator); removed with it.
    #[serde(default)]
    pub brings: Vec<String>,
    /// Not installed by default and not listed (installer, Settings,
    /// `modules`): it comes with the module that brings it, or by name
    /// (`omarchy-patch modules add stems`).
    #[serde(default)]
    pub hidden: bool,
    /// Its files are downloaded (the release's manifest lists them).
    #[serde(default)]
    pub download: bool,
    /// An install step that is not a file download (Stems' runtime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// What it downloads (filled in a release's manifest).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payloads: Vec<Payload>,
    /// Estimated download for a `step` (bytes), shown before it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    /// The archive's file name (also its cache name).
    pub file: String,
    /// Only for this architecture (`x86_64`, `aarch64`); none: every one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Where it unpacks: `themes` (Patch's shared themes folder) or `data`
    /// (the module's own folder).
    pub dest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDef {
    pub id: String,
    pub name: String,
    pub about: String,
    /// The installer's preselected profile.
    #[serde(default)]
    pub default: bool,
    /// Kept for `--profile`, not offered on the install screen.
    #[serde(default)]
    pub hidden: bool,
    /// Its default layout (a `DefaultLayout` id).
    pub layout: String,
    /// Ticked when the profile is chosen.
    pub modules: Vec<String>,
    /// Offered, unticked, inside the profile.
    pub offer: Vec<String>,
}

/// The catalogue built into this binary.
pub fn builtin() -> &'static Catalogue {
    static C: OnceLock<Catalogue> = OnceLock::new();
    C.get_or_init(|| {
        // Development builds only (built with PATCH_DEV_OVERRIDES set): a
        // test catalogue, e.g. with a sandbox licence service. A release
        // never reads it, so it cannot unlock anything.
        if dev_overrides()
            && let Some(c) = std::env::var("PATCH_CATALOGUE").ok().and_then(|p| std::fs::read_to_string(p).ok())
        {
            return parse(&c).expect("PATCH_CATALOGUE is a valid catalogue");
        }
        parse(BUILTIN_JSON).expect("modules.json is valid")
    })
}

/// Whether this build honours development overrides (test catalogue,
/// sandbox licence service, another manifest key): only when built with
/// `PATCH_DEV_OVERRIDES` set.
pub fn dev_overrides() -> bool {
    option_env!("PATCH_DEV_OVERRIDES").is_some()
}

pub fn parse(text: &str) -> Result<Catalogue, String> {
    let c: Catalogue = serde_json::from_str(text).map_err(|e| format!("module catalogue: {e}"))?;
    c.check()?;
    Ok(c)
}

impl Catalogue {
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id == id)
    }

    pub fn profile(&self, id: &str) -> Option<&ProfileDef> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// The installer's preselected profile.
    pub fn default_profile(&self) -> &ProfileDef {
        self.profiles.iter().find(|p| p.default).unwrap_or(&self.profiles[0])
    }

    /// `ids` with everything they need, in catalogue order. Unknown ids are
    /// left out.
    pub fn with_needs<S: AsRef<str>>(&self, ids: &[S]) -> Vec<String> {
        let mut on: Vec<&str> = Vec::new();
        let mut todo: Vec<&str> = ids.iter().map(|s| s.as_ref()).collect();
        while let Some(id) = todo.pop() {
            let Some(m) = self.module(id) else { continue };
            if on.contains(&m.id.as_str()) {
                continue;
            }
            on.push(&m.id);
            todo.extend(m.needs.iter().map(String::as_str));
        }
        self.modules.iter().filter(|m| on.contains(&m.id.as_str())).map(|m| m.id.clone()).collect()
    }

    /// The modules that need `id` (directly or through another), which go
    /// when it goes.
    pub fn needed_by(&self, id: &str) -> Vec<String> {
        self.modules
            .iter()
            .filter(|m| m.id != id && self.with_needs(&[m.id.as_str()]).iter().any(|n| n == id))
            .map(|m| m.id.clone())
            .collect()
    }

    /// The payloads `id` downloads on this architecture.
    pub fn payloads_for<'a>(&'a self, id: &str, arch: &'a str) -> impl Iterator<Item = &'a Payload> + 'a {
        self.module(id)
            .into_iter()
            .flat_map(|m| m.payloads.iter())
            .filter(move |p| p.arch.as_deref().is_none_or(|a| a == arch))
    }

    /// What `id` downloads here (bytes): its payloads, or its step's estimate.
    pub fn download_size(&self, id: &str, arch: &str) -> u64 {
        let files: u64 = self.payloads_for(id, arch).map(|p| p.size).sum();
        files + self.module(id).and_then(|m| m.step_size).unwrap_or(0)
    }

    /// What can't be true of a catalogue: unknown ids, cycles, a payload
    /// without a hash.
    pub fn check(&self) -> Result<(), String> {
        let known = |id: &str| self.module(id).is_some();
        for (i, m) in self.modules.iter().enumerate() {
            if self.modules[..i].iter().any(|x| x.id == m.id) {
                return Err(format!("module {} twice", m.id));
            }
            for n in m.needs.iter().chain(&m.brings) {
                if !known(n) || n == &m.id {
                    return Err(format!("{} needs or brings unknown {n}", m.id));
                }
            }
            if !m.payloads.is_empty() && !m.download {
                return Err(format!("{}: payloads but no download", m.id));
            }
            for p in &m.payloads {
                let hex = p.sha256.len() == 64 && p.sha256.bytes().all(|b| b.is_ascii_hexdigit());
                if !hex || p.url.is_empty() || p.file.contains('/') || !matches!(p.dest.as_str(), "themes" | "data") {
                    return Err(format!("{}: payload {} is incomplete", m.id, p.file));
                }
            }
        }
        for m in &self.modules {
            if self.with_needs(&[m.id.as_str()]).len() > self.modules.len() {
                return Err(format!("{}: needs loop", m.id));
            }
        }
        for l in &self.licences {
            if let Some(m) = l.modules.iter().find(|m| !known(m)) {
                return Err(format!("licence {} names unknown {m}", l.id));
            }
        }
        if !self.profiles.iter().any(|p| p.default) {
            return Err("no default profile".into());
        }
        for p in &self.profiles {
            for id in p.modules.iter().chain(&p.offer) {
                if !known(id) {
                    return Err(format!("profile {} names unknown {id}", p.id));
                }
            }
            if crate::setup::DefaultLayout::from_id(&p.layout).is_none() {
                return Err(format!("profile {}: unknown layout {}", p.id, p.layout));
            }
        }
        Ok(())
    }
}

/// This machine's architecture as manifests name it.
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// "76 MB", "1.4 GB" for menus.
pub fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1e6;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else if bytes > 0 {
        format!("{mb:.1} MB")
    } else {
        "0 MB".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{self, Setup};

    #[test]
    fn the_builtin_catalogue_is_whole() {
        let c = builtin();
        c.check().unwrap();
        // Every switch-backed module is a setup switch, and every module
        // switch in the setup is in the catalogue.
        for m in c.modules.iter().filter(|m| m.switch) {
            assert!(Setup::default().get(&m.id).is_some(), "{} has no switch", m.id);
        }
        for (id, _, _) in setup::MODULES {
            assert!(c.module(id).is_some_and(|m| m.switch), "{id} not in modules.json");
        }
        assert_eq!(c.default_profile().id, "standard");
    }

    #[test]
    fn needs_come_along_and_go_together() {
        let c = builtin();
        assert_eq!(c.with_needs(&["stems"]), ["dj", "stems"], "Stems are free: no Auto Mix needed");
        assert_eq!(c.module("ai_dj").unwrap().brings, ["stems"], "Auto Mix brings its stems");
        assert_eq!(c.with_needs(&["wallpapers"]), ["themes", "wallpapers"]);
        let mut by = c.needed_by("dj");
        by.sort();
        assert_eq!(by, ["ai_dj", "stems"]);
        assert!(c.needed_by("ai_dj").is_empty(), "removing Auto Mix leaves the stems");
        assert!(c.with_needs(&["nonsense"]).is_empty());
    }

    #[test]
    fn previews_are_never_a_default_outside_their_own_profile() {
        let c = builtin();
        for p in &c.profiles {
            for id in &p.modules {
                let m = c.module(id).unwrap();
                if m.preview {
                    assert!(matches!(p.id.as_str(), "podcast" | "full"), "{} ticks preview {id}", p.id);
                }
            }
        }
    }

    /// The user's choices (2026-11-19): Standard is the core and the EQ (and,
    /// 2026-11-25, every profile brings the Patch themes); the
    /// DJ profile brings decks, crossfader, Mini Booth (the `dj` module) and
    /// Auto Mix (the AI DJ), and offers the Visualiser unticked; the themes
    /// and the wallpapers are two modules. Only Auto Mix is sold. Stems are
    /// free, not installed by default and not listed; Auto Mix brings them.
    #[test]
    fn the_profiles_the_user_chose() {
        let c = builtin();
        assert_eq!(c.profile("standard").unwrap().modules, ["eq", "themes"]);
        for p in ["dj", "podcast", "live_coding", "full"] {
            assert!(c.profile(p).unwrap().modules.iter().any(|m| m == "themes"), "{p} brings the themes");
        }
        let dj = c.profile("dj").unwrap();
        assert!(["eq", "dj", "ai_dj"].iter().all(|m| dj.modules.iter().any(|x| x == m)));
        assert!(dj.offer.iter().any(|x| x == "visualiser"));
        let stems = c.module("stems").unwrap();
        assert!(stems.hidden, "Stems are not offered on the screens");
        assert!(!c.licences.iter().flat_map(|l| &l.modules).any(|m| m == "stems"), "Stems are not part of Auto Mix");
        for p in &c.profiles {
            assert!(!p.offer.iter().chain(&p.modules).any(|m| m == "stems"), "{} lists Stems", p.id);
        }
        if let Some(l) = c.licences.first() {
            assert_eq!(l.name, "Auto Mix");
        }
        assert!(c.module("themes").is_some() && c.module("wallpapers").is_some());
        for s in ["deck-a", "dj-mixer", "dj-mini"] {
            assert_eq!(setup::surface_module(s), Some("dj"), "{s} comes with DJ");
        }
    }

    /// The module is shown as Auto Mix; its id stays `ai_dj`.
    #[test]
    fn auto_mix_is_the_name() {
        let c = builtin();
        let module = c.module("ai_dj").unwrap();
        assert_eq!(module.id, "ai_dj");
        assert_eq!(module.name, "Auto Mix");
        assert!(!module.name.contains("AI") && !module.about.contains("AI DJ"), "{}", module.about);
        assert!(module.about.contains("orders and crossfades"));
        for module in &c.modules {
            assert!(!module.name.contains("AI DJ") && !module.about.contains("AI DJ"), "{}", module.id);
        }
        for profile in &c.profiles {
            assert!(!profile.about.contains("AI DJ"), "{}", profile.id);
        }
        let shown = setup::MODULES.iter().find(|m| m.0 == "ai_dj").unwrap();
        assert_eq!(shown.1, "Auto Mix");
        assert!(!shown.2.contains("AI DJ"));
    }

    #[test]
    fn a_manifest_carries_payloads() {
        let mut text: serde_json::Value = serde_json::from_str(BUILTIN_JSON).unwrap();
        let wall = text["modules"].as_array_mut().unwrap().iter_mut().find(|m| m["id"] == "wallpapers").unwrap();
        wall["payloads"] = serde_json::json!([{
            "file": "patch-wallpapers.tar.zst", "url": "https://example.invalid/w.tar.zst",
            "size": 76_000_000u64, "sha256": "ab".repeat(32), "dest": "themes"
        }]);
        let c = parse(&text.to_string()).unwrap();
        assert_eq!(c.download_size("wallpapers", "x86_64"), 76_000_000);
        assert_eq!(human_size(c.download_size("wallpapers", "aarch64")), "76 MB");
        text["modules"][13]["payloads"][0]["sha256"] = "nope".into();
        assert!(parse(&text.to_string()).is_err(), "a payload needs its hash");
    }
}
