# DEPLOY

Standing up the whole bridge on one Linux host: a Celestia chain, three enclaves, eight routes
and the UI. Each step is a command block to paste, then a check.

Run everything as the user the systemd units name (`chef`), from `~/tee-ism-nonzk/devnet`
unless a block says otherwise. The scripts are idempotent: a re-run redoes only what is missing
or out of date. Every id they create lands in `.state/out/<name>`, one file each; after
`. scripts/lib.sh`, `load <name>` prints it, and `make -C devnet status` lists them all.

On a laptop, `make -C devnet init && make -C devnet start` runs steps 3, 5 and 8 against a
local chain and starts the relayer and a dev UI. `make stop` deletes the enclaves and
`.state/`; never run it on a deployed host.

---

## 0. What you need

```
docker + compose, go 1.26+, rust, foundry (forge, cast), node + npm, python3, jq, curl
phala CLI, logged in (phala status)          nix, to build the enclave images
```

| account | for | fund with |
|---|---|---|
| a fresh EVM key, used by nothing else | every EVM deploy and relayer submission | Sepolia, Arbitrum Sepolia, Base Sepolia ETH; Eden TIA |
| Phala | three `tdx.small` CVMs, $4.38/day | a Phala balance |
| Alchemy, free tier | Base archive reads | nothing |

Only port 3000 must be reachable (3001 and 3002 are dashboards). The chain is served through
the gateway on 3000.

## 1. Sources and secrets

```sh
git clone -b jonas/tee-ism git@github.com:jonas089/tee-hyperlane.git       ~/tee-ism-nonzk
git clone -b jonas/tee-ism https://github.com/celestiaorg/celestia-app.git ~/celestia-app-local
cd ~/tee-ism-nonzk
ln -s ../../deploy/check-secrets.sh .git/hooks/pre-commit      # refuses a commit carrying a key
mkdir -p devnet/.state && chmod 700 devnet/.state
cp devnet/.env.example devnet/.env && chmod 600 devnet/.env
$EDITOR devnet/.env      # EVM_PRIVATE_KEY (keep the 0x) and ALCHEMY_BASE_KEY
```

celestia-app must be at `../celestia-app-local` (or set `CELESTIA_APP_DIR`), and only ever on
`jonas/tee-ism`. `devnet/.env` and `devnet/.state/` are gitignored, and the repo is public.

> Never share `EVM_PRIVATE_KEY` with another running deployment. Two relayers on one key race
> for nonces and fail at random with `nonce too low`.

## 2. Build

```sh
cd ~/celestia-app-local
for b in cmd/celestia-appd x/teeism/cmd/teeism-collateral x/teeism/cmd/teeism-identity; do
  go build -o ~/tee-ism-nonzk/devnet/.state/bin/$(basename $b) ./$b
done
cd ~/tee-ism-nonzk/tee-hyperlane && cargo build --release -p tee-coprocessor -p gas-oracle
```

## 3. The chain

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/10-celestia-up.sh          # image, mnemonic (written back to .env), genesis, start
./scripts/20-celestia-hyperlane.sh   # mailbox, merkle tree hook, noop hook
grep -E '^pruning|^min-retain-blocks' .state/celestia/config/app.toml
```

The check must print `pruning = "nothing"` and `min-retain-blocks = 0`. A quiet route's ISM
stays at an old height, and the light client needs the commit at that height to move on. If a
pruned chain drops it, the ISM has to be replaced.

## 4. Enclave images

Needs x86_64 Linux. Skip this step to reuse the digests already pinned in
`deploy/docker-compose.<family>.yml`.

```sh
cd ~/tee-ism-nonzk
echo "$GHCR_TOKEN" | docker login ghcr.io -u jonas089 --password-stdin   # PAT with write:packages
for f in celestia ethereum evolve; do
  nix build .#image-$f -o result-$f
  docker load < result-$f
  docker push ghcr.io/jonas089/tee-node:reproducible-$f
  d=$(docker inspect --format '{{index .RepoDigests 0}}' \
        ghcr.io/jonas089/tee-node:reproducible-$f | cut -d@ -f2)
  sed -i "s|tee-node@sha256:[0-9a-f]*|tee-node@$d|" deploy/docker-compose.$f.yml
