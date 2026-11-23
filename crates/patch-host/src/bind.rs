/// Implement [`SurfaceWindow`](crate::SurfaceWindow) for a Unit's generated
/// window type. Expects the design-system types (`Host`, `Theme`,
/// `SurfaceItem`, `UnitItem`, `LayoutItem`, `TileItem`, `KeyHint`,
/// `SettingItem`, `ProfileItem`) in scope.
#[macro_export]
macro_rules! surface_window {
    ($ty:ty) => {
        impl $crate::SurfaceWindow for $ty {
            fn window(&self) -> &$crate::slint::Window {
                $crate::slint::ComponentHandle::window(self)
            }
            fn show(&self) -> Result<(), $crate::slint::PlatformError> {
                $crate::slint::ComponentHandle::show(self)
            }
            fn hide(&self) -> Result<(), $crate::slint::PlatformError> {
                $crate::slint::ComponentHandle::hide(self)
            }
            fn apply_theme(&self, palette: &$crate::patch_ui::Palette) {
                $crate::patch_ui::apply_theme!(self, palette);
            }
            fn set_style(&self, style: $crate::patch_ui::style::Style) {
                use $crate::slint::ComponentHandle as _;
                let t = self.global::<Theme>();
                t.set_patch_style(style.patch_style());
                t.set_pro(style.pro());
            }
            fn apply_host(&self, h: &$crate::HostState) {
                use $crate::slint::{ComponentHandle as _, ModelRc, VecModel};
                let hints = |v: &[(String, String)]| {
                    ModelRc::new(VecModel::from(
                        v.iter()
                            .map(|(k, l)| KeyHint {
                                key: k.into(),
                                label: l.into(),
                            })
                            .collect::<Vec<_>>(),
                    ))
                };
                let g = self.global::<Host>();
                g.set_app_name(h.app_name.as_str().into());
                g.set_app_label(h.app_name.to_uppercase().into());
                g.set_app_kind(h.app_kind.as_str().into());
                g.set_unit_name(h.unit_name.as_str().into());
                g.set_unit_scope(h.unit_scope.as_str().into());
                g.set_surface_id(h.surface_id.as_str().into());
                g.set_window_id(h.window_id.as_str().into());
                g.set_surface_title(h.surface_title.as_str().into());
                g.set_heading(h.heading.as_str().into());
                g.set_window_title(h.window_title.as_str().into());
                g.set_settings(ModelRc::new(VecModel::from(
                    h.settings
                        .iter()
                        .map(|s| SettingItem {
                            id: s.id.as_str().into(),
                            label: s.label.as_str().into(),
                            detail: s.detail.as_str().into(),
                            group: s.group.as_str().into(),
                            on: s.on,
                            action: s.action,
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_profiles(ModelRc::new(VecModel::from(
                    h.profiles
                        .iter()
                        .map(|p| ProfileItem {
                            id: p.id.as_str().into(),
                            name: p.name.as_str().into(),
                            about: p.about.as_str().into(),
                            active: p.active,
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_units(ModelRc::new(VecModel::from(
                    h.units
                        .iter()
                        .map(|u| UnitItem {
                            name: u.name.as_str().into(),
                            scope: u.scope.as_str().into(),
                            status: u.status.as_str().into(),
                            open: u.open,
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_unit_groups(ModelRc::new(VecModel::from(
                    ["live", "mock", "planned"]
                        .iter()
                        .filter_map(|status| {
                            let names: Vec<&str> = h
                                .units
                                .iter()
                                .filter(|u| u.status == *status)
                                .map(|u| u.name.as_str())
                                .collect();
                            (!names.is_empty()).then(|| KeyHint {
                                key: (*status).into(),
                                label: names.join(" · ").into(),
                            })
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_surfaces(ModelRc::new(VecModel::from(
                    h.surfaces
                        .iter()
                        .map(|s| SurfaceItem {
                            id: s.id.as_str().into(),
                            title: s.title.as_str().into(),
                            unit: s.unit.as_str().into(),
                            key: s.key.as_str().into(),
                            open: s.open,
                            group: s.group.as_str().into(),
                            first: s.first,
                            section: s.section,
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_layouts(ModelRc::new(VecModel::from(
                    h.layouts
                        .iter()
                        .map(|l| LayoutItem {
                            id: l.id.as_str().into(),
                            name: l.name.as_str().into(),
                            summary: l.summary.as_str().into(),
                            available: l.available,
                            note: l.note.as_str().into(),
                            tiles: ModelRc::new(VecModel::from(
                                l.tiles
                                    .iter()
                                    .map(|t| TileItem {
                                        surface: t.surface.as_str().into(),
                                        title: t.title.as_str().into(),
                                        x: t.rect[0],
                                        y: t.rect[1],
                                        w: t.rect[2],
                                        h: t.rect[3],
                                        planned: t.planned,
                                    })
                                    .collect::<Vec<_>>(),
                            )),
                        })
                        .collect::<Vec<_>>(),
                )));
                g.set_active_layout(h.active_layout.as_str().into());
                g.set_hints(hints(&h.hints));
                g.set_common_hints(hints(&h.common_hints));
                g.set_global_hints(hints(&h.global_hints));
                g.set_surface_keys(hints(&h.surface_keys));
                g.set_provenance(h.provenance.as_str().into());
                g.set_badge(h.badge.as_str().into());
                g.set_version(h.version.as_str().into());
                g.set_hyprland(h.hyprland);
            }
            fn bind_host(&self, host: std::rc::Weak<dyn $crate::HostOps>, surface: &str) {
                use $crate::slint::ComponentHandle as _;
                let g = self.global::<Host>();
                let h = host.clone();
                g.on_toggle_surface(move |id| {
                    if let Some(h) = h.upgrade() {
                        h.toggle(&id)
                    }
                });
                let h = host.clone();
                g.on_focus_surface(move |id| {
                    if let Some(h) = h.upgrade() {
                        h.open(&id);
                    }
                });
                let h = host.clone();
                g.on_open_layout(move |id| {
                    if let Some(h) = h.upgrade() {
                        h.open_layout(&id);
                    }
                });
                let h = host.clone();
                g.on_show_controller(move || {
                    if let Some(h) = h.upgrade() {
                        h.controller()
                    }
                });
                let h = host.clone();
                let s = surface.to_string();
                g.on_close_window(move || {
                    if let Some(h) = h.upgrade() {
                        h.close(&s)
                    }
                });
                let h = host.clone();
                g.on_quit_session(move || {
                    if let Some(h) = h.upgrade() {
                        h.quit()
                    }
                });
                let h = host.clone();
                g.on_toggle_setting(move |id| {
                    if let Some(h) = h.upgrade() {
                        h.toggle_setting(&id)
                    }
                });
                let h = host.clone();
                g.on_choose_profile(move |id| {
                    if let Some(h) = h.upgrade() {
                        h.choose_profile(&id)
                    }
                });
                let h = host.clone();
                let s = surface.to_string();
                g.on_make_room(move || {
                    if let Some(h) = h.upgrade() {
                        h.make_room(&s)
                    }
                });
                let h = host.clone();
                let s = surface.to_string();
                g.on_make_room_for(move |w, hh| {
                    if let Some(h) = h.upgrade() {
                        h.make_room_for(&s, w.max(0) as u32, hh.max(0) as u32)
                    }
                });
                let h = host.clone();
                let s = surface.to_string();
                g.on_keep_small(move || {
                    if let Some(h) = h.upgrade() {
                        h.keep_small(&s)
                    }
                });
                let h = host;
                let s = surface.to_string();
                g.on_key(move |text, ctrl, shift| {
                    h.upgrade().is_some_and(|h| h.key(&s, &text, ctrl, shift))
                });
            }
            fn set_size_note(&self, note: &str) {
                use $crate::slint::ComponentHandle as _;
                let g = self.global::<Host>();
                if g.get_size_note().as_str() != note {
                    g.set_size_note(note.into());
                }
            }
        }
    };
}
