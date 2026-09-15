//! The example config is the deployed config's template, so it has to parse.
//!
//! Adding a field to `Config` and forgetting the example, or writing an example the parser
//! rejects, is only discovered when a deploy fails. Parsing it here is cheap.

use tee_coprocessor::config::Config;

#[test]
fn the_shipped_example_parses_and_names_our_routers() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/coprocessor.toml.example"
    ))
    .expect("example config");
    let config: Config = toml::from_str(&raw).expect("example config parses");

    assert!(config.tick_secs > 0);
    assert!(
        config.celestia_lag >= 1,
        "the app hash for H lives in H+1, so a lag below one cannot be attested"
    );
    assert!(!config.routes.is_empty());

    // Every route should know what belongs to it; a route without routers still works, but
    // falls back to waking on any traffic for its chain.
    for route in &config.routes {
        assert!(
            !route.routers.is_empty(),
            "route {} has no routers, so it will prove for other people's messages",
            route.name
        );
    }
}
