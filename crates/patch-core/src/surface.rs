/// A surface is one pane of a Unit that can live in its own Wayland window.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceSpec {
    /// The Unit this surface belongs to.
    pub unit: crate::UnitKind,
    /// Stable id, used in window titles, IPC and layout recipes.
    pub id: &'static str,
    pub title: &'static str,
    /// The letter that focuses or opens the surface: always
    /// [`crate::keys::letter`] of its id (the one table of letters).
    pub key: Option<char>,
    pub origin: Origin,
    /// Several windows of this surface may be open at once (instances
    /// `meters`, `meters#2`, …); see [`crate::instance`].
    pub multi: bool,
    /// The smallest window (logical px, content and frame) in which every
    /// part of the surface is shown: layouts give it at least this, and a
    /// smaller window offers to make room (docs/layouts.md).
    pub min_size: (u16, u16),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    /// Provided by the Unit itself.
    Core,
    /// Contributed by a plugin. `implemented == false` marks planned surfaces
    /// that only exist so the architecture and layouts can reference them.
    Plugin {
        plugin: &'static str,
        implemented: bool,
    },
    /// The window of a planned Unit: it holds the Unit's place in the
    /// workspace and says what it will do. The Unit itself is not built.
    Placeholder,
}

impl Origin {
    pub fn is_implemented(&self) -> bool {
        match self {
            Origin::Core => true,
            Origin::Plugin { implemented, .. } => *implemented,
            Origin::Placeholder => false,
        }
    }

    /// Has a window that can be opened (built, or a planned Unit's placeholder).
    pub fn can_open(&self) -> bool {
        self.is_implemented() || matches!(self, Origin::Placeholder)
    }
}

impl SurfaceSpec {
    pub const fn core(
        unit: crate::UnitKind,
        id: &'static str,
        title: &'static str,
    ) -> Self {
        SurfaceSpec {
            unit,
            id,
            title,
            key: crate::keys::letter(id),
            origin: Origin::Core,
            multi: false,
            min_size: (480, 320),
        }
    }

    /// A surface contributed by a unit (plugin) that is implemented.
    pub const fn unit(
        unit: crate::UnitKind,
        plugin: &'static str,
        id: &'static str,
        title: &'static str,
    ) -> Self {
        SurfaceSpec {
            unit,
            id,
            title,
            key: crate::keys::letter(id),
            origin: Origin::Plugin {
                plugin,
                implemented: true,
            },
            multi: false,
            min_size: (480, 320),
        }
    }

    pub const fn planned(
        unit: crate::UnitKind,
        plugin: &'static str,
        id: &'static str,
        title: &'static str,
    ) -> Self {
        SurfaceSpec {
            unit,
            id,
            title,
            key: None,
            origin: Origin::Plugin {
                plugin,
                implemented: false,
            },
            multi: false,
            min_size: (320, 200),
        }
    }

    /// The placeholder window of a planned Unit.
    pub const fn placeholder(unit: crate::UnitKind, id: &'static str, title: &'static str) -> Self {
        SurfaceSpec {
            unit,
            id,
            title,
            key: None,
            origin: Origin::Placeholder,
            multi: false,
            min_size: (320, 200),
        }
    }

    /// The smallest window that shows everything (logical px).
    pub const fn min(mut self, w: u16, h: u16) -> Self {
        self.min_size = (w, h);
        self
    }

    /// Allow several windows of this surface at once.
    pub const fn multi(mut self) -> Self {
        self.multi = true;
        self
    }

    /// Window title: "Patch · Mixer", or "Patch · DJ Booth · Deck A" when the
    /// surface isn't named after its Unit. Hyprland rules and focus match on it.
    pub fn window_title(&self, app: crate::AppId) -> String {
        self.instance_window_title(app, 1)
    }

    /// Title of instance `n` (1 = the first window).
    pub fn instance_title(&self, n: u32) -> String {
        if n <= 1 {
            self.title.to_string()
        } else {
            format!("{} {n}", self.title)
        }
    }

    /// Window title of instance `n`: "Patch · Meter 2", "Patch · DJ Booth · Deck A".
    pub fn instance_window_title(&self, app: crate::AppId, n: u32) -> String {
        self.named_window_title(app, &self.instance_title(n))
    }

    /// Window title of a window the app names itself ("Patch · Plugins ·
    /// Graillon 3" for one rack plugin's window).
    pub fn named_window_title(&self, app: crate::AppId, title: &str) -> String {
        if title == self.unit.name() || (self.title == self.unit.name() && title.starts_with(self.title)) {
            format!("{} · {}", app.name(), title)
        } else {
            format!("{} · {} · {}", app.name(), self.unit.name(), title)
        }
    }
}
