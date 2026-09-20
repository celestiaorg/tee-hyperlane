# Standing this up on another host

Everything on the deployment host, start to finish. Two Phala CVMs are the only pieces that
are not on it.

Paths below assume the layout the unit files expect: sources under `~/celestia-app-teeism`
and `~/tee-ism-nonzk`, a user named `chef`. Change the user and you must change it in the
three unit files too; they hardcode it.

## 0. What the host needs

```sh
docker, docker compose            the chain and the gateway
go                                celestia-appd and the teeism helpers
rust + cargo                      the coprocessor and the gas oracle
foundry (forge, cast)             contracts, and the oracle shells out to cast
node + npm                        the UI
python3, jq, curl                 the scripts
phala                             the CVMs; `phala status` must show you logged in
```

The host also needs **one forwarded port**. This deployment has 3000 and 3001 and 3002 open,
and nothing else: 26657, 1317 and 9090 are not reachable from outside, which is why the chain
is served through the gateway rather than directly. If your host forwards everything, the
gateway is still how the UI and Keplr reach the chain on one origin.

## 1. Sources

```sh
git clone -b jonas/tee-ism git@github.com:jonas089/tee-hyperlane.git      ~/tee-ism-nonzk
git clone -b jonas/tee-ism https://github.com/celestiaorg/celestia-app.git ~/celestia-app-teeism
```

`jonas/tee-ism` is the branch on both. Nothing in this deployment lives outside them except
the three files in the next step, so a clone is the whole of it.

## 2. Secrets

```sh
mkdir -p ~/tee-ism-nonzk/devnet/.state && chmod 700 ~/tee-ism-nonzk/devnet/.state
cd ~/tee-ism-nonzk/devnet/.state
printf '%s' "<evm private key>"  > evm-key      && chmod 600 evm-key
printf '%s' "<alchemy key>"      > alchemy-key  && chmod 600 alchemy-key
# The genesis mnemonic. Copy the existing one to keep the same funded address, or omit the
# file and 10-celestia-up.sh generates one.
printf '%s' "<24 words>"         > mnemonic     && chmod 600 mnemonic
```

`make stop` preserves all three.

## 3. Build

```sh
cd ~/celestia-app-teeism
docker build -t celestia-app-teeism:local -f docker/standalone.Dockerfile .
mkdir -p ~/tee-ism-nonzk/devnet/.state/bin
go build -o ~/tee-ism-nonzk/devnet/.state/bin/celestia-appd      ./cmd/celestia-appd
go build -o ~/tee-ism-nonzk/devnet/.state/bin/teeism-collateral  ./x/teeism/cmd/teeism-collateral
go build -o ~/tee-ism-nonzk/devnet/.state/bin/teeism-identity    ./x/teeism/cmd/teeism-identity

cd ~/tee-ism-nonzk/tee-hyperlane
cargo build --release -p tee-coprocessor -p gas-oracle
```

The cargo build needs **Go on PATH** for a cgo dependency and fails with `Failed to build Go
library` if it is missing, naming neither Go nor the crate.

## 4. The chain

```sh
cd ~/tee-ism-nonzk/devnet
export DEVNET_MNEMONIC="$(head -1 .state/mnemonic)"
STATE_DIR=../.state CELESTIA_IMAGE=celestia-app-teeism:local CHAINID=teeism-local \
CELESTIA_UID="$(id -u):$(id -g)" DEVNET_MNEMONIC="$DEVNET_MNEMONIC" \
  docker compose -f celestia/docker-compose.yml up -d
```

`CELESTIA_UID` is not optional on Linux. The home directory is a bind mount and the image
runs as uid 10001; without it the chain dies immediately with `permission denied` creating its
data directory. Docker Desktop hides this, so it does not reproduce on macOS.

Confirm pruning is off before anything else depends on it:

```sh
grep -E '^pruning|^min-retain-blocks' .state/celestia/config/app.toml
# pruning = "nothing"   min-retain-blocks = 0
```

An ISM only advances when a batch is delivered, so a quiet route's trusted height falls behind
the head. The light client proves forward from that height and needs the commit at it. With
the default 3000-block retention that commit is gone in fifty minutes and the route cannot be
recovered - the ISM has to be redeployed.

## 5. Hyperlane, enclaves and ISMs

```sh
./scripts/20-celestia-hyperlane.sh            # mailbox, merkle hook, noop hook

phala deploy --name tee-nonzk-cel --compose enclave/docker-compose.yml \
  --instance-type tdx.small --node-id 18 --image dstack-0.5.9 --no-dev-os --wait --json
phala deploy --name tee-nonzk-eth --compose enclave/docker-compose.yml ...   # same flags
```

`--no-dev-os` matters: with an SSH key present the CLI otherwise provisions an image that
permits shell access into the CVM, which makes the measurements meaningless.

Both CVMs run the same compose, so they measure identically and one ISM identity accepts
either. Record the cel one and continue:

