//! The EQ surface's bindings.

use super::*;

impl AudioApp {
    /// EQ windows: edits show at once (local), and go to the service as
    /// validated commands (the engine glides to them).
    pub(super) fn bind_eq(&self, w: &PatchWindow) {
        let e = w.global::<crate::ui::EqState>();
        let send = {
            let me = self.me.clone();
            move |cmd: Command| {
                if let Some(app) = me.upgrade()
                    && let Err(err) = app.command(cmd, Origin::User)
                {
                    *app.toast.borrow_mut() = format!("EQ: {err}");
                    app.push_toast();
                }
            }
        };
        // The window's target (which EQ it edits): a fixed one or "app:<name>".
        let target = {
            let ew = w.as_weak();
            move || -> String {
                ew.upgrade()
                    .map(|w| w.global::<crate::ui::EqState>().get_target().to_string())
                    .filter(|t| patch_core::eq::valid_target(t))
                    .unwrap_or_else(|| "master".into())
            }
        };
        // Change band i of this window's target locally, then send it.
        let edit = {
            let (panel, send, target) = (self.master_eq.clone(), send.clone(), target.clone());
            move |i: usize, f: &dyn Fn(&patch_core::eq::EqBand) -> Option<patch_core::eq::EqBand>| {
                let t = target();
                let mut st = panel.get(&t);
                if let Some(n) = st.bands.get(i).and_then(f) {
                    st.bands[i] = n;
                    st.edited = !st.preset.is_empty();
                    panel.set(&t, st);
                    panel.touch();
                    send(crate::eq_view::command(&t, i, &n));
                }
            }
        };
        let ew = w.as_weak();
        e.on_select(move |i| {
            if let Some(w) = ew.upgrade() {
                let e = w.global::<crate::ui::EqState>();
                if e.get_selected() != i {
                    e.set_entry("".into());
                }
                e.set_selected(i.clamp(0, patch_core::eq::BANDS as i32 - 1));
            }
        });
        // Typing a value for the selected band: f (frequency), g (gain),
        // Q (Q), or a click on the value; enter sets it, esc cancels.
        let ew = w.as_weak();
        e.on_begin_entry(move |field| {
            if let Some(w) = ew.upgrade() {
                begin_entry(&w, field.as_str());
            }
        });
        let ed = edit.clone();
        e.on_drag(move |i, x, y| {
            ed(i.max(0) as usize, &|b| {
                Some(crate::eq_view::dragged(b, x, y))
            })
        });
        let ed = edit.clone();
        e.on_wheel(move |i, d| {
            ed(i.max(0) as usize, &|b| {
                crate::eq_view::keyed(b, if d > 0.0 { "]" } else { "[" }, false)
            })
        });
        let ed = edit.clone();
        e.on_toggle(move |i| {
            ed(i.max(0) as usize, &|b| {
                Some(patch_core::eq::EqBand { on: !b.on, ..*b })
            })
        });
        let ed = edit.clone();
        e.on_cycle_kind(move |i| {
            ed(i.max(0) as usize, &|b| {
                Some(patch_core::eq::EqBand {
                    kind: crate::eq_view::next_kind(b.kind),
                    on: true,
                    ..*b
                })
            })
        });
        let (panel, s2, t2) = (self.master_eq.clone(), send.clone(), target.clone());
        e.on_set_bypass(move |on| {
            let t = t2();
            let mut st = panel.get(&t);
            st.bypass = on;
            panel.set(&t, st);
            panel.touch();
            s2(Command::EqBypass {
                target: Some(t),
                on,
            });
        });
        let (panel, s8, t8, ew8) = (self.master_eq.clone(), send.clone(), target.clone(), w.as_weak());
        e.on_set_gain(move |db| {
            let t = t8();
            let mut st = panel.get(&t);
            let range = patch_core::eq::GAIN_RANGE;
            // Tenths: a correction's -6.2 stays, a step lands on halves.
            let db = ((db * 10.0).round() / 10.0).clamp(-range, range);
            if db == st.gain_db {
                return;
            }
            st.gain_db = db;
            st.edited = !st.preset.is_empty();
            // Shown now: the next key steps from here, not from the last frame.
            if let Some(w) = ew8.upgrade() {
                w.global::<crate::ui::EqState>().set_gain(db);
            }
            panel.set(&t, st);
            panel.touch();
            s8(Command::EqGain { target: Some(t), db });
        });
        let panel = self.master_eq.clone();
        e.on_toggle_spectrum(move || panel.spectrum.set(!panel.spectrum.get()));
        let (panel, s4, t4) = (self.master_eq.clone(), send.clone(), target.clone());
        e.on_next_preset(move |dir| {
            let t = t4();
            if let Some(name) = panel.step_preset(&t, dir) {
                s4(Command::EqPreset {
                    target: Some(t),
                    name,
                });
                panel.refresh_now();
            }
        });
        let (panel, s5, t5) = (self.master_eq.clone(), send.clone(), target.clone());
        e.on_save_preset(move || {
            let t = t5();
            let name = panel.save_name(&t);
            s5(Command::EqSave {
                target: Some(t),
                name,
            });
            panel.refresh_now();
        });
        let (panel, s6, ew6) = (self.master_eq.clone(), send.clone(), w.as_weak());
        e.on_add_app(move |app| {
            s6(Command::EqApp {
                app: app.to_string(),
                on: true,
            });
            panel.apps.borrow_mut().push(app.to_string());
            if let Some(w) = ew6.upgrade() {
                w.global::<crate::ui::EqState>()
                    .set_target(format!("app:{app}").into());
            }
            panel.refresh_now();
        });
        let (panel, s7, ew7) = (self.master_eq.clone(), send.clone(), w.as_weak());
        e.on_remove_app(move |id| {
            let app = id.trim_start_matches("app:").to_string();
            s7(Command::EqApp {
                app: app.clone(),
                on: false,
            });
            panel.apps.borrow_mut().retain(|a| *a != app);
            if let Some(w) = ew7.upgrade() {
                let e = w.global::<crate::ui::EqState>();
                if e.get_target() == id {
                    e.set_target("master".into());
                }
            }
            panel.refresh_now();
        });
        let (panel, s3, t3) = (self.master_eq.clone(), send.clone(), target.clone());
        e.on_reset(move || {
            let t = t3();
            panel.set(&t, patch_core::eq::EqState::default());
            panel.touch();
            s3(Command::EqReset { target: Some(t) });
        });
        let (ew, panel_m) = (w.as_weak(), self.master_eq.clone());
        e.on_key(move |text, shift| {
            let Some(w) = ew.upgrade() else { return false };
            let e = w.global::<crate::ui::EqState>();
            let field = e.get_entry().to_string();
            if !field.is_empty() {
                let mut typed = e.get_entry_text().to_string();
                match text.as_str() {
                    "\n" | "\r" => {
                        let i = e.get_selected().max(0) as usize;
                        let t = panel_m.get(&target());
                        match t.bands.get(i).and_then(|b| crate::eq_view::typed(b, &field, &typed)) {
                            Some(n) => edit(i, &|_| Some(n)),
                            None => flash(&w),
                        }
                        e.set_entry("".into());
                        return true;
                    }
                    "\u{1b}" => {
                        e.set_entry("".into());
                        return true;
                    }
                    "\u{8}" | "\u{7f}" => {
                        typed.pop();
                    }
                    "f" | "g" | "Q" => {
                        begin_entry(&w, entry_field(text.as_str()));
                        return true;
                    }
                    t if t.chars().count() == 1
                        && t.chars().all(|c| c.is_ascii_digit() || ".,-+kK".contains(c))
                        && typed.chars().count() < 10 =>
                    {
                        typed.push_str(t);
                    }
                    // Tab, the controller: the window's own keys still work.
                    "\t" => return false,
                    t if t.chars().any(|c| c.is_control() || ('\u{f700}'..='\u{f8ff}').contains(&c)) => return false,
                    _ => {
                        flash(&w);
                        return true;
                    }
                }
                e.set_entry_text(typed.clone().into());
                e.set_entry_prompt(crate::eq_view::entry_prompt(&field, &typed).into());
                return true;
            }
            if matches!(text.as_str(), "f" | "g" | "Q") {
                begin_entry(&w, entry_field(text.as_str()));
                return true;
            }
            if text.as_str() == "m" {
                // The next EQ target, wrapping: fixed ones, then apps.
                let e = w.global::<crate::ui::EqState>();
                let mut ids: Vec<String> =
                    panel_m.fixed.borrow().iter().map(|(t, _)| t.clone()).collect();
                ids.extend(panel_m.apps.borrow().iter().map(|a| format!("app:{a}")));
                let cur = e.get_target().to_string();
                let i = ids
                    .iter()
                    .position(|t| *t == cur)
                    .map(|i| (i + 1) % ids.len())
                    .unwrap_or(0);
                e.set_target(ids[i].clone().into());
                return true;
            }
            let i = w.global::<crate::ui::EqState>().get_selected().max(0) as usize;
            let known =
                crate::eq_view::keyed(&patch_core::eq::EqBand::default(), text.as_str(), shift)
                    .is_some();
            if known {
                edit(i, &|b| crate::eq_view::keyed(b, text.as_str(), shift));
            }
            known
        });
        if self.frozen {
            self.master_eq.set("master", crate::eq_view::demo());
        } else if let Some(t) = self.master_eq.remembered_target() {
            // The tab last chosen (a restart or an update reopened it on
            // Master, which read as lost settings).
            e.set_target(t.into());
        }
        // Show it at once (no blank first frame).
        self.master_eq.touch();
    }
}

