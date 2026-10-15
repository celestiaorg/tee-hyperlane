# MAINTAIN

On ark as `chef`, in `~/tee-ism-nonzk`. After `git pull`, rebuild the CLI: `cargo install --locked --path cli`.

## Monthly: Intel collateral

```sh
teeism pccs status          # what expires when; exits 1 if anything is within 14 days
teeism pccs seed            # republish all of it; --dry-run checks and fetches without sending
```

- Skipped: Celestia -> EVM routes stop 30 days after the last seed, with `TCBR` or `PCKCRLH`.
- Celestia-destination routes need nothing: the relayer fetches fresh collateral with each attestation.
- Where it comes from and what it is checked against: [DEPLOY B](DEPLOY.md#b-intel-root-ca-and-collateral).

## Before each TCB recovery: the next evaluation number

Intel's standard evaluation number moves twelve months after release: **21 on about 10 Feb 2027, 22 on about 11 Aug 2027**. `teeism pccs deploy` covers every number Intel already publishes; for a newer one, weeks before:

```sh
teeism pccs tcb-eval 23      # its QE identity and TCB info contracts on every chain, registered, seeded
```

- Check the enclaves' host passes the new number, or ask Phala to patch it ([ADMIN](ADMIN.md#tcb-recovery)).
- Celestia needs nothing.

## Heartbeat

Every route attests at least hourly, with no messages if nothing was dispatched, so its ISM stays near its origin's head and the next proof starts from recent state.

- The relayer sends one once an ISM's trusted state is an hour old (`HEARTBEAT`, `tee-coprocessor/src/route.rs`).
- The enclave, `TeeDcapIsm` and `x/teeism` all refuse an empty batch that moves the origin clock less than 30 minutes, so nobody can use the public enclave to advance an ISM more often.
- An empty batch cannot skip a message: the enclave replays the batch from the tree under the ISM's own root, and zero ids reproduce the head's tree only if nothing was dispatched.
- Cost: one attestation per quiet route per hour. On Sepolia at 1.1 gwei that is ~0.006 ETH, about 0.14 ETH a day if the route is idle all day.
- Check: `teeism status` lists every route's state age; over two hours is reported.

## Update: code only

Relayer, gas oracle, UI or CLI.

```sh
git pull && cargo install --locked --path cli
teeism build
teeism services restart
teeism ui --public-url http://178.199.12.26:3000      # UI changes only
```

## Update: enclave code

Changes to `crates/tee-node`, `crates/hyperlane-types`, `crates/helios-consensus-core` or `tee-hyperlane/Cargo.lock` move the images and re-pin every ISM the changed CVM attests. In-flight transfers are kept.

```sh
git pull && cargo install --locked --path cli && teeism build
cp -a devnet/.state ~/teeism-state-$(date +%F)            # backup
teeism images                                              # then commit and push the pins (DEPLOY step 6)
teeism enclaves up && teeism enclaves verify
teeism isms evm && teeism isms celestia                    # re-pin in place; check every line says re-pinning
teeism services restart
teeism smoke
teeism enclaves delete --retired                           # only once smoke passed
```

- An ISM owned by a multisig: re-pin it from the multisig first ([ADMIN](ADMIN.md#re-pin-the-enclave-identity)); `teeism` re-pins only what its keys own and would otherwise replace it.
- A changed `TeeDcapIsm` `VERSION` replaces each EVM ISM from its last state and re-points the routers (`teeism warp evm`). It never redeploys a router: a new router orphans what the old one minted.
- Then update the ids in `README.md`.

## Update: celestia-app

A new commit on `jonas/tee-ism`:

```sh
git -C ../celestia-app-local pull
teeism build
teeism celestia up --new-image     # restarts the running chain on the new binary
teeism services restart
```

Only for changes that keep the state machine compatible with the existing chain. Anything else needs a coordinated upgrade or a fresh chain ([DEPLOY](DEPLOY.md)).

## Re-pin to a new mr_td (new dstack OS image)

[DEPLOY A](DEPLOY.md#a-re-pin-with-the-admin-key-a-new-mr_td).

## One chain in its own CVM

```sh
teeism enclaves up --chain base     # any of celestia ethereum base arbitrum eden
teeism isms evm && teeism isms celestia && teeism warp evm
teeism services restart
```

Costs one more CVM.

## Tear everything down

```sh
teeism teardown            # lists what it would delete
teeism teardown --yes      # services, containers, CVMs, devnet/.state (devnet/.env is kept)
```

Contracts on the EVM chains stay where they are, unused. A CVM named `teeism-*` that the record does not know is reported, never deleted: `teeism enclaves delete --app-id <id>`.

## No message is lost

- A re-pin keeps each ISM's state; a replaced ISM resumes from the old one's last state, and stops rather than fall back to the head if it cannot read it.
- Celestia -> EVM resumes only if its last state is under 14 days old.

## Waiting or stuck?

| route | normal wait |
|---|---|
| Celestia -> any EVM | < 1 min |
| Eden -> Celestia | 1-4 min |
| Sepolia -> Celestia | ~15 min |
| Arbitrum -> Celestia | < 1 min |
| Base -> Celestia | < 1 min |

Base or Arbitrum waiting more than a few minutes: stuck, see below. `leaves=N` counts other people's messages too.

## Symptoms

| you see | fix |
|---|---|
| `TCBR`, `PCKCRLH` or another collateral code | `teeism pccs seed` |
| `TCBR` right after a seed | Intel moved the evaluation number: `teeism pccs tcb-eval <n>` |
| `WrongEnclave`, `IdentityChanged` | `teeism services restart` (rewrites the enclave URLs); `teeism enclaves verify --ism-chain <chain>` |
| `EmptyBatchTooSoon`, `an empty batch must advance the origin clock` | nothing, unless it repeats: someone else advanced the ISM |
| `unknown command "teeism"` from the relayer | the unit's PATH must start with `.state/bin` |
| `eth_getProof` fails at an old height | use an archive endpoint ([DEPLOY G](DEPLOY.md#g-endpoints)) |
| Sepolia: `eth_getProof` ... `operation timed out` against `127.0.0.1:8545` | point `SEPOLIA_RPC` at an archive for one pass |
| a Celestia-origin ISM stuck, trusted commit missing | replace the ISM |
| `no enrolled router found` | `teeism warp evm` |
| random `nonce too low` | another relayer uses the same EVM key |
| every route backing off | an endpoint is rate-limited |
| UI shows `Failed to fetch` | `teeism ui` on the host |
| `discarding a batch` | nothing |
| `carrying sync committee updates` every tick | nothing |
| Sepolia: `GLOAS_FORK_VERSION missing` | point `SEPOLIA_BEACON` at Lodestar ([DEPLOY F](DEPLOY.md#f-sepolia-node)) |
| Sepolia: `no finalized checkpoint ... rebuilds this ISM's light-client store` | if `.state/proofs/chains/sepolia/store-*.json` is gone, re-anchor the Sepolia ISM |
| Sepolia deliveries: `ClockBehindAttest` | `SEPOLIA_SEND_RPC` must be at the head |
| `error decoding response body` once | nothing, unless it repeats |
| Eden: `no celestia header in the last 400 rebuilds this ISM's store` | restore `.state/proofs/chains/eden/` from a backup |
| Eden: `no celestia block ... carries an eden header at a height we hold a proof for` | wait a few minutes; if it persists, check mocha-light is synced |
| Base: `nothing from the base sequencer on p2p in Ns` | `RUST_LOG=debug` shows `base gossip peers=... received=...`; check outbound TCP and UDP 9222 |
| Arbitrum: `nothing from the arbitrum sequencer feed in Ns` | check `ARBITRUM_FEED` |
| `signed by 0x..., not the pinned sequencer 0x...` | set the new key in `tee-node/src/chains/l2/<chain>.rs` and roll out new enclave code |
| `proving the tree at signed block N` once | nothing, unless it repeats |
| `reading the tree at the trusted height` | check `ALCHEMY_KEY` and its quota |

Decode a four-letter verifier code: `cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR) --rpc-url <rpc>`.

## Slack

Set `SLACK_BOT_TOKEN` and `SLACK_CHANNEL` (and `EXPLORER_URL` for links) in `devnet/.env`, then `teeism services restart`. **No status post for over 30 minutes means the relayer is down.**

## Check what runs

```sh
teeism status                                     # everything deployed, and every route's state age
teeism enclaves verify                            # the shared CVM runs this repo's compose
teeism enclaves verify --ism-chain sepolia        # and that chain's ISM accepts it
teeism enclaves verify --target base              # a chain in its own CVM
teeism pccs check-quote --enclave celestia        # each EVM verifier accepts a fresh quote
```

`origin_domain` on the EVM ISMs reads `1297040200`, not `1297040299`. Expected.

## Secrets

`teeism check-secrets` runs before every commit (`teeism check-secrets --install-hook`). If a key reaches GitHub, rotate it.

## Intel's side breaks

A re-pin doesn't fix it. See [ADMIN.md](ADMIN.md#when-something-breaks-live).