done
grep image: deploy/docker-compose.*.yml
```

The digest to pin is the registry's, which exists only after the push. Builds are
reproducible, so an unchanged digest means that family's code did not change. Commit the
compose files, since they are what the enclaves measure.

## 5. Enclaves

```sh
cd ~/tee-ism-nonzk/devnet
./scripts/30-enclave-up.sh
for f in celestia ethereum evolve; do
  FAMILY=$f ../deploy/verify-digest.sh "$(cat .state/out/enclave-app-id-$f)"
done
```

The script deploys one CVM per family (node 18, `dstack-0.5.9`, `--no-dev-os`) and records
each url and identity. It keeps an existing CVM only if that CVM measured the current compose
file. Every `verify-digest.sh` check must say `ok`.

> Never run `phala cvms upgrade` on an enclave. It rewrites the measured compose `name` field
> to the app id, producing a `compose_hash` no ISM pins. Always deploy fresh.

## 6. Automata DCAP, per EVM chain

Each EVM chain verifies quotes through our own Automata deployment. Sepolia, Arbitrum, Base
and Eden already have one, so copy its address records and publish current collateral:

```sh
scp <previous-host>:tee-ism-nonzk/devnet/.state/out/pccs-*.json .state/out/
for c in sepolia arbitrum base eden; do ./scripts/seed-evm-collateral.sh $c; done
```

Without the records, step 8 prints `no PCCS on <chain>; skipping` and continues. For a chain
with no deployment yet, see [appendix C](#c-automata-on-a-new-evm-chain).

## 7. Mocha light node (Eden only)

Eden posts its headers to mocha, and proving a blob needs a celestia-node light node.

```sh
IMG=ghcr.io/celestiaorg/celestia-node:v0.34.2-mocha
D=$PWD/.state/mocha-light && mkdir -p "$D"
docker run --rm -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG celestia light init --p2p.network mocha
$EDITOR $D/config.toml
#   under [Share.LightAvailability] add   SampleAmount = 16
#   set Header.Syncer.PruningWindow to    "800h0m0s"
docker run -d --name mocha-light --restart unless-stopped -p 127.0.0.1:26658:26658 \
  -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG \
  celestia light start --p2p.network mocha --rpc.addr 0.0.0.0 --rpc.port 26658 --rpc.skip-auth
```

Use a `-mocha` tag; other tags cannot sync mocha-5. The first sync takes about half an hour.
Until it finishes, step 8 skips Eden's ISM, so re-run step 8 afterwards.

## 8. Warp routes and ISMs

```sh
./scripts/50-warp-celestia.sh    # TIA collateral and USDC synthetic on Celestia
./scripts/80-evm-isms.sh         # a TeeDcapIsm per EVM chain, pinning the celestia enclave
./scripts/85-celestia-isms.sh    # an ISM per EVM origin, and the routing ISM over them
./scripts/90-evm-warp.sh         # EVM routers, pointed at their ISM, enrolled both ways
. scripts/lib.sh && for k in ism-sepolia ism-arbitrum ism-base ism-eden \
  ism-celestia-sepolia ism-celestia-arbitrum ism-celestia-base ism-celestia-eden routing-ism-id; do
  printf '%-24s %s\n' $k "$(load $k 2>/dev/null)"; done