```sh
printf '%s' "https://<cel-app-id>-8080.dstack-pha-prod9.phala.network" > .state/out/enclave-url
printf '%s' "<cel-app-id>" > .state/out/enclave-app-id

ENCLAVE_URL="https://<eth-app-id>-8080.dstack-pha-prod9.phala.network" ./scripts/40-create-ism.sh
./scripts/50-warp-celestia.sh
```

Then the two L2-origin ISMs on Celestia, the three EVM ISMs, and the routing ISM that fans
three origins into one token. Those steps, with the exact commands, are in
[TEEISM-SERVER.md](TEEISM-SERVER.md) under "Warp routers" and "One ISM is not enough".

The PCCS address records must be present before `80-evm-isms.sh`, or it prints
`no PCCS on <chain>; skipping` and deploys nothing:

```sh
cp <from the previous host>/pccs-*.json .state/out/
```

## 5b. USDC

TIA is Celestia-native, so its Celestia side is a collateral token and the EVM sides are
synthetics. USDC is the other way round: the collateral lives on Sepolia and Celestia holds a
synthetic. Reusing an existing EVM router is what makes this two commands rather than a
deployment.

```sh
A=.state/bin/celestia-appd
TX="--from relayer --keyring-backend test --home .state/celestia --chain-id teeism-local \
    --node http://localhost:26657 --fees 200000utia --gas 900000 --broadcast-mode sync --yes -o json"

# Celestia side: a synthetic, pointed at the routing ISM like every other recipient.
$A tx warp create-synthetic-token $(cat .state/out/mailbox-id) $TX
# token id is deterministic: 0x726f757465725f61707000000000000000000000000000020000000000000001
$A tx warp set-token $USDC_TOKEN --ism-id $(cat .state/out/routing-ism-id) $TX
$A tx warp enroll-remote-router $USDC_TOKEN 11155111 <sepolia router, 32 bytes> 50000 $TX

# Sepolia side: the existing collateral router, repointed.
cast send $SEPOLIA_USDC "setInterchainSecurityModule(address)" $(cat .state/out/ism-sepolia)
cast send $SEPOLIA_USDC "enrollRemoteRouter(uint32,bytes32)" 1297040299 $USDC_TOKEN
cast send $SEPOLIA_USDC "setHook(address)" <the same aggregation hook the TIA router uses>
```

The denom on Celestia is `hyperlane/<token id>`, which is what the UI's `CELESTIA_DENOM` has
to name. Sending from Sepolia needs an ERC20 `approve` to the router first; sending from
Celestia does not, because the synthetic is burned rather than transferred.

Arbitrum and Base hold synthetics too, wired the same way: point the router at that chain's
TeeDcapIsm, enrol the Celestia token, set the hook the TIA router on that chain uses, and
enrol the router back on the Celestia token. Then add each router to `routers` on both of its
routes, or its transfers cannot trigger a batch and wait for other traffic.

## 6. Paymaster and oracle

Also in [TEEISM-SERVER.md](TEEISM-SERVER.md): create the IGP, derive and fund the `bridge`
key the oracle signs with, transfer IGP ownership to it, set the mailbox hooks
(`--required-hook` merkle, `--default-hook` igp), and register this deployment's Celestia
domain on each EVM IGP with `setDestinationGasConfigs`.

Write `.state/gas-oracle.toml` from [gas-oracle.teeism.toml](gas-oracle.teeism.toml),
adjusting `igp_id` and `evm_key_file`.

## 7. Relayer config

Write `.state/coprocessor.toml` from [coprocessor.teeism.toml](coprocessor.teeism.toml),
replacing `ALCHEMY_KEY` and every ISM and router address with this deployment's.

Two things in it are load-bearing:

- No endpoint here needs an API key. The free ones that do each job, verified:

```
sepolia   execution + archive   https://rpc.sepolia.ethpandaops.io     serves eth_getProof historically
sepolia   beacon                https://ethereum-sepolia-beacon-api.publicnode.com
arbitrum  destination + logs    https://sepolia-rollup.arbitrum.io/rpc, publicnode
arbitrum  l2 archive            https://api.zan.top/arb-sepolia        the only free eth_getProof past ~1k blocks
base      destination + logs    https://sepolia.base.org
base      l2 archive            none found; see below
```

  Base-origin needs `eth_getProof` roughly 220k blocks back, the five day dispute window.
  Every free endpoint tried refuses with "distance to target block exceeds maximum permitted",
  and Base's own RPC does not serve the method at all. That route needs a paid archive or a
  self-hosted node.

- `archive_rpc` must **not** be a metered key. `dispatched_messages` runs on the archive
  reader, so a large `eth_getLogs` sweep goes there; pointing it at a rate-limited key
  exhausts the tier and backs off every route at once, including routes with nothing to do
  with Ethereum. `https://rpc.sepolia.ethpandaops.io` serves both logs and historical
  `eth_getProof`.
- `attest_only = true` on every route. That is what selects the direct submit path; without
  it the route uses the proof-carrying script and fails on a missing vkey.

