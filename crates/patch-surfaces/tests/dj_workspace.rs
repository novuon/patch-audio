//! Actual unified-workspace callbacks; no live audio or persistent user data.
use patch_host::HostedApp;
use patch_surfaces::{
    AudioApp,
    ui::{DjState, DjWorkspaceState},
};
use slint::{ComponentHandle, Model};
#[test]
fn aliases_keep_all_views_and_draft_edits_do_not_change_live_queue() {
    patch_host::shot::install().unwrap();
    let app = AudioApp::new();
    let window = app.create("dj");
    let s = window.global::<DjWorkspaceState>();
    let before = window.global::<DjState>().get_queue().row_count();
    s.invoke_action("add_path".into(), "/missing/first.flac".into());
    s.invoke_action("add_path".into(), "/missing/second.flac".into());
    assert_eq!(s.get_draft_tracks().row_count(), 2);
    s.invoke_action("pin".into(), "1".into());
    s.invoke_action("draft_up".into(), "1".into());
    assert!(s.get_draft_tracks().row_data(0).unwrap().selected);
    assert_eq!(s.get_draft_tracks().row_data(0).unwrap().title, "second");
    assert_eq!(window.global::<DjState>().get_queue().row_count(), before);
    s.invoke_action("draft_remove".into(), "999".into());
    assert!(s.get_message().contains("retry"));
    assert_eq!(s.get_draft_tracks().row_count(), 2);
    let spec = AudioApp::spec();
    assert!(
        spec.surface("dj").is_some(),
        "the unified DJ Booth remains available"
    );
    assert!(
        spec.surface("light-dj").is_none(),
        "legacy Light DJ is disabled"
    );
    assert!(
        spec.surface("ai-dj").is_none(),
        "legacy standalone Auto Mix is disabled"
    );
    assert!(spec.surface("ai-stage").is_none(), "AI Stage is gone");
}
