//! The shipped example is the live deployment's config, so it has to parse, and every route in
//! it has to build: an origin its `from` chain can index, a destination its `to` chain can serve.

use tee_coprocessor::config::Config;

fn example() -> Config {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/coprocessor.toml.example"
    );
    let mut config = Config::load(path).expect("the example must parse");
    config.proof_dir = std::env::temp_dir()
        .join("tee-hyperlane-example")
        .display()
        .to_string();
    config
}

#[test]
fn every_route_builds() {
    std::env::set_var("CELHOME", "/nonexistent");
    let config = example();
    assert_eq!(config.routes.len(), 8);
    for route in &config.routes {
        config
            .indexer(&route.from)
            .unwrap_or_else(|e| panic!("{}: {e:#}", route.name));
        config
            .destination(&route.to, &route.ism)
            .unwrap_or_else(|e| panic!("{}: {e:#}", route.name));
        assert!(!route.routers.is_empty(), "{} names no routers", route.name);
    }
}
