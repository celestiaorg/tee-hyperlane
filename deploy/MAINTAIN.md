# MAINTAIN

Run on ark as `chef`. Every block starts with a `cd`.

## Monthly: Intel collateral

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/collateral-status.sh                                              # what expires when
for c in sepolia arbitrum base eden; do ./scripts/seed-evm-collateral.sh $c; done
```

- Skipped: Celestia → EVM routes stop after 30 days with `TCBR`.
- It prints the fix for anything missing. `DRY_RUN=1` checks without sending.

## Before each TCB recovery: the next evaluation number

Dates: **21 on about 10 Feb 2027, 22 on about 11 Aug 2027**. Run weeks before:

```sh
cd ~/tee-ism-nonzk/devnet
for c in sepolia arbitrum base eden; do ./scripts/76-tcb-eval.sh $c 21; done   # owner-signed: EVM_SIGNER
```

- Check the enclaves' host passes the new number, or ask Phala to patch it ([ADMIN](ADMIN.md#tcb-recovery)).
- Celestia needs nothing.

## Update: code only

Coprocessor, gas oracle, UI or scripts.

```sh
cd ~/tee-ism-nonzk && git pull
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh && write_config
sudo systemctl restart teeism-relayer teeism-gas-oracle
```

UI changes: also rebuild the UI ([DEPLOY step 11](DEPLOY.md#11-ui-and-gateway)).

## Update: enclave code

Changes to `crates/tee-node`, `crates/hyperlane-types` or `Cargo.lock`. Re-pins every ISM the changed CVM attests ([One chain in its own CVM](#one-chain-in-its-own-cvm)). In-flight transfers are kept ([No message is lost](#no-message-is-lost)).

**1. Pull, stop, back up**

```sh
cd ~/tee-ism-nonzk && git pull
sudo systemctl stop teeism-relayer
cp -a devnet/.state ~/teeism-state-$(date +%F)
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
```

Do not delete `devnet/.state/proofs/`.

**2. Images:** [DEPLOY step 4](DEPLOY.md#4-enclave-images): build, pin, commit and push.

**3. Enclaves:** [DEPLOY step 5](DEPLOY.md#5-enclaves) with the original flags: none for the shared CVM, `--<chain>` for a chain in its own.

**4. ISMs and routers**

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/75-dcap-verifiers.sh && ./scripts/80-evm-isms.sh && ./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh
```

