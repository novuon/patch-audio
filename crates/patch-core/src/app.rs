use crate::{LayoutRecipe, SurfaceSpec};

/// Identity of the application. Patch is one application; its parts
/// (graph, mixer, meters, …) are surfaces and units inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppId {
    Patch,
}

impl AppId {
    pub const ALL: [AppId; 1] = [AppId::Patch];

    pub fn slug(self) -> &'static str {
        "patch"
    }

    pub fn name(self) -> &'static str {
        "Patch"
    }

    pub fn summary(self) -> &'static str {
        "audio routing, mixing, devices, meters"
    }

    /// Wayland app_id / Hyprland class shared by every Patch window.
    pub fn app_id(self) -> String {
        "omarchy.patch".into()
    }

    /// Shown next to the name in the controller.
    pub fn kind(self) -> &'static str {
        "audio"
    }

    pub fn from_slug(s: &str) -> Option<AppId> {
        (s == "patch").then_some(AppId::Patch)
    }
}

/// Static description of the application: its surfaces and layout recipes.
/// Plugins contribute additional surfaces and recipes.
#[derive(Debug, Clone)]
pub struct AppSpec {
    pub id: AppId,
    pub surfaces: Vec<SurfaceSpec>,
    pub layouts: Vec<LayoutRecipe>,
}

impl AppSpec {
    pub fn surface(&self, id: &str) -> Option<&SurfaceSpec> {
        self.surfaces.iter().find(|s| s.id == id)
    }

    /// The spec and instance number behind a window id (`meters#2`). Only
    /// `multi` surfaces have instances beyond the first.
    pub fn instance(&self, id: &str) -> Option<(&SurfaceSpec, u32)> {
        let n = crate::instance::number(id)?;
        let s = self.surface(crate::instance::base(id))?;
        (n == 1 || s.multi).then_some((s, n))
    }

    pub fn surface_for_key(&self, key: char) -> Option<&SurfaceSpec> {
        self.surfaces
            .iter()
            .find(|s| s.key == Some(key) && s.origin.is_implemented())
    }

    /// A layout by id, or by an older name (`all` is now `dev`, …).
    pub fn layout(&self, id: &str) -> Option<&LayoutRecipe> {
        let id = crate::layout::canonical(id);
        self.layouts.iter().find(|l| l.id == id)
    }

    /// Every Unit: those with surfaces (implemented if any surface is), plus
    /// the planned ones that have no surfaces yet. A tester kit (built with
    /// PATCH_KIT) leaves out the Club Booth, not even as planned.
    pub fn units(&self) -> Vec<crate::UnitSpec> {
        crate::UnitKind::ALL
            .iter()
            .filter(|k| option_env!("PATCH_KIT").is_none() || **k != crate::UnitKind::ClubBooth)
            .map(|k| {
                let surfaces: Vec<&'static str> = self
                    .surfaces
                    .iter()
                    .filter(|s| s.unit == *k)
                    .map(|s| s.id)
                    .collect();
                let implemented = self
                    .surfaces
                    .iter()
                    .any(|s| s.unit == *k && s.origin.is_implemented());
                crate::UnitSpec {
                    kind: *k,
                    surfaces,
                    implemented,
                }
            })
            .collect()
    }

    /// Surfaces that can actually be opened (core, or implemented plugin).
    pub fn openable(&self) -> impl Iterator<Item = &SurfaceSpec> {
        self.surfaces.iter().filter(|s| s.origin.can_open())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_only_for_multi_surfaces() {
        let spec = AppSpec {
            id: AppId::Patch,
            surfaces: vec![
                SurfaceSpec::core(crate::UnitKind::Meter, "meters", "Meter").multi(),
                SurfaceSpec::core(crate::UnitKind::Mixer, "mixer", "Mixer"),
            ],
            layouts: vec![],
        };
        assert_eq!(
            spec.instance("meters").map(|(s, n)| (s.id, n)),
            Some(("meters", 1))
        );
        assert_eq!(
            spec.instance("meters#3").map(|(s, n)| (s.id, n)),
            Some(("meters", 3))
        );
        assert_eq!(
            spec.instance("mixer").map(|(s, n)| (s.id, n)),
            Some(("mixer", 1))
        );
        assert!(spec.instance("mixer#2").is_none());
        assert!(spec.instance("meters#1").is_none());
        assert!(spec.instance("nope").is_none());
        let (s, n) = spec.instance("meters#2").unwrap();
        assert_eq!(s.instance_window_title(AppId::Patch, n), "Patch · Meter 2");
        assert_eq!(
            s.instance_window_title(AppId::Patch, 1),
            s.window_title(AppId::Patch)
        );
    }
}
