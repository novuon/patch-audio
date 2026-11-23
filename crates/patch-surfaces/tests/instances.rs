//! Several windows of one surface: Meters and Library can have instances,
//! every other surface stays single. Runs the real session host on the
//! headless screenshot platform (mock provider, no compositor).

use patch_core::ipc::{Request, Status};
use patch_host::Session;
use patch_surfaces::AudioApp;

fn open_ids(s: &Session<AudioApp>) -> Vec<String> {
    Status::parse(&s.handle(Request::Status))
        .expect("status")
        .open
}

#[test]
fn instances() {
    patch_host::shot::install().expect("shot platform");
    let s = Session::<AudioApp>::new();
    let req = |r: Request| s.handle(r);

    // A multi surface opens another window each time it is asked for.
    assert_eq!(req(Request::Open("meters".into())), "ok");
    assert_eq!(req(Request::Open("meters".into())), "ok");
    assert_eq!(req(Request::Open("library".into())), "ok");
    assert_eq!(req(Request::Open("library".into())), "ok");
    assert_eq!(open_ids(&s), ["meters", "meters#2", "library", "library#2"]);

    // Single surfaces are focused, never duplicated, and have no instances.
    assert_eq!(req(Request::Open("mixer".into())), "ok");
    assert_eq!(req(Request::Open("mixer".into())), "ok");
    assert_eq!(req(Request::Open("mixer#2".into())), "err");
    assert_eq!(
        open_ids(&s)
            .iter()
            .filter(|o| o.starts_with("mixer"))
            .count(),
        1
    );

    // `close meters` closes the newest; an instance id closes that window.
    req(Request::Open("meters".into()));
    assert!(open_ids(&s).contains(&"meters#3".to_string()));
    req(Request::Close("meters".into()));
    assert!(!open_ids(&s).contains(&"meters#3".to_string()));
    req(Request::Close("meters".into()));
    assert_eq!(
        open_ids(&s)
            .iter()
            .filter(|o| o.starts_with("meters"))
            .collect::<Vec<_>>(),
        ["meters"]
    );
    req(Request::Close("library#2".into()));
    assert!(open_ids(&s).contains(&"library".to_string()));
    assert!(!open_ids(&s).contains(&"library#2".to_string()));

    // Freed numbers are reused, lowest first.
    req(Request::Open("meters".into()));
    req(Request::Open("meters".into()));
    req(Request::Close("meters#2".into()));
    req(Request::Open("meters".into()));
    assert_eq!(
        open_ids(&s)
            .iter()
            .filter(|o| o.starts_with("meters"))
            .count(),
        3
    );
    assert!(open_ids(&s).contains(&"meters#2".to_string()));

    // Toggle closes one instance at a time, then opens again.
    for _ in 0..3 {
        req(Request::Toggle("meters".into()));
    }
    assert!(!open_ids(&s).iter().any(|o| o.starts_with("meters")));
    req(Request::Toggle("meters".into()));
    assert_eq!(
        open_ids(&s)
            .iter()
            .filter(|o| o.starts_with("meters"))
            .collect::<Vec<_>>(),
        ["meters"]
    );

    // A layout is a complete arrangement: windows it doesn't name close.
    req(Request::Open("meters".into()));
    assert_eq!(req(Request::Layout("compact".into())), "ok");
    assert!(!open_ids(&s).contains(&"meters#2".to_string()));
}
