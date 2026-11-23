//! Routing on the graph: connecting, dropping onto the grid, through-Patch toggles, restoring and refreshing external nodes.

use super::*;

impl AudioApp {
    /// Pick up changes made outside Patch (new streams, volume keys, devices).
    pub(super) fn refresh_external(&self) {
        let changed = self.provider.borrow_mut().refresh();
        if let Some(n) = self.provider.borrow_mut().take_notice() {
            *self.toast.borrow_mut() = n;
            self.push_toast();
        }
        if changed {
            self.fix_selection();
            self.rebuild_graph();
            self.rebuild_strips();
            self.rebuild_devices();
            self.rebuild_selection();
        } else {
            self.sync_strips();
        }
    }

    /// Keep the selection on something that exists.
    pub(super) fn fix_selection(&self) {
        let nodes = self.provider_nodes();
        let sel = self.selected.borrow().clone();
        // Engine microphone channels are Mixer strips, not Router nodes.
        // Validate against both inventories so stream churn cannot discard them.
        if !selection_exists(
            &sel,
            nodes.iter().map(|n| n.id),
            self.provider.borrow().strips().iter().map(|s| s.node),
        ) {
            let first = nodes
                .iter()
                .find(|n| n.kind == NodeKind::Source)
                .map(|n| n.id)
                .unwrap_or("master");
            *self.selected.borrow_mut() = first.to_string();
        }
    }

    pub(super) fn toggle_through_patch(&self, id: &str) {
        let current = self.provider.borrow().through_patch(id);
        let Some(through) = current else {
            *self.toast.borrow_mut() = "Only playback streams can go through Patch.".into();
            self.push_toast();
            return;
        };
        let result = self.provider.borrow_mut().route_through_patch(id, !through);
        *self.toast.borrow_mut() = match result {
            Ok(()) if !through => "Playing through the Patch bus.".into(),
            Ok(()) => "Playing straight to the output.".into(),
            Err(e) => format!("Routing failed: {e}"),
        };
        self.after_routing();
    }

    pub(super) fn connect(&self, from: &str, to: &str) {
        let to_name = self
            .provider
            .borrow()
            .targets(from)
            .into_iter()
            .find(|(id, _, _)| *id == to)
            .map(|(_, name, _)| name.to_string())
            .or_else(|| {
                self.provider_nodes()
                    .into_iter()
                    .find(|n| n.id == to)
                    .map(|n| n.name.to_string())
            })
            .unwrap_or_else(|| to.to_string());
        let main_out = from == "output";
        let to_name = if main_out {
            self.provider
                .borrow()
                .main_outputs()
                .into_iter()
                .find(|o| o.id == to)
                .map(|o| o.name)
                .unwrap_or(to_name)
        } else {
            to_name
        };
        let r = self.provider.borrow_mut().connect(from, to);
        *self.toast.borrow_mut() = match r {
            Ok(()) if main_out => format!("Main output → {to_name}."),
            Ok(()) => format!("Patched to {to_name}."),
            Err(e) => format!("Can't patch there: {e}"),
        };
        self.after_routing();
    }

    /// A cable was dropped in the Graph at (column, row).
    pub(super) fn drop_at(&self, from: &str, col: i32, row: i32) {
        let target = self
            .nodes
            .iter()
            .find(|n| n.col == col && n.row == row)
            .map(|n| n.id.to_string());
        match target {
            Some(t) if t != from => self.connect(from, &t),
            _ => {
                *self.toast.borrow_mut() = "Drop the cable on a destination to patch.".into();
                self.push_toast();
            }
        }
    }

    pub(super) fn restore_routing(&self) {
        let result = self.provider.borrow_mut().restore_routing();
        *self.toast.borrow_mut() = match result {
            Ok(()) => "Restored: streams play straight to the output, Patch bus removed.".into(),
            Err(e) => format!("Restore failed: {e}"),
        };
        self.after_routing();
    }

    pub(super) fn after_routing(&self) {
        self.fix_selection();
        self.rebuild_graph();
        self.rebuild_strips();
        self.rebuild_devices();
        self.rebuild_selection();
        self.push_toast();
    }

    pub(super) fn push_toast(&self) {
        let t: SharedString = self.toast.borrow().as_str().into();
        self.broadcast(|s| s.set_toast(t.clone()));
    }
}