fn entry_field(key: &str) -> &'static str {
    match key {
        "f" => "freq",
        "g" => "gain",
        _ => "q",
    }
}

/// Start typing `field` for the selected band (a pass filter has no gain:
/// the window flashes instead).
fn begin_entry(w: &PatchWindow, field: &str) {
    let e = w.global::<crate::ui::EqState>();
    let i = e.get_selected().max(0) as usize;
    let no_gain = slint::Model::row_data(&e.get_bands(), i).is_some_and(|b| b.gain.as_str() == "—");
    if field == "gain" && no_gain {
        flash(w);
        return;
    }
    e.set_entry(field.into());
    e.set_entry_text("".into());
    e.set_entry_prompt(crate::eq_view::entry_prompt(field, "").into());
}

/// A key the EQ can't use: a short flash, so nothing happening is visible.
fn flash(w: &PatchWindow) {
    let e = w.global::<crate::ui::EqState>();
    e.set_invalid(true);
    let ew = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(180), move || {
        if let Some(w) = ew.upgrade() {
            w.global::<crate::ui::EqState>().set_invalid(false);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_host::session::HostedApp;
    use slint::platform::WindowEvent;

    /// The EQ takes its keys the moment its window has focus: a tester had
    /// to click a band before `1`–`8`, the arrows or `[ ]` did anything.
    #[test]
    fn the_eq_takes_keys_without_a_click() {
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let w = app.create("eq");
        w.show().unwrap();
        // A frame first: the surface is built when the window first draws.
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 900, 600);
        w.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
        let text: slint::SharedString = "2".into();
        w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text });
        assert_eq!(w.global::<crate::ui::EqState>().get_selected(), 1);
    }

    /// Ctrl+S (a save habit) and the other Ctrl chords are not the EQ's
    /// letters: the spectrum, bypass and gain stay; `s` alone still works.
    #[test]
    fn ctrl_chords_are_not_eq_letters() {
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let w = app.create("eq");
        w.show().unwrap();
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 900, 600);
        w.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
        let ctrl: slint::SharedString = slint::platform::Key::Control.into();
        let press = |t: &str| {
            let text: slint::SharedString = t.into();
            w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            w.window().dispatch_event(WindowEvent::KeyReleased { text });
        };
        w.window().dispatch_event(WindowEvent::KeyPressed { text: ctrl.clone() });
        for k in ["s", "b", "=", "m"] {
            press(k);
        }
        w.window().dispatch_event(WindowEvent::KeyReleased { text: ctrl });
        assert!(app.master_eq.spectrum.get(), "Ctrl+S left the spectrum on");
        let st = app.master_eq.get("master");
        assert!(!st.bypass, "Ctrl+B didn't bypass");
        assert_eq!(st.gain_db, 0.0, "Ctrl+= didn't raise the gain");
        assert_eq!(w.global::<crate::ui::EqState>().get_target().as_str(), "master");
        press("s");
        assert!(!app.master_eq.spectrum.get(), "s alone turns it off");
    }

    /// `-` and `=` move the input gain in half decibels (a mock app: the
    /// command is refused, the window shows the change at once).
    #[test]
    fn minus_and_equals_move_the_input_gain() {
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let w = app.create("eq");
        w.show().unwrap();
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 900, 600);
        w.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
        let key = |k: &str| {
            let text: slint::SharedString = k.into();
            w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            w.window().dispatch_event(WindowEvent::KeyReleased { text });
        };
        key("-");
        key("-");
        key("=");
        assert_eq!(app.master_eq.get("master").gain_db, -0.5);
        assert_eq!(w.global::<crate::ui::EqState>().get_gain(), -0.5);
    }

    /// "f 400 enter" moves the selected band to 400 Hz; digits while typing
    /// don't select bands; esc cancels; a letter that means nothing flashes.
    #[test]
    fn a_frequency_is_typed() {
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let w = app.create("eq");
        w.show().unwrap();
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 900, 600);
        w.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
        let press = |t: &str| {
            let text: slint::SharedString = t.into();
            w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            w.window().dispatch_event(WindowEvent::KeyReleased { text });
        };
        let e = w.global::<crate::ui::EqState>();
        press("3");
        for k in ["f", "4", "0", "0"] {
            press(k);
        }
        assert_eq!(e.get_selected(), 2, "digits went to the value, not the band");
        assert_eq!(e.get_entry_text().as_str(), "400");
        press("x");
        assert!(e.get_invalid(), "a key that means nothing flashes");
        press("\n");
        assert_eq!(e.get_entry().as_str(), "");
        assert_eq!(app.master_eq.get("master").bands[2].freq, 400.0);
        let gain = app.master_eq.get("master").bands[2].gain_db;
        press("g");
        press("5");
        press("\u{1b}");
        assert_eq!(e.get_entry().as_str(), "");
        assert_eq!(app.master_eq.get("master").bands[2].gain_db, gain, "esc cancelled");
        // While typing, - is a minus sign, not the input gain.
        press("g");
        press("-");
        press("3");
        assert_eq!(e.get_entry_text().as_str(), "-3");
        assert_eq!(app.master_eq.get("master").gain_db, 0.0);
        press("\n");
        assert_eq!(app.master_eq.get("master").bands[2].gain_db, -3.0);
    }
}
