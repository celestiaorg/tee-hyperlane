# teeism

The one tool that deploys, rotates, inspects and tears down the bridge. How to run it, step by step: [deploy/DEPLOY.md](../deploy/DEPLOY.md).

## Reading the code

Start at `src/commands.rs`: every command in deploy order, and the file that implements it.

```
src/
  main.rs          entry point; prints errors
  commands.rs      the commands and their flags; calls one step each
  steps/           one file per command; a command with subcommands is a folder, one file each
  shared/          what the steps use; skip it on a first read
```

| shared/ | what it is |
|---|---|
| `record.rs` | `devnet/.state/deployment.json`: every id a step created |
| `settings.rs` | `devnet/.env`, and where everything lives |
| `chains.rs` | the fixed facts about every chain |
| `evm.rs` | sending, deploying and reading EVM contracts |
| `celestia_cli.rs` | the `celestia-appd` binary: transactions, queries |
| `intel.rs` | fetching Intel's collateral |
| `shell.rs` | running external programs |
| `print.rs` | terminal output |
| `incomplete.rs` | the "partly done" error (exit 3) |
| `node_config.rs` | editing a node's TOML config |
| `revert_names.rs` | naming Solidity errors in a revert |

Own Cargo workspace on purpose: the enclave images are built from `tee-hyperlane/Cargo.lock`, which this tool's dependencies must never move.

```sh
cargo install --locked --path cli      # from the repo root
cargo test --manifest-path cli/Cargo.toml
```
