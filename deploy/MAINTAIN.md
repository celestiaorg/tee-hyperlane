# MAINTAIN

Keeping a deployed bridge running. On ark, as `chef`, from `~/tee-ism-nonzk/devnet`.

---

## Monthly: republish Intel's collateral

```sh
./scripts/collateral-status.sh                     # read-only: what each chain holds, and when it lapses
for c in sepolia arbitrum base eden; do ./scripts/seed-evm-collateral.sh $c; done
```

Each artifact reports `published`, `already current` or `FAILED` with a reason. Nothing it
uploads is trusted: the DAO checks Intel's signature on upload.

TCB info, QE identity and the PCK CRLs are valid for 30 days on each EVM chain. When they
lapse, the EVM ISMs reject quotes with `TCBR` or `PCKCRLH` and the four Celestia-to-EVM routes
stop. Routes into Celestia are unaffected, because they carry fresh collateral in every
transaction.

If `TCBR` persists right after a successful republish, Intel has moved the TDX TCB evaluation
number past 20. The versioned DAOs pin 20, so each chain needs a new versioned FMSPC DAO for the
new number, with the router pointed at it.

## Rolling out new code

| change | what to do |
|---|---|
| coprocessor, gas oracle, UI, scripts | rsync, rebuild, `write_config`, restart. No ISM changes |
| a new asset | [DEPLOY appendix A](DEPLOY.md#a-adding-an-asset) |
| enclave code (`crates/tee-node`, `hyperlane-types`, `tee-attestation`, `Cargo.lock`) | the sequence below: new ISMs for each family whose digest moved |
| a new dstack OS or Phala KMS | the sequence below, for all three families |

An enclave identity is immutable in both ISMs, so a new image always means new ISMs, with the
routers re-pointed at them. The scripts compare what is recorded with what is pinned and redo
only what differs, so the sequence below handles one family or all three.

**0. Drain.** A new ISM starts from its origin's current head, so a transfer dispatched and
not yet delivered never arrives. Wait until every transfer in flight has arrived (the UI's
history shows them); Base can hold one for five days. To carry one across instead, see
[Keeping in-flight messages](#keeping-in-flight-messages).

**1. Code, stop, back up.** From the machine with the repo:

```sh
rsync -a --exclude target --exclude .git --exclude keys --exclude devnet/.state \
  --exclude 'result-*' ./ chef@<host>:~/tee-ism-nonzk/
```

On the host:

```sh
sudo systemctl stop teeism-relayer
cp -a .state ~/teeism-state-$(date +%F)          # the rollback
mv .state/proofs ~/teeism-proofs-$(date +%F)     # batch history for the old ISMs
```

On a host from before the relayer served the API, also remove the old units:

```sh
sudo systemctl disable --now teeism-api bridge-ui
sudo rm /etc/systemd/system/{teeism-api,bridge-ui}.service && sudo systemctl daemon-reload
```

**2. Images:** [DEPLOY step 4](DEPLOY.md#4-enclave-images).

**3. Enclaves:** [DEPLOY step 5](DEPLOY.md#5-enclaves). This deploys a CVM only for families
whose compose changed. The old CVMs keep running.

**4. ISMs and routers.** Each script replaces only ISMs that pin an old identity, and
`90-evm-warp.sh` re-points the routers:

```sh
./scripts/80-evm-isms.sh && ./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh
```

Each router should print `repointing <router> at <ism>`. If one prints `deploying the collateral
USDC router`, stop: a new collateral router abandons the old one's escrow.

**5. Relayer:**

```sh
. scripts/lib.sh && write_config
sudo cp ../deploy/server/teeism-relayer.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl start teeism-relayer
curl -s localhost:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'
```

**6. Test.** Send one transfer each way on every route. Until they land nothing is lost, since
the old CVMs and ISMs still exist.

To roll back:
1. restore `~/teeism-state-*` as `.state`
2. with the new scripts still in place, run `./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh`,
   which re-point the routing ISM and routers at whatever `.state/out/` names
3. restore the old code, rebuild, and restart the relayer

**7. Retire and record:**
- delete the old CVMs with `phala cvms delete`
- copy the compose files back into the repo
- refresh `coprocessor.toml.example` (the command is at the top of that file)
- update the identities and ISM ids in `README.md`
- run `deploy/check-secrets.sh`, then commit

## Replacing an enclave, same image

An enclave holds no state, so a CVM running the same image is a drop-in swap. Deleting the old
CVM makes `30-enclave-up.sh` deploy a new one:

```sh
phala cvms delete <old app id>
./scripts/30-enclave-up.sh      # new CVM, same identity; check it matches README.md
. scripts/lib.sh && write_config && sudo systemctl restart teeism-relayer
```

Routes resume from the heights their ISMs hold. If the identity differs, the image or OS
changed: use the sequence above.

## Keeping in-flight messages

Anchor the replacement ISM at the old one's checkpoint instead of the current head. Take the
old state and splice the new identity into its last 32 bytes:

```sh
. scripts/lib.sh
old="$(cast call "$(cat ~/teeism-state-*/out/ism-sepolia)" 'state()(bytes)' --rpc-url <rpc>)"
ISM_GENESIS="${old:0:170}$(load identity-digest-celestia | sed 's/^0x//')" \
  CHAINS="sepolia:11155111:0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766" ./scripts/80-evm-isms.sh
```

For Celestia-side ISMs, set `ISM_GENESIS_<ORIGIN>` (for example `ISM_GENESIS_BASE`) before
running `85-celestia-isms.sh`. The route then replays everything since the old checkpoint.

## Is it waiting or stuck?

Most "stuck" routes are waiting. Check this first:

| route | normal wait | how to confirm |
|---|---|---|
| Celestia to any EVM chain | under a minute | |
| Sepolia to Celestia | ~15 min (finality), more if an epoch boundary slot was empty | |
| Eden to Celestia | 1-2 min (Eden's DA posting) | |
| Arbitrum to Celestia | ~1h40m | |
| Base to Celestia | 5 days + ~3 min (dispute game) | anchor below the dispatch block means waiting: `cast call 0x2fF5cC82dBf333Ea30D8ee462178ab1707315355 "getAnchorRoot()(bytes32,uint256)" --rpc-url https://rpc.sepolia.ethpandaops.io` |

- `leaves=N` in the log is the batch size. The EVM mailboxes are Hyperlane's shared ones, so
  most leaves are other people's.
- An ISM's `timestamp` is hours old on an idle route. It moves only when there is something to
  deliver, plus a heartbeat every 12 hours.

## Symptoms

| symptom | cause | fix |
|---|---|---|
| `TCBR`, `PCKCRLH`, `QEIDCH` or another collateral code on Celestia to EVM routes | collateral expired | the monthly job |
| `TCBR` right after republishing | Intel moved the evaluation number | a new versioned DAO per chain |
| a quote revert with no data at all | quote bytes damaged in transit, not a wrong enclave | check the relayer's submission |
| `WrongEnclave` or `IdentityChanged` | the enclave's identity is not the one the ISM pins | [roll out](#rolling-out-new-code), or point the route back at the right CVM |
| `unknown command "teeism"` on Celestia-origin routes | a different `celestia-appd` on PATH | the unit must put `.state/bin` first |
| `eth_getProof` fails at an old height | endpoint pruned | set an archive endpoint ([DEPLOY appendix D](DEPLOY.md#d-endpoints)) |
| a Celestia-origin ISM never moves and the light client cannot find its trusted commit | chain was pruned | the ISM must be replaced; keep `pruning = "nothing"` |
| `discarding a batch the ISM has moved past` | something advanced the ISM first | nothing; the next pass rebuilds from the ISM |
| `no enrolled router found for origin` | ISM created without its routers | run `90-evm-warp.sh` for that chain |
| `nonce too low`, `replacement transaction underpriced` at random | two relayers share the EVM key | one key per deployment |
| every route backing off at once | a shared endpoint is rate-limited | move logs off metered keys |
| UI says `Failed to fetch` for everyone | bundle built with a local `.env.local` | rebuild on the host ([DEPLOY step 11](DEPLOY.md#11-ui-and-gateway)) |

To expand an Automata code: `cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)`.

A route is paused after repeated failures, backing off up to 30 minutes. Its blocker shows in
`/api/status` and in the log as `route failed`.

## Verifying what runs

```sh
FAMILY=celestia ../deploy/verify-digest.sh <app id>                        # compose -> quote
FAMILY=celestia ../deploy/verify-digest.sh <app id> --ism <ism> --rpc <rpc> # and the ISM accepts it
FAMILY=celestia ../deploy/verify-digest.sh <app id> --rebuild              # and the image is this source, ~35 min
```

It reproduces `compose_hash`, finds it inside the signed quote, and diffs the measured compose
against this checkout. `os_image_hash` must be `bd369a8c…`; `de9c74f0…` is a dev image with
shell access.

To check the wiring behind it, start from the router, not from a config:

```sh
cast call <router> 'interchainSecurityModule()(address)' --rpc-url <rpc>    # the ISM it uses
cast call <ism> 'enclaveMeasurements()(bytes32)' --rpc-url <rpc>            # what that ISM pins
curl -s localhost:1317/hyperlane/v1/tokens | jq -r '.tokens[] | "\(.id) \(.ism_id)"'
curl -s localhost:1317/hyperlane/v1/isms/<routing ism> | jq -r '.ism.routes[]'
```

Every EVM ISM pins the `celestia` family; each Celestia-side ISM pins the family of its origin.
`TeeDcapIsm` source is verified on Blockscout (`<chain>.blockscout.com/address/<ism>`) on all
four EVM chains, not on Etherscan.

Two readings that look wrong and are not:
- **`origin_domain` on the EVM ISMs is `1297040200`, mocha's domain, not this chain's.** It is
  a constant in `celestia/mod.rs`. What binds an ISM to this particular chain is the
  light-client commitment in its state.
- **An idle route's ISM state is hours old.** That is normal; see above.

## Secrets

`keys/`, `devnet/.env` and `devnet/.state/` hold real credentials and are gitignored. Run
`deploy/check-secrets.sh` before every commit (the pre-commit hook from DEPLOY step 1 does
this). A key that reaches the public repo must be rotated; deleting it leaves it in history.