- ISM owned by a multisig: re-pin it from the multisig first ([ADMIN](ADMIN.md#re-pin-the-enclave-identity)).
- Each router should print `repointing`. **If one prints `deploying the synthetic ... router`, press Ctrl-C** (a new router orphans minted tokens).

**5. Start**

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh && write_config
sudo cp ~/tee-ism-nonzk/deploy/server/teeism-relayer.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl start teeism-relayer
curl -s localhost:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'     # 8 routes
```

**6. UI:** set the new `VITE_*_ISM` values and rebuild ([DEPLOY step 11](DEPLOY.md#11-ui-and-gateway)).

**7. Test:** one transfer each way per route.

**8. Clean up**, only once step 7 works:

```sh
phala cvms delete <old app id>        # each replaced CVM, ids in ~/teeism-state-*/out/enclave-app-id-*
```

Then update the ids in `README.md`.

**Rollback** (before step 8):

```sh
sudo systemctl stop teeism-relayer
cd ~/tee-ism-nonzk/devnet && rm -rf .state && cp -a ~/teeism-state-<date> .state
./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh      # routers back to the old ISMs
cd ~/tee-ism-nonzk && git checkout <previous commit>
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor
sudo systemctl start teeism-relayer
```

## Replace an enclave, same image

```sh
phala cvms delete <old app id>
cd ~/tee-ism-nonzk/devnet
./scripts/30-enclave-up.sh                          # identity must match README.md
. scripts/lib.sh && write_config && sudo systemctl restart teeism-relayer
```

## One chain in its own CVM

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/30-enclave-up.sh --base                   # any of --celestia --ethereum --base --arbitrum --eden
./scripts/80-evm-isms.sh && ./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh
. scripts/lib.sh && write_config && sudo systemctl restart teeism-relayer
```

Costs one more CVM.

## No message is lost

- Replaced ISMs resume from the old ISM's last state. If it can't be read, the script stops.
- Celestia → EVM resumes only if its last state is under 14 days old.

## Waiting or stuck?

| route | normal wait |
|---|---|
| Celestia → any EVM | < 1 min |
| Eden → Celestia | 1-4 min |
| Sepolia → Celestia | ~15 min |
| Arbitrum → Celestia | < 1 min |
| Base → Celestia | < 1 min |

Base or Arbitrum waiting more than a few minutes: stuck, see below. `leaves=N` counts other people's messages too.

## Symptoms

| you see | fix |
|---|---|
| `TCBR`, `PCKCRLH` or another collateral code | run the monthly job |
| `TCBR` right after the monthly job | deploy a new versioned FMSPC DAO per chain |
| `WrongEnclave`, `IdentityChanged` | check the urls from `write_config` |
| `unknown command "teeism"` | the unit's PATH must start with `.state/bin` |
| `eth_getProof` fails at an old height | use an archive endpoint ([DEPLOY F](DEPLOY.md#f-endpoints)) |
| Sepolia: `eth_getProof` … `operation timed out` against `127.0.0.1:8545` | point `SEPOLIA_RPC` at an archive for one pass |
| a Celestia-origin ISM stuck, trusted commit missing | replace the ISM |
| `no enrolled router found` | run `90-evm-warp.sh` |
| random `nonce too low` | another relayer uses the same EVM key |
| every route backing off | an endpoint is rate-limited |
| UI shows `Failed to fetch` | rebuild the UI on the host |
| `discarding a batch` | nothing |
| `carrying sync committee updates` every tick | nothing |
| Sepolia: `GLOAS_FORK_VERSION missing` | point `SEPOLIA_BEACON` at Lodestar ([DEPLOY 6b](DEPLOY.md#6b-sepolia-node)) |
| Sepolia: `no finalized checkpoint … rebuilds this ISM's light-client store` | if `.state/proofs/chains/sepolia/store-*.json` is gone, re-anchor the Sepolia ISM |
| Sepolia deliveries: `ClockBehindAttest` | `send_rpc` (`SEPOLIA_SEND_RPC`) must be at the head |
| Sepolia: `finality update rejected: invalid sync committee period` | nothing since `7b51626`; before that, clears within 20 minutes |
| `error decoding response body` once | nothing, unless it repeats |
| Eden: `no celestia header in the last 400 rebuilds this ISM's store` | restore `.state/proofs/chains/eden/` from a backup |
| Eden: `no celestia block … carries an eden header at a height we hold a proof for` | wait a few minutes; if it persists, check mocha-light is synced |
| Base: `nothing from the base sequencer on p2p in Ns` | `RUST_LOG=debug` shows `base gossip peers=… received=…`; check outbound TCP and UDP 9222 |
| Arbitrum: `nothing from the arbitrum sequencer feed in Ns` | check `ARBITRUM_FEED` |
| `signed by 0x…, not the pinned sequencer 0x…` | set the new key in `tee-node/src/chains/l2/<chain>.rs` and roll out new enclave code |
| `proving the tree at signed block N` once | nothing, unless it repeats |
| `reading the tree at the trusted height` | check `ALCHEMY_KEY` and its quota |

Decode a four-letter code: `cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)`.

## Slack

Set `SLACK_BOT_TOKEN` and `SLACK_CHANNEL` (and `EXPLORER_URL` for links) in `devnet/.env`, then restart the relayer. **No status post for over 30 minutes means the relayer is down.**

## Check what runs

```sh
cd ~/tee-ism-nonzk
TARGET=all deploy/verify-digest.sh <app id>                                 # the shared CVM runs this repo
TARGET=all deploy/verify-digest.sh <app id> --ism <ism> --rpc <rpc>         # and the ISM accepts it
TARGET=base deploy/verify-digest.sh <app id>                                # a chain in its own CVM
cast call <router> 'interchainSecurityModule()(address)' --rpc-url <rpc>     # which ISM a router uses
```

`origin_domain` on the EVM ISMs reads `1297040200`, not `1297040299`. Expected.

## Secrets

Run `deploy/check-secrets.sh` before committing. If a key reaches GitHub, rotate it.

## Intel's side breaks

A re-pin doesn't fix it. See [ADMIN.md](ADMIN.md#when-something-breaks-live).
