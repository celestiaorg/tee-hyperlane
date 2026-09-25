# MAINTAIN

Run on ark as `chef`. Every block starts with a `cd`, so paste it from anywhere.

## Monthly: Intel collateral

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/collateral-status.sh                                              # what expires when
for c in sepolia arbitrum base eden; do ./scripts/seed-evm-collateral.sh $c; done
```

Skip it and the Celestia → EVM routes stop after 30 days with `TCBR`. Routes into Celestia
don't need it.

## Update: code only

For changes to the coprocessor, gas oracle, UI or scripts.

```sh
cd ~/tee-ism-nonzk && git pull
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh && write_config
sudo systemctl restart teeism-relayer teeism-gas-oracle
```

For UI changes, also rebuild the UI ([DEPLOY step 11](DEPLOY.md#11-ui-and-gateway)).

## Update: enclave code

For changes to `crates/tee-node`, `hyperlane-types`, `tee-attestation` or `Cargo.lock`. Each
changed enclave needs new ISMs. The scripts only redo what changed.

**0.** Wait for in-flight transfers to land (Base takes 5 days), or
[keep them](#keep-in-flight-messages).

**1. Pull, stop, back up**

```sh
cd ~/tee-ism-nonzk && git pull
sudo systemctl stop teeism-relayer
cp -a devnet/.state ~/teeism-state-$(date +%F)
mv devnet/.state/proofs ~/teeism-proofs-$(date +%F)
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
```

**2. Images:** [DEPLOY step 4](DEPLOY.md#4-enclave-images).

**3. Enclaves:** [DEPLOY step 5](DEPLOY.md#5-enclaves). The old CVMs keep running.

**4. ISMs and routers**

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/80-evm-isms.sh && ./scripts/85-celestia-isms.sh && ./scripts/90-evm-warp.sh
```

Each router should print `repointing`. **If one prints `deploying the collateral USDC router`,
press Ctrl-C.** A new collateral router strands the USDC held in the old one.

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
phala cvms delete <old app id>        # ×3, ids in ~/teeism-state-*/out/enclave-app-id-*
cd ~/tee-ism-nonzk && git commit deploy/docker-compose.*.yml -m "Pin new enclave images" && git push
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

## Keep in-flight messages

Start each new ISM from the old ISM's state, with only the enclave identity (its last 32
bytes) replaced. Run this before step 4:

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh; OLD=~/teeism-state-<date>/out
# EVM side, e.g. Sepolia
old=$(cast call "$(cat $OLD/ism-sepolia)" 'state()(bytes)' --rpc-url <sepolia rpc>)
ISM_GENESIS="${old:0:170}$(load identity-digest-celestia | sed s/^0x//)" \
  CHAINS="sepolia:11155111:0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766" ./scripts/80-evm-isms.sh
# Celestia side, e.g. Base
old=0x$(.state/bin/celestia-appd q teeism ism "$(cat $OLD/ism-celestia-base)" -o json \
  | python3 -c 'import base64,json,sys;print(base64.b64decode(json.load(sys.stdin)["ism"]["state"]).hex())')
ISM_GENESIS_BASE="${old:0:170}$(load identity-digest-ethereum | sed s/^0x//)" ./scripts/85-celestia-isms.sh
```

Then run step 4 as usual; it skips the ISMs you just made. For Celestia → EVM routes, the old checkpoint must be
under 14 days old.

## Waiting or stuck?

| route | normal wait |
|---|---|
| Celestia → any EVM | < 1 min |
| Eden → Celestia | 1-2 min |
| Sepolia → Celestia | ~15 min |
| Arbitrum → Celestia | ~1h 40m |
| Base → Celestia | 5 days |

To tell whether a Base transfer is still waiting, compare the anchor's L2 block with your
dispatch block. If the anchor is below it, the transfer is waiting:

```sh
cast call 0x2fF5cC82dBf333Ea30D8ee462178ab1707315355 "getAnchorRoot()(bytes32,uint256)" \
  --rpc-url https://rpc.sepolia.ethpandaops.io
```

`leaves=N` in the log counts other people's messages too.

## Symptoms

| you see | fix |
|---|---|
| `TCBR`, `PCKCRLH` or another collateral code | run the monthly job |
| `TCBR` right after the monthly job | Intel moved on; deploy a new versioned FMSPC DAO per chain |
| `WrongEnclave`, `IdentityChanged` | a route points at the wrong enclave; check the urls from `write_config` |
| `unknown command "teeism"` | the unit's PATH must start with `.state/bin` |
| `eth_getProof` fails at an old height | use an archive endpoint ([DEPLOY D](DEPLOY.md#d-endpoints)) |
| a Celestia-origin ISM stuck, trusted commit missing | the chain was pruned; replace the ISM |
| `no enrolled router found` | run `90-evm-warp.sh` |
| random `nonce too low` | another relayer uses the same EVM key |
| every route backing off | an endpoint is rate-limited |
| UI shows `Failed to fetch` | the UI was built with the wrong `.env.local`; rebuild on the host |
| `discarding a batch` | nothing; it rebuilds by itself |

To decode a four-letter code: `cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)`.

## Check what runs

```sh
cd ~/tee-ism-nonzk
FAMILY=celestia deploy/verify-digest.sh <app id>                            # the enclave runs this repo
FAMILY=celestia deploy/verify-digest.sh <app id> --ism <ism> --rpc <rpc>    # and the ISM accepts it
cast call <router> 'interchainSecurityModule()(address)' --rpc-url <rpc>     # which ISM a router uses
```

`origin_domain` on the EVM ISMs reads `1297040200` (mocha's domain), not `1297040299`. That is
expected.

## Secrets

`devnet/.env`, `devnet/.state/` and `keys/` are gitignored. Run `deploy/check-secrets.sh`
before committing. If a key reaches GitHub, rotate it.
