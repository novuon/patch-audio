//! Exercise the actual Slint callbacks on the headless platform.
use patch_host::HostedApp;
use patch_surfaces::{AudioApp, ui::SeqState};
use slint::{ComponentHandle, Model};

#[test]
fn edits_reach_the_model_and_survive_reopening() {
    patch_host::shot::install().unwrap();
    let app = AudioApp::new();
    let window = app.create("sequencer");
    let s = window.global::<SeqState>();
    assert!(s.get_ready());
    assert_eq!(s.get_tracks().row_count(), 8);
    s.invoke_cell(0, 1, true);
    assert_eq!(s.get_velocity(), 100);
    s.invoke_action("velocity".into(), 51);
    s.invoke_action("probability".into(), 34);
    s.invoke_action("repeats".into(), 3);
    s.invoke_cell(0, 1, false);
    assert_eq!(s.get_velocity(), 51);
    assert_eq!(s.get_probability(), 34);
    assert_eq!(s.get_repeats(), 3);
    s.invoke_action("duplicate".into(), 0);
    assert_eq!(s.get_length(), 32);
    s.invoke_cell(0, 17, false);
    assert_eq!(s.get_velocity(), 51);
    s.invoke_action("mute".into(), 0);
    assert!(s.get_tracks().row_data(0).unwrap().muted);
    drop(window);
    let reopened = app.create("sequencer");
    let s = reopened.global::<SeqState>();
    assert_eq!(s.get_step(), 17);
    assert_eq!(s.get_length(), 32);
    assert_eq!(s.get_velocity(), 51);
    s.invoke_action("clear".into(), 0);
    let steps = s.get_tracks().row_data(0).unwrap().steps;
    for i in 0..32 {
        assert_eq!(steps.row_data(i), Some(0));
    }
    s.invoke_cell(-1, 200, true); // Invalid callback indices must be harmless.
    assert_eq!(s.get_step(), 17);
}