```

Every line of the check must show an id. Each ISM's genesis anchors at its origin's current
head.

`CHAINS=` (for 80 and 90) and `ORIGINS=` (for 85) bring up a subset. Set both or neither: an ISM
without an enrolled router attests fine and then fails every delivery with `no enrolled
router`.

> `90-evm-warp.sh` re-points an existing router rather than replacing it. If it ever prints
> `deploying the collateral USDC router` on a chain that already has one, stop it: a new
> collateral router abandons the old one's escrow.

## 9. Paymaster and gas oracle

On Celestia, create an IGP, hand it to a dedicated `bridge` key (account 3), and make it the
mailbox's default hook:

```sh
. scripts/lib.sh
A=.state/bin/celestia-appd; H="--home .state/celestia --keyring-backend test"
TX="$H --chain-id teeism-local --node http://localhost:26657 --fees 200000utia --gas 400000 -y"
$A tx hyperlane hooks igp create utia --from relayer $TX      # prints the igp id
IGP=<igp id>
echo "$CELESTIA_MNEMONIC" | $A keys add bridge --recover --account 3 $H --output json
BRIDGE=$($A keys show bridge -a $H)
$A tx bank send relayer $BRIDGE 100000000utia $TX
$A tx hyperlane hooks igp set-owner $IGP --new-owner $BRIDGE --from relayer $TX
$A tx hyperlane mailbox set "$(load mailbox-id)" --required-hook "$(load merkle-hook-id)" \
  --default-hook $IGP --from relayer $TX
```

On each EVM chain, register this chain's domain with the IGP. Without it, `transferRemote`
reverts with `IGP: no gas oracle for domain 1297040299`:

```sh
cast send <igp> "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(1297040299,(<storage gas oracle>,150000))]" --rpc-url <rpc> --private-key $EVM_PRIVATE_KEY
```

| chain | igp | storage gas oracle |
|---|---|---|
| Sepolia | `0x48b1BF6CC2e45Ca52947E95Bb216C2eBdCB19c49` | `0x225B8488242c90085B7A8Ea33Ce8e39Ae9f79722` |
| Arbitrum Sepolia | `0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B` | `0xfA8036Cb092079B095ed60750d7b39c3C220F288` |
| Base Sepolia | `0x5591613C85E9bC95104980d4485c958ee80f6F76` | `0x7A7042C8784700618be87Aac7F9336620e216Bb9` |

Each EVM router's hook must be the chain's `TreeAndPaymasterHook`, never the IGP on its own. A
router has one hook, and without the merkle tree hook its messages are never inserted and can
never be attested. For a new router, copy the hook from the TIA router on the same chain:

```sh
cast send <router> "setHook(address)" "$(cast call <tia router> 'hook()(address)' --rpc-url <rpc>)" \
  --rpc-url <rpc> --private-key $EVM_PRIVATE_KEY
```

Then configure the oracle and run one round:

```sh
cp ../deploy/gas-oracle.toml.example .state/gas-oracle.toml
sed -i "s|^igp_id = .*|igp_id = \"$IGP\"|" .state/gas-oracle.toml
printf '%s' "$EVM_PRIVATE_KEY" > .state/evm-key && chmod 600 .state/evm-key   # its evm_key_file
../tee-hyperlane/target/release/gas-oracle --config .state/gas-oracle.toml --once
```

## 10. Services

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh && write_config
sudo cp ../deploy/server/teeism-{relayer,gas-oracle}.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now teeism-relayer teeism-gas-oracle
curl -s localhost:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'
```

All eight routes must show a height. `write_config` generates `.state/coprocessor.toml` from
`.state/out/` and `.env`, and rebuilds the binary if the source changed. Never edit the toml by
hand; [coprocessor.toml.example](coprocessor.toml.example) is what `write_config` produces on
ark. The relayer unit reads `devnet/.env` directly and puts `.state/bin` first on PATH, because
it signs by calling `cast` and `celestia-appd`.

The faucet needs no setup. It gives 1000 TIA per address from the `faucet` key (account 4),
which `10-celestia-up.sh` funds at genesis, and the relayer serves it.

