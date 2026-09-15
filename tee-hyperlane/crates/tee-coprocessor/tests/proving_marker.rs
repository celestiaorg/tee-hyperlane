//! The "this route is on the CPU" marker must not outlive the process that claimed it.
//!
//! It is removed when its guard drops, which covers every ordinary end to a tick including an
//! error. It does not cover the process being killed, and a service gets killed every time it
//! is upgraded. A marker left behind that way made the dashboard report a route as proving
//! for as long as the file sat there, which is the exact misreport the marker exists to
//! prevent.

use tee_coprocessor::tasks::ProofStore;

#[test]
fn a_marker_is_cleared_while_its_guard_lives_and_after_it_drops() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProofStore::new(dir.path());
    store.prepare("route").unwrap();
    let marker = dir.path().join("route").join("proving");

    {
        let _guard = store.mark_proving("route");
        assert!(marker.exists(), "a route holding the prover should say so");
    }
    assert!(!marker.exists(), "dropping the guard should clear it");
}

#[test]
fn a_marker_left_by_a_killed_process_is_cleared_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProofStore::new(dir.path());
    store.prepare("route").unwrap();
    let marker = dir.path().join("route").join("proving");

    // What a kill leaves behind: the file, with no guard anywhere to remove it.
    std::fs::write(&marker, b"").unwrap();
    assert!(marker.exists());

    // Starting up again holds no permit, so anything here belongs to a process that is gone.
    let restarted = ProofStore::new(dir.path());
    restarted.prepare("route").unwrap();
    assert!(
        !marker.exists(),
        "a marker surviving a restart reports a dead process as still proving"
    );
}

#[test]
fn preparing_a_route_does_not_disturb_a_staged_batch() {
    // Clearing the marker must not be mistaken for clearing the work.
    let dir = tempfile::tempdir().unwrap();
    let store = ProofStore::new(dir.path());
    store.prepare("route").unwrap();
    let staged = store.staging("route", "proved.json");
    std::fs::write(&staged, b"{}").unwrap();

    ProofStore::new(dir.path()).prepare("route").unwrap();
    assert!(staged.exists(), "a finished proof must survive a restart");
}
