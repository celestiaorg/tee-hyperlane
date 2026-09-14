//! A short log scan has to fail here, not inside the enclave.
//!
//! The failure it guards against is a pruned endpoint answering `eth_getLogs` with an empty
//! array instead of an error, which is indistinguishable from "nothing happened" unless
//! something independent says how many leaves the range must hold. The tree's own counts do.

use tee_coprocessor::commands::check_scan;

#[test]
fn a_scan_that_found_every_leaf_passes() {
    assert!(check_scan(107_099, 107_107, 8).is_ok());
}

#[test]
fn a_short_scan_names_how_many_are_missing() {
    // The live base-to-celestia failure: publicnode had pruned the older half of the range.
    let err = check_scan(107_089, 107_107, 8).unwrap_err().to_string();
    assert!(err.contains("grew by 18 leaves"), "{err}");
    assert!(err.contains("found 8"), "{err}");
    assert!(err.contains("missing 10"), "{err}");
    assert!(err.contains("logs_rpc"), "{err}");
}

#[test]
fn a_scan_returning_more_than_the_tree_grew_also_fails() {
    // Double-counting is as wrong as dropping, and the enclave would reject it just as hard.
    assert!(check_scan(107_099, 107_107, 9).is_err());
}

#[test]
fn a_quiet_range_expects_nothing() {
    assert!(check_scan(107_107, 107_107, 0).is_ok());
}