## 11. UI and gateway

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh
HOST=http://<public host>:3000
opt() { has "$1" && load "$1" || true; }
cat > ../bridge-app/.env.local <<ENV
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
VITE_CELESTIA_USDC_ROUTER=$(opt celestia-usdc-token-id)
$(for c in sepolia arbitrum base eden; do C=$(echo $c | tr a-z A-Z)
  echo "VITE_${C}_RPC=$HOST/evm/$c/"
  echo "VITE_${C}_ISM=$(opt ism-$c)"
  echo "VITE_${C}_TIA_ROUTER=$(opt $c-router)"
  echo "VITE_${C}_USDC_ROUTER=$(opt $c-usdc-router)"; done)
VITE_PROVING_SECONDS=30
ENV
cd ../bridge-app && npm install --silent && VITE_DEVNET=1 npm run build
grep -c 'localhost:26657' dist/assets/index-*.js      # must print 0
cd ../devnet/gateway && UI_DIST=~/tee-ism-nonzk/bridge-app/dist docker compose up -d
```

Build the bundle on the host that serves it, because Vite compiles `.env.local` into it. An
empty `*_ROUTER` hides that asset on that route.

## 12. Check it

```sh
B=http://<public host>:3000
curl -s -o /dev/null -w '%{http_code}\n' $B/                     # 200
curl -s $B/rpc/status | jq -r .result.node_info.network          # teeism-local
curl -s -X POST $B/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"status"}' | head -c 20   # JSON, not a redirect
curl -s $B/api/status | jq -r '.[] | "\(.name) \(.height)"'      # eight routes
```

Then bridge 0.1 TIA each way on one route ([INTERACT.md](INTERACT.md)). Celestia to EVM lands
in under a minute.

---

## Appendix

### A. Adding an asset

ISMs are per origin chain, not per token, so a new asset needs no enclave or ISM changes.

1. Add a row to `TOKENS` in `scripts/90-evm-warp.sh` (and an arm to `kind_for` if the asset is
   native to an EVM chain). Add the Celestia side to `scripts/50-warp-celestia.sh`.
2. Re-run `50-warp-celestia.sh` and `90-evm-warp.sh`, set each new EVM router's hook (step 9),
   then run `write_config` and restart the relayer.
3. Add the new routers' `VITE_*` values (step 11), rebuild the UI, and fund the collateral side.

On Celestia a router id is 32 bytes; the CLI rejects a bare 20-byte address.

### B. Adding a chain

**As an origin**, a chain is one file in each crate, under the chain its trust comes from:

| crate | implements | does |
|---|---|---|
| `tee-node` | `origin::Origin` | `verify` authenticates a head and returns its state root; `merkle_tree` proves the Hyperlane tree under it |
| `tee-coprocessor` | `origin::Indexer` | `gather` fetches what those two need; `index` lists the messages between two heights; `bootstrap` builds a genesis state |

Then:
- a `Chain { name, domain, origin }` entry in the parent module's `CHAINS`
- a `kind` arm in `tee-coprocessor/src/config.rs`
- the chain's table and routes in `write_config`
- a row in the ISM scripts (step 8) and its UI values (step 11)

Adding the chain to an existing family changes only that family's identity. A new family also
needs:
- entries in `families` and `familyOnly` in `flake.nix`
- a cargo feature
- a compose file

Two rules for every `verify`:
- take anchor contracts, storage slots and keys from constants in the chain's file, never from
  the input
- never take the time from the input

Before relying on a storage layout, reproduce a live `(count, root)` from raw state in a test,
then run `tests/live.rs` for the chain.

**As a destination**, a chain needs an ISM that behaves like `TeeDcapIsm.sol`, and a
`Destination` in `tee-coprocessor/src/destination.rs`. EVM chains reuse `destination::Evm`.

### C. Automata on a new EVM chain

Automata's `new-network.sh` is not enough. In `devnet/automata/`, against the new chain:

1. `make deploy-helpers`, then `make deploy-dao`
2. `forge script script/automata/DeployCrlV2.s.sol`. This deploys `PccsDependencyConfig`, which
   the versioned DAOs need and the upstream sequence never deploys.
3. `deploy_versioned.sh` for `storage-v2`, `tcb-eval`, `versioned 20`, `fmspc-v2 20`
4. `grantRoles(deployer, 1)` on each versioned DAO and on the TcbEvalDao
5. copy the PCCS record into the attestation repo at
   `rust-crates/libraries/network-registry/deployment/current/<chain-id>/onchain_pccs.json`
6. point `dcap.json` at our entrypoint, then run `DeployRouter`, `deployEntrypoint()` and
   `DeployVerifier`
7. `setQeIdDaoVersionedAddr(20, …)` and `setFmspcTcbDaoVersionedAddr(20, …)` on the router
8. `setCallerAuthorization(router, true)` on both storage contracts
9. write `.state/out/pccs-<chain>.json`, then run `scripts/seed-evm-collateral.sh <chain>`

Pitfalls:
- `Salt.sol` must stay domain-separated, or CREATE2 lands on Automata's own addresses.
- `SKIP_ESTIMATE` must be the string `true`.
- `config_versioned.sh` reports `granted` on the V2 DAO without granting anything.
- `cast` cannot encode a tuple holding a JSON string, so TCB info and QE identity need
  hand-built calldata.
- Transactions sent back to back fail with `replacement transaction underpriced`.
- On Eden only the V1 FMSPC DAO accepts TCB info.
- Needed submodules: `forge-std`, `solady`, and `openzeppelin-contracts` at v5.0.2.

The addresses are the same on every chain, because CREATE2 goes through a shared deployer:

```
AttestationEntrypoint  0x961D4408f512D4a169bD76433460d2981a70c71F
PCCSRouter             0xdA7336571D634bE002035Af6ec55F0816A2Ed263
V4QuoteVerifier        0xFFd8Ddff9b7e9ce124A7fdddcd817bA4d8B37ab7
FmspcTcbDao_20         0x7BDA83918CAAD9b5EC7F88A24660167E90053690   (Base: 0x06D080A8642803465500D6C9004Cc9CF48094EeD)
```

### D. Endpoints

These are `write_config`'s defaults. Override any of them in `devnet/.env` with the variable
in the last column. Only Base's archive needs a key.

```
sepolia   rpc (archive)   https://rpc.sepolia.ethpandaops.io                          SEPOLIA_RPC
sepolia   beacon          https://ethereum-sepolia-beacon-api.publicnode.com          SEPOLIA_BEACON
arbitrum  rpc (archive)   https://api.zan.top/arb-sepolia                             ARBITRUM_ARCHIVE
arbitrum  logs            https://arbitrum-sepolia-rpc.publicnode.com                 ARBITRUM_LOGS
arbitrum  send            https://sepolia-rollup.arbitrum.io/rpc                      ARBITRUM_RPC
base      rpc (archive)   Alchemy, from ALCHEMY_BASE_KEY                              BASE_ARCHIVE
base      logs, send      https://sepolia.base.org                                    BASE_RPC
eden      rpc             https://ev-reth-eden-testnet.binarybuilders.services:8545/  EDEN_ARCHIVE
eden      logs, send      https://rpc.testnet.eden.gateway.fm/                        EDEN_RPC
eden      da              http://localhost:26658                                      EDEN_DA_RPC
```

- **Base.** Its origin reads `eth_getProof` about five days back, which no free Base endpoint
  serves. The key goes only to Base's `rpc`. Base's logs go to `sepolia.base.org`, because the
  free tier caps `eth_getLogs` at ten blocks.
- **Eden.** Its `rpc` must serve `debug_executionWitness`, and no public Eden endpoint does.
- **Metered keys.** Never put one on an endpoint that serves logs. The index sweeps `eth_getLogs`
  there, and exhausting the key backs off every route.
