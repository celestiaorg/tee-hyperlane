# teeism

The one tool that deploys, rotates, inspects and tears down the bridge. How to run it, step by step: [deploy/DEPLOY.md](../deploy/DEPLOY.md).

## Reading the code

Start at `src/commands.rs`: every command in deploy order, and the file that implements it.

```
src/
  main.rs            entry point; prints errors
  commands.rs        the commands and their flags; calls one step each
  steps/
    celestia/        the bridge's chain: up.rs (genesis, node), hyperlane.rs (core, warp),
                     paymaster.rs, mocha.rs (Eden's light node), appd.rs (the celestia-appd wrapper)
    enclaves/        images.rs, up.rs (CVMs, identities), verify.rs, quote.rs (reading a quote)
    pccs/            deploy.rs, seed.rs (and fetching from Intel), status.rs
    isms.rs          both kinds of ISM
    warp.rs  eden.rs  services.rs  build.rs  preflight.rs  ui.rs  smoke.rs  status.rs
    teardown.rs  check_secrets.rs
    upstream.rs      upgrades and releases upstream that could break us (deploy/upstream.toml)
  shared/            what at least three steps use; skip it on a first read
    record.rs        devnet/.state/deployment.json: every id a step created
    settings.rs      devnet/.env, and where everything lives
    chains.rs        the fixed facts about every chain
    evm.rs           sending, deploying and reading EVM contracts; naming reverts
    shell.rs         running external programs
    print.rs         terminal output; the "partly done" error (exit 3)
```

Its own Cargo workspace on purpose: the enclave images are built from `tee-hyperlane/Cargo.lock`, which this tool's dependencies must never move.

```sh
cargo install --locked --path cli      # from the repo root
cargo test --manifest-path cli/Cargo.toml
```