## 8. Services

```sh
sudo cp deploy/server/teeism-{relayer,api,gas-oracle}.service /etc/systemd/system/
printf 'EVM_PRIVATE_KEY=0x%s\n' "$(cat .state/evm-key)" > .state/relayer.env && chmod 600 .state/relayer.env
sudo systemctl daemon-reload
sudo systemctl enable --now teeism-relayer teeism-api teeism-gas-oracle
```

Each unit sets `PATH` with `.state/bin` **first**. The API and the oracle shell out to
`celestia-appd` by name, and any other build on the host will not have the teeism module:
the symptom is `unknown command "teeism" for "query"` on every Celestia-origin route.

The API and the oracle bind `0.0.0.0`, not localhost, or their dashboards are unreachable
from anywhere but the host.

## 9. The gateway

```sh
cd ~/tee-ism-nonzk/bridge-app
cat > .env.local <<ENV
VITE_CELESTIA_NAME=Celestia teeism
VITE_CELESTIA_CHAIN_ID=teeism-local
VITE_CELESTIA_DOMAIN=1297040299
VITE_CELESTIA_RPC=http://<host>:3000/rpc
VITE_CELESTIA_REST=http://<host>:3000/rest
VITE_CELESTIA_EXPLORER=http://<host>:3000
VITE_RELAYER_API=http://<host>:3000/api
VITE_CELESTIA_MAILBOX_ID=<mailbox-id>
VITE_CELESTIA_ISM_ID=<routing-ism-id>
VITE_CELESTIA_IGP_ID=<igp-id>
VITE_CELESTIA_TIA_ROUTER=<celestia-token-id>
VITE_CELESTIA_USDC_ROUTER=<celestia-usdc-token-id>
VITE_SEPOLIA_RPC=http://<host>:3000/evm/sepolia/
VITE_ARBITRUM_RPC=http://<host>:3000/evm/arbitrum/
VITE_BASE_RPC=http://<host>:3000/evm/base/
VITE_SEPOLIA_ISM=<TeeDcapIsm on sepolia>
VITE_ARBITRUM_ISM=<TeeDcapIsm on arbitrum>
VITE_BASE_ISM=<TeeDcapIsm on base>
VITE_SEPOLIA_USDC_ROUTER=<sepolia usdc collateral router>
# Left empty deliberately: this deployment has no USDC on the L2s, and an empty value is how
# a route reports itself as not deployed instead of offering a button that cannot work.
VITE_ARBITRUM_USDC_ROUTER=
VITE_BASE_USDC_ROUTER=
VITE_PROVING_SECONDS=30
ENV
VITE_DEVNET=1 npm install --silent && VITE_DEVNET=1 npm run build

cd ../devnet/gateway
# site.conf ships with ALCHEMY_ETH/ARB/BASE placeholders; substitute or point them at the
# public endpoints, which is what this deployment does.
UI_DIST=~/tee-ism-nonzk/bridge-app/dist docker compose up -d
```

A container rather than the host's nginx, deliberately: the host's nginx may serve unrelated
sites that bind port 80, and if anything else holds 80 it cannot start at all.

Every proxied path must answer on both spellings. nginx redirects `/rpc` to `/rpc/` with a
301, and CosmJS POSTs to the RPC root without the slash, parses the redirect's HTML body as
JSON, and surfaces `Unexpected token '<'` at the moment someone presses Bridge. The
`rewrite ^/(rpc|rest|celestia|api|evm/[a-z]+)$ /$1/ last;` line handles it internally.

## 10. Check it

```sh
B=http://<host>:3000
curl -s -o /dev/null -w '%{http_code}\n' $B/                       # UI            200
curl -s $B/rpc/status | jq -r .result.node_info.network            # chain id
curl -s -X POST $B/rpc -d '{"jsonrpc":"2.0","id":1,"method":"status"}' \
     -H 'content-type: application/json' | head -c 40              # no slash, must be JSON
curl -s "$B/rest/cosmos/bank/v1beta1/balances/<addr>?"             # REST
curl -s -o /dev/null -w '%{http_code}\n' $B/api/health             # attestation API 200
for c in sepolia arbitrum base; do curl -s -X POST $B/evm/$c/ \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'; done
curl -s -o /dev/null -w '%{http_code}\n' http://<host>:3001/       # relayer dashboard
curl -s -o /dev/null -w '%{http_code}\n' http://<host>:3002/       # gas oracle dashboard
curl -s http://<host>:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'
```

The last one is the real check: every route must report a height and a state root. A route
showing none is usually the `celestia-appd` PATH problem above.

Then bridge 0.1 TIA from Celestia and watch it land. Celestia to an EVM chain is under half a
minute; the reverse waits on origin finality.

## Keplr

Keplr cannot add a custom chain from its own settings. Open the UI and connect - it calls
`experimentalSuggestChain` with the values from `.env.local`. Import the mnemonic from
`.state/mnemonic`; account 0 is the funded one.
