# DEPLOY

One Linux host, as `chef`. Every block starts with a `cd`. Every script is safe to re-run.

- Exit 3: partial (e.g. Eden before mocha syncs). Fix, re-run.
- Ids: `devnet/.state/out/`; `cd ~/tee-ism-nonzk && make status`.
- Laptop: `make init && make start` (repo root). No Nix: `FORCE_UNPINNED=1` (skips step 5's pin checks).
- **Never run `make stop` on ark**: it deletes the enclaves and `.state/`.

## 0. What you need

- docker, go 1.26+, rust, foundry, node, python3, jq, curl, nix
- phala CLI, logged in; Phala balance ($4.38/day)
- Alchemy key (free)
- dedicated EVM key (relayer): ETH on Sepolia, Arbitrum Sepolia, Base Sepolia; TIA on Eden. With `EVM_SIGNER=ledger`, fund the Ledger too.
- Inbound: port 3000 only. Outbound: Base p2p (TCP, UDP 9222).

## 1. Code and secrets

```sh
git clone https://github.com/jonas089/tee-hyperlane.git ~/tee-ism-nonzk
git clone -b jonas/tee-ism https://github.com/celestiaorg/celestia-app.git ~/celestia-app-local
cd ~/tee-ism-nonzk
ln -s ../../deploy/check-secrets.sh .git/hooks/pre-commit
mkdir -p devnet/.state && chmod 700 devnet/.state
cp devnet/.env.example devnet/.env && chmod 600 devnet/.env
$EDITOR devnet/.env      # set EVM_PRIVATE_KEY (with 0x) and ALCHEMY_KEY
```

celestia-app: always `jonas/tee-ism`, never `main`.

### Signing EVM transactions on a Ledger

1. `devnet/.env`: `EVM_SIGNER=ledger` (`EVM_LEDGER_PATH` for a non-first account). Keep `EVM_PRIVATE_KEY` (relayer).
2. Ledger unlocked, Ethereum app open, blind signing on.
3. Hand-signed commands take `$SIGN`:

```sh
SIGN="--ledger --from $(cast wallet address --ledger)"   # or: SIGN="--private-key $EVM_PRIVATE_KEY"
```

- `forge script`: `--ledger --sender <address>`. Automata (appendix D): `LEDGER=true OWNER=<ledger address>` instead of `PRIVATE_KEY`.
- Walkthrough: [Appendix E](#e-celestia-and-one-evm-chain-signed-on-a-ledger).

## 2. Build

```sh
cd ~/celestia-app-local
for b in cmd/celestia-appd x/teeism/cmd/teeism-collateral x/teeism/cmd/teeism-identity; do
  go build -o ~/tee-ism-nonzk/devnet/.state/bin/$(basename $b) ./$b
done
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
```

## 3. Chain

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/10-celestia-up.sh
./scripts/20-celestia-hyperlane.sh
grep -E '^pruning|^min-retain-blocks' .state/celestia/config/app.toml   # "nothing" and 0
```

## 4. Enclave images

```sh
cd ~/tee-ism-nonzk/devnet
docker login ghcr.io -u jonas089          # once; password: a GitHub token with write:packages
./scripts/25-images.sh
cd ~/tee-ism-nonzk && git add deploy/images.lock deploy/docker-compose.*.yml
git commit -m "Pin enclave images" && git push
```

- Images: `celestia`, `ethereum`, `base`, `arbitrum`, `eden`.
- Step 5 needs these pins pushed. No GitHub token on the server: commit the files elsewhere.

## 5. Enclaves

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/30-enclave-up.sh
TARGET=all ~/tee-ism-nonzk/deploy/verify-digest.sh "$(cat .state/out/enclave-app-id-all)"
```

- Default: one CVM, ports 8080 to 8084.
- Own CVM: `./scripts/30-enclave-up.sh --base` (any of `--celestia --ethereum --base --arbitrum --eden`). Verify: `TARGET=base verify-digest.sh <its app id>`.
- Every check must say `ok`.
- **Never use `phala cvms upgrade`: it changes the enclave identity.**

## 6. Intel collateral on the EVM chains

```sh
cd ~/tee-ism-nonzk/devnet
scp -P 50 chef@<old host>:tee-ism-nonzk/devnet/.state/out/pccs-*.json .state/out/
for c in sepolia arbitrum base eden; do ./scripts/seed-evm-collateral.sh $c; done
```

- Before ~10 Feb 2027: `./scripts/76-tcb-eval.sh <chain> 21`.
- No Automata contracts yet: [appendix D](#d-automata-on-a-new-evm-chain).

## 6b. Sepolia node

| | client | default (override) |
|---|---|---|
| execution | reth 2.7.0+ | `127.0.0.1:8545` (`SEPOLIA_RPC`) |
| beacon | Lodestar v1.49.0+ | `127.0.0.1:9596` (`SEPOLIA_BEACON`) |

- reth: ~1 day of history and `--rpc.eth-proof-window` (ark: 120k blocks).
- Not lighthouse (404s after Glamsterdam). Lodestar uses the same JWT secret.

```sh
curl -s localhost:9596/eth/v1/beacon/light_client/finality_update | jq -r .version   # gloas
curl -s localhost:9596/eth/v1/config/spec | jq -r .data.GLOAS_FORK_EPOCH            # 353024
```

## 7. Mocha light node (for Eden)

```sh
cd ~/tee-ism-nonzk/devnet
IMG=ghcr.io/celestiaorg/celestia-node:v0.34.2-mocha
D=$PWD/.state/mocha-light && mkdir -p "$D"
docker run --rm -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG celestia light init --p2p.network mocha
$EDITOR $D/config.toml      # [Share.LightAvailability] SampleAmount = 16; Header.Syncer.PruningWindow = "800h0m0s"
docker run -d --name mocha-light --restart unless-stopped -p 127.0.0.1:26658:26658 \
  -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG \
  celestia light start --p2p.network mocha --rpc.addr 0.0.0.0 --rpc.port 26658 --rpc.skip-auth
```

Syncs in ~30 min; re-run step 8 after for Eden.

## 8. Tokens, ISMs, warp routes

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/50-warp-celestia.sh
./scripts/75-dcap-verifiers.sh   # a v5 quote verifier on each EVM chain, next to v4
./scripts/80-evm-isms.sh
./scripts/85-celestia-isms.sh
./scripts/90-evm-warp.sh
. scripts/lib.sh; for k in ism-{sepolia,arbitrum,base,eden} ism-celestia-{sepolia,arbitrum,base,eden} routing-ism-id; do
  printf '%-24s %s\n' $k "$(load $k 2>/dev/null)"; done
```

- Every line must print an id.
- teeUSD supply (1B) goes to the `user` key (`TEEUSD_HOLDER` to change).
- **If `90-evm-warp.sh` prints `deploying the synthetic ... router` on a chain that already has one, press Ctrl-C**: it orphans existing tokens.

## 9. Paymaster and gas oracle

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh
A=.state/bin/celestia-appd; H="--home .state/celestia --keyring-backend test"
TX="$H --chain-id teeism-local --node http://localhost:26657 --fees 200000utia --gas 400000 -y"
$A tx hyperlane hooks igp create utia --from relayer $TX
IGP=<igp id from the output>
echo "$CELESTIA_MNEMONIC" | $A keys add bridge --recover --account 3 $H --output json
BRIDGE=$($A keys show bridge -a $H)
$A tx bank send relayer $BRIDGE 100000000utia $TX
$A tx hyperlane hooks igp set-owner $IGP --new-owner $BRIDGE --from relayer $TX
$A tx hyperlane mailbox set "$(load mailbox-id)" --required-hook "$(load merkle-hook-id)" --default-hook $IGP --from relayer $TX

cp ~/tee-ism-nonzk/deploy/gas-oracle.toml.example .state/gas-oracle.toml
sed -i "s|^igp_id = .*|igp_id = \"$IGP\"|" .state/gas-oracle.toml
printf '%s' "$EVM_PRIVATE_KEY" > .state/evm-key && chmod 600 .state/evm-key
~/tee-ism-nonzk/tee-hyperlane/target/release/gas-oracle --config .state/gas-oracle.toml --once
```

**EVM side, once per chain** (done on Sepolia, Arbitrum, Base). Else: `no gas oracle for domain`.

```sh
cast send <igp> "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(1297040299,(<oracle>,150000))]" --rpc-url <rpc> $SIGN
```

| chain | igp | oracle |
|---|---|---|
| Sepolia | `0x48b1BF6CC2e45Ca52947E95Bb216C2eBdCB19c49` | `0x225B8488242c90085B7A8Ea33Ce8e39Ae9f79722` |
| Arbitrum | `0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B` | `0xfA8036Cb092079B095ed60750d7b39c3C220F288` |
| Base | `0x5591613C85E9bC95104980d4485c958ee80f6F76` | `0x7A7042C8784700618be87Aac7F9336620e216Bb9` |

**Router hooks.** No hook means undeliverable messages.

1. Each new EVM router:
   ```sh
   cast call <router> 'hook()(address)' --rpc-url <rpc>
   ```
2. If `0x000…`, use that chain's TIA router hook:
   ```sh
   cast send <router> "setHook(address)" <that hook> --rpc-url <rpc> $SIGN
   ```

## 10. Services

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh && write_config
sudo cp ~/tee-ism-nonzk/deploy/server/teeism-{relayer,gas-oracle}.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now teeism-relayer teeism-gas-oracle
curl -s localhost:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'     # 8 routes
```

Never hand-edit `.state/coprocessor.toml`.

## 11. UI and gateway

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh
HOST=http://<public host>:3000
opt() { has "$1" && load "$1" || true; }
cat > ~/tee-ism-nonzk/bridge-app/.env.local <<ENV
VITE_CELESTIA_NAME=Celestia teeism
VITE_CELESTIA_CHAIN_ID=teeism-local
VITE_CELESTIA_DOMAIN=1297040299
VITE_CELESTIA_RPC=$HOST/rpc
VITE_CELESTIA_REST=$HOST/rest
VITE_CELESTIA_EXPLORER=$HOST
VITE_RELAYER_API=$HOST/api
VITE_CELESTIA_MAILBOX_ID=$(load mailbox-id)
VITE_CELESTIA_ISM_ID=$(load routing-ism-id)
VITE_CELESTIA_IGP_ID=$IGP
VITE_CELESTIA_TIA_ROUTER=$(load celestia-token-id)
VITE_CELESTIA_TEEUSD_ROUTER=$(opt celestia-teeusd-token-id)
$(for c in sepolia arbitrum base eden; do C=$(echo $c | tr a-z A-Z)
  echo "VITE_${C}_RPC=$HOST/evm/$c/"
  echo "VITE_${C}_ISM=$(opt ism-$c)"
  echo "VITE_${C}_TIA_ROUTER=$(opt $c-router)"
  echo "VITE_${C}_TEEUSD_ROUTER=$(opt $c-teeusd-router)"; done)
VITE_PROVING_SECONDS=30
ENV
cd ~/tee-ism-nonzk/bridge-app && npm install --silent && VITE_DEVNET=1 npm run build
grep -c 'localhost:26657' dist/assets/index-*.js      # must print 0
cd ~/tee-ism-nonzk/devnet/gateway && UI_DIST=~/tee-ism-nonzk/bridge-app/dist docker compose up -d
```

Build the UI on the server (`.env.local` is baked in).

## 12. Check

```sh
B=http://<public host>:3000
curl -s -o /dev/null -w '%{http_code}\n' $B/                  # 200
curl -s $B/rpc/status | jq -r .result.node_info.network       # teeism-local
curl -s $B/api/status | jq -r '.[] | "\(.name) \(.height)"'   # 8 routes
```

Then send 0.1 TIA each way ([INTERACT.md](INTERACT.md)).

---

## Appendix

### A. Adding an asset

No enclave or ISM changes.

1. Add it to `TOKENS` in `scripts/90-evm-warp.sh` and to `scripts/50-warp-celestia.sh` (like teeUSD).
2. Re-run both, set router hooks (step 9), `write_config`, restart the relayer.
3. Routers to `.env.local` (step 11), asset to `[trade]` in `write_config`; rebuild the UI.

### B. The venue

Needs teeUSD, its pools and `90-evm-warp.sh`'s factories ([API.md](API.md)).

Faucet (base units): `FAUCET_TIA` 5k, `FAUCET_TEEUSD` 20k, pauses below `FAUCET_TIA_FLOOR` 500M / `FAUCET_TEEUSD_FLOOR` 1B, from `FAUCET_KEY` (`user`). Uniswap: `uniswap_for` in `scripts/lib.sh`.

### C. Adding a chain

**Origin**: one file per crate, under `l1/` (light client) or `l2/` (rollup).

| file | implement | returns |
|---|---|---|
| `tee-node/src/chains/l2/arbitrum.rs` | `origin::Origin` | `verify`: the verified state root. `merkle_tree`: the Hyperlane tree under it |
| `tee-coprocessor/src/origin/l2/arbitrum.rs` | `origin::Indexer` | `gather`: the inputs for those two. `index`: messages between heights. `bootstrap`: a genesis state |

secp256k1 sequencer: reuse `tee-node/src/chains/l2/sequencer.rs` and `tee-coprocessor/src/origin/l2/sequenced.rs`.

Register it:
1. cargo feature in `tee-node/Cargo.toml`; chain in `origin::chains()`
2. image: chain in `flake.nix` (`chains`, `chainFiles`, `keeps`), a `deploy/docker-compose.<chain>.yml`, a port in `enclave_port` and `ENCLAVE_CHAINS` in `devnet/scripts/lib.sh`
3. `kind` arm in `tee-coprocessor/src/config.rs`
4. its table in `write_config`; a row in the ISM scripts

Rules:
- Addresses, storage slots, keys: constants, never inputs. Time: never an input.
- A new image changes the shared CVM's identity and every ISM attested from it.

**Destination**: EVM reuses `tee-coprocessor/src/destination/evm.rs` and `TeeDcapIsm.sol`. Others need a `TeeDcapIsm`-like ISM and `destination/<kind>.rs` (`destination::Destination`).

### D. Automata on a new EVM chain

In `devnet/automata/`:

1. `make deploy-helpers && make deploy-dao`
2. `forge script script/automata/DeployCrlV2.s.sol`
3. `deploy_versioned.sh` for `storage-v2`, `tcb-eval`, `versioned 20`, `fmspc-v2 20`
4. `grantRoles(deployer, 1)` on each versioned DAO and on the TcbEvalDao
5. copy the PCCS record to the attestation repo's
   `network-registry/deployment/current/<chain-id>/onchain_pccs.json`
6. point `dcap.json` at our entrypoint, then run `DeployRouter`, `deployEntrypoint()`,
   `DeployVerifier`
7. on the router: `setQeIdDaoVersionedAddr(20, …)`, `setFmspcTcbDaoVersionedAddr(20, …)`
8. `setCallerAuthorization(router, true)` on both storage contracts
9. write `.state/out/pccs-<chain>.json`, then run `scripts/seed-evm-collateral.sh <chain>`

Ledger: `make`, `deploy_versioned.sh` take `LEDGER=true OWNER=<ledger address>`; step 2 `--ledger --sender <ledger address>`; steps 4, 7, 8 `cast send … $SIGN`. In step 4 grant `EVM_PRIVATE_KEY`'s address (monthly seeding).

Pitfalls:
- keep `Salt.sol` domain-separated
- `SKIP_ESTIMATE=true`, as a string
- send transactions one at a time
- TCB info and QE identity need hand-built calldata
- Eden accepts TCB info only in the V1 DAO
- `openzeppelin-contracts` must be v5.0.2

Recorded in `devnet/.state/out/pccs-<chain>.json` (plus `V5QuoteVerifier` from `75-dcap-verifiers.sh`):

| contract | Sepolia | Arbitrum | Base | Eden |
|---|---|---|---|---|
| AttestationEntrypoint | `0x961D4408f512D4a169bD76433460d2981a70c71F` | same | same | `0x6d748C482E0Eae5b63b5664CDaBE2C4F0461B6B8` |
| V4QuoteVerifier | `0xFFd8Ddff9b7e9ce124A7fdddcd817bA4d8B37ab7` | same | same | `0x0D00346F2FE363BBf086f1F753f6A3426627Ea92` |
| PCCSRouter | `0xdA7336571D634bE002035Af6ec55F0816A2Ed263` | same | same | `0x2c86Cb1a65079D6389c7bb0cf375651a36e8E992` |
| PcsDao | `0x3c3fF9105e62228c7dA62C3bA04d24D320c4433C` | same | same | same |
| EnclaveIdentityDaoVersioned | `0x426B9aC0e424dEcC66e4C3a7d9293839e16D8fc1` | same | same | `0x4108d529200FE300EF38E7E68447E34bc8d4b891` |
| FmspcTcbDaoVersioned | `0x7BDA83918CAAD9b5EC7F88A24660167E90053690` | `0xe08E2eE491666702312Fc71566420e2C221DdeC3` | `0x06D080A8642803465500D6C9004Cc9CF48094EeD` | `0x61F9E7c62B7c3ade50aeD35065Ea7a8733417471` (V1) |
| TcbEvalDao | `0x03b1B658C34Bb7919A9cA2067d0055f7dD5C5495` | same | same | `0xDd0F2b5B38391f1Bdf47c670aA161d5Aed7310d7` |

### E. Celestia and one EVM chain, signed on a Ledger

Main steps, Eden only, with these differences:

**Steps 0–1.** In `devnet/.env`:
```sh
EVM_SIGNER=ledger
```
```sh
LEDGER=$(cast wallet address --ledger); SIGN="--ledger --from $LEDGER"; EDEN=https://rpc.testnet.eden.gateway.fm/
```

**Steps 4–5.** Only two enclaves:
```sh
CHAINS="celestia eden" ./scripts/25-images.sh
./scripts/30-enclave-up.sh --celestia --eden
```

**Step 6.** [Appendix D](#d-automata-on-a-new-evm-chain) on Eden, then:
```sh
./scripts/seed-evm-collateral.sh eden          # the relayer's key, with ATTESTER_ROLE
CHAINS=eden ./scripts/75-dcap-verifiers.sh     # Ledger
./scripts/76-tcb-eval.sh eden 21               # before 21 becomes standard (~10 Feb 2027)
```

**Step 7.** As written.

**Hyperlane core on Eden**, before step 8:
```sh
cd ~/tee-ism-nonzk/tee-hyperlane/contracts
LOCAL_DOMAIN=3735928814 OWNER=$LEDGER forge script script/DeployHyperlaneCore.s.sol:DeployHyperlaneCore \
  --rpc-url $EDEN --ledger --sender $LEDGER --broadcast
echo "EDEN_MAILBOX=<Mailbox>" >> ~/tee-ism-nonzk/devnet/.env      # from the output
echo "EDEN_HOOK=<MerkleTreeHook>" >> ~/tee-ism-nonzk/devnet/.env
```

**Step 8**:
```sh
cd ~/tee-ism-nonzk/devnet && set -a && . ./.env && set +a && . scripts/lib.sh
ROW="eden:3735928814:$EDEN_MAILBOX"
./scripts/50-warp-celestia.sh
CHAINS=$ROW ./scripts/80-evm-isms.sh
(cd ../tee-hyperlane/contracts && MAILBOX=$EDEN_MAILBOX MERKLE_TREE_HOOK=$EDEN_HOOK DEFAULT_ISM=$(load ism-eden) \
  OWNER=$LEDGER forge script script/DeployHyperlaneCore.s.sol:InitHyperlaneCore \
  --rpc-url $EDEN --ledger --sender $LEDGER --broadcast)
ORIGINS="eden:3735928814:eden:$(pad32 $EDEN_HOOK)" ./scripts/85-celestia-isms.sh
CHAINS=$ROW ./scripts/90-evm-warp.sh
```
Leave Eden's router hooks empty.

**Step 9.** Celestia block only. `.state/gas-oracle.toml`: keep only the `Eden` destination, remove all `evm_origins`.

**Steps 10–12.** 2 routes. Before the UI build: set Eden's `mailbox:` in `bridge-app/src/config.ts` to `$EDEN_MAILBOX`; use `for c in eden` in `.env.local`. Admin: [ADMIN](ADMIN.md).

### F. Endpoints

Defaults, each overridable in `devnet/.env`:

```
SEPOLIA_RPC       http://127.0.0.1:8545   (reth, step 6b)
SEPOLIA_BEACON    http://127.0.0.1:9596   (Lodestar, step 6b)
SEPOLIA_SEND_RPC  SEPOLIA_RPC              (deliveries; must be at the head)
ARBITRUM_ARCHIVE  Alchemy, from ALCHEMY_KEY
ARBITRUM_FEED     wss://sepolia-rollup.arbitrum.io/feed
ARBITRUM_LOGS     https://sepolia-rollup.arbitrum.io/rpc   (serves eth_getLogs over 1M blocks; publicnode caps at 50k)
ARBITRUM_RPC      https://sepolia-rollup.arbitrum.io/rpc
BASE_ARCHIVE      Alchemy, from ALCHEMY_KEY
BASE_RPC          https://sepolia.base.org
BASE_P2P_PORT     9222   (UDP, for discovery; outbound only)
EDEN_ARCHIVE      https://ev-reth-eden-testnet.binarybuilders.services:8545/
EDEN_RPC          https://rpc.testnet.eden.gateway.fm/
EDEN_DA_RPC       http://localhost:26658
```

Never use a metered key for logs: `eth_getLogs` sweeps drain it and stall every route.
