# DEPLOY

Standing up a complete TEE ISM bridge from nothing: a Celestia chain, three enclaves, eight
routes across four networks, a UI, and the services that keep it running.

Read [MAINTAIN.md](MAINTAIN.md) for keeping it alive and [INTERACT.md](INTERACT.md) for using
it. Those three files are the whole documentation set.

Everything here has been done end to end on the host called `ark`. Where a step has a
non-obvious failure mode, it is called out inline rather than left for you to discover.

---

## 0. What you need

**On the deployment host:**

```
docker + compose      the chain and the gateway
go 1.26+              celestia-appd and the teeism helpers
rust + cargo          the coprocessor and the gas oracle
foundry (forge, cast) contracts; the oracle and relayer also shell out to cast
node + npm            the UI
python3, jq, curl     the scripts
phala                 the CVMs. `phala status` must show you logged in
nix                   only if you rebuild the enclave image
```

**Accounts and funds:**

| | for | needs |
|---|---|---|
| EVM key | deploys and relays on all three EVM chains | Sepolia, Arbitrum Sepolia and Base Sepolia ETH |
| Celestia mnemonic | genesis accounts, the relayer, the oracle | nothing; the chain is yours |
| Phala account | three CVMs at $0.0608/hr each, $4.38/day | a funded Phala balance |

**Ports.** One forwarded port is enough. `ark` exposes 3000, 3001 and 3002 and nothing else;
26657, 1317 and 9090 are deliberately unreachable, which is why the chain is served through
the gateway. Even on a host that forwards everything, the gateway is still how the UI and
Keplr reach the chain on a single origin.

---

## 1. Where every config lives, and what goes in it

Nothing here is created by a clone. Three files start as a template you copy and edit, two
more you write from scratch, and everything else a script writes for you.

```sh
cp devnet/.env.example             devnet/.env                      # every secret, one file
cp deploy/coprocessor.toml.example devnet/.state/coprocessor.toml   # the eight routes
cp deploy/gas-oracle.toml.example  devnet/.state/gas-oracle.toml    # paymaster upkeep
chmod 600 devnet/.env
```

The two `.state/` copies can wait until step 2 has created that directory. Only `devnet/.env`
is needed before anything else runs.

| you fill in | at | holds |
|---|---|---|
| `devnet/.env` | step 2 | every secret and metered key. One value is required, three are optional |
| `devnet/.state/coprocessor.toml` | step 12 | the eight routes. Every ISM and router address |
| `devnet/.state/gas-oracle.toml` | step 11 | `igp_id` and `evm_key_file` |
| `bridge-app/.env.local` | step 14 | every address the UI shows |
| `deploy/docker-compose.<family>.yml` | step 5, only if you rebuild an enclave | that family's image digest. **Measured** |

| written for you | by | holds |
|---|---|---|
| `devnet/.state/out/*` | the numbered scripts | one deployed id per file |
| `devnet/.state/out/pccs-<chain>.json` | step 8, or copied from another host | the Automata addresses per EVM chain |
| `devnet/.state/bin/*` | step 3 | `celestia-appd`, `teeism-collateral`, `teeism-identity` |
| `devnet/.state/celestia/` | step 4 | chain data and the keyring |
| `/etc/systemd/system/teeism-*.service` | step 13 | copied from [server/](server/) |

`devnet/.env` sits outside `.state/` deliberately, because `make stop` deletes that directory.
Both are gitignored, so nothing here is committable; tracked files carry placeholders like
`ALCHEMY_KEY` instead of values.

**`.state/out/` is the source of truth between steps.** Each file holds one id. Later scripts
and the relayer read them back rather than re-parsing transaction logs, so if a step fails
halfway you can inspect exactly what got deployed.

---

## 2. Sources and secrets

```sh
git clone -b jonas/tee-ism git@github.com:jonas089/tee-hyperlane.git       ~/tee-ism-nonzk
git clone -b jonas/tee-ism https://github.com/celestiaorg/celestia-app.git ~/celestia-app-teeism
```

`jonas/tee-ism` is the branch on both. The unit files hardcode a user named `chef` and those
two paths; change either and change the units too.

```sh
cd ~/tee-ism-nonzk
mkdir -p devnet/.state && chmod 700 devnet/.state
cp devnet/.env.example devnet/.env && chmod 600 devnet/.env
$EDITOR devnet/.env
```

Only `EVM_PRIVATE_KEY` is required. Leave `CELESTIA_MNEMONIC` empty on a first deploy and step
4 generates one and writes it back into the same file. `ALCHEMY_API_KEY` and `ALCHEMY_BASE_KEY`
are wanted by two routes each; the file says which and why.

> **`EVM_PRIVATE_KEY` must be this deployment's alone.** It pays for deploys *and* signs every
> relayer submission, so two deployments sharing one key put two relayers in a nonce race on
> every EVM chain they have in common. Both then see `nonce too low` and `replacement
> transaction underpriced` at random, on routes that are otherwise healthy. Generate a fresh
> one with `cast wallet new` and fund it; do not copy the key from a host that is still
> running.

`devnet/.env` survives `make stop`, which is the point of keeping it out of `.state/`.

> **Upgrading a host that predates this file**, and still has `.state/evm-key`,
> `.state/mnemonic`, `.state/alchemy-key`, `.state/alchemy-base-key` or `.state/relayer.env`:
> nothing reads those any more. Copy their values into `devnet/.env`, point the relayer unit
> at it, restart, and only then delete them.

**This repository is public.** An Alchemy key was committed to it once and reached `main`,
where it was scraped. Install the guard before you do anything else:

```sh
deploy/check-secrets.sh                                   # scan tracked files
ln -s ../../deploy/check-secrets.sh .git/hooks/pre-commit # refuse the commit instead
```

Rotating is the only fix for a key already pushed. Removing it from the tip leaves it in
history and does nothing about whoever already has it.

---

## 3. Build

```sh
cd ~/celestia-app-teeism
docker build -t celestia-app-teeism:local -f docker/standalone.Dockerfile .
mkdir -p ~/tee-ism-nonzk/devnet/.state/bin
go build -o ~/tee-ism-nonzk/devnet/.state/bin/celestia-appd     ./cmd/celestia-appd
go build -o ~/tee-ism-nonzk/devnet/.state/bin/teeism-collateral ./x/teeism/cmd/teeism-collateral
go build -o ~/tee-ism-nonzk/devnet/.state/bin/teeism-identity   ./x/teeism/cmd/teeism-identity

cd ~/tee-ism-nonzk/tee-hyperlane
cargo build --release -p tee-coprocessor -p gas-oracle
```

> The cargo build needs **Go on PATH** for a cgo dependency. Without it it fails with
> `Failed to build Go library`, naming neither Go nor the crate.

> The chain image carries no version or commit. `celestia-appd version --long` reports empty
> strings, so an image cannot tell you which commit built it. Record the commit yourself, or
> add `-ldflags` before you ship a second one.

---

## 4. The chain

```sh
cd ~/tee-ism-nonzk/devnet
set -a; . .env; set +a       # CELESTIA_MNEMONIC, and everything else, from the one file
STATE_DIR=../.state CELESTIA_IMAGE=celestia-app-teeism:local CHAINID=teeism-local \
CELESTIA_UID="$(id -u):$(id -g)" DEVNET_MNEMONIC="$CELESTIA_MNEMONIC" \
  docker compose -f celestia/docker-compose.yml up -d
```

> **On a first deploy, run `./scripts/10-celestia-up.sh` instead of the compose command
> above.** It generates the genesis mnemonic, writes it back into `devnet/.env`, builds the
> host binaries, and brings the chain up the same way. The raw form is here for when you need
> to vary something in it.

> **`CELESTIA_UID` is not optional on Linux.** The home directory is a bind mount and the
> image runs as uid 10001. Without it the chain dies immediately with `permission denied`
> creating its data directory. Docker Desktop hides this, so it does not reproduce on macOS.

Confirm pruning is off before anything depends on it:

```sh
grep -E '^pruning|^min-retain-blocks' .state/celestia/config/app.toml
# pruning = "nothing"   min-retain-blocks = 0
```

> An ISM only advances when a batch is delivered, so a quiet route's trusted height falls
> behind the head. The light client proves forward from that height and needs the commit at
> it. With the default 3000-block retention that commit is gone in fifty minutes and the route
> cannot be recovered: the ISM has to be redeployed. This wedged all three Celestia-origin
> ISMs once.

---

## 5. The enclave image, and pinning it

The image digest is the root of the whole identity chain:

```
image digest -> app_compose document -> sha256 = compose_hash
             -> mr_config_id in the TDX quote -> identity pinned by every ISM
```

There are three images, one per origin family, so that a change to one origin does not
re-deploy the ISMs of the others:

```sh
nix build .#image-celestia    # the Celestia origin; the EVM ISMs pin it
nix build .#image-ethereum    # Sepolia, Arbitrum and Base origins
nix build .#image-evolve      # Eden: a mocha light client and the ev-reth executor
```

Load, tag and push each, then pin its digest in `deploy/docker-compose.<family>.yml`. Those
three compose files are what the three CVMs are deployed from, and their hashes are the three
identities.

Adding a family is four small things: the `families` list in `flake.nix`, a cargo feature in
`crates/tee-node/Cargo.toml`, a module under `crates/tee-node/src/origins/`, and a compose
file. Nothing else in the build is per-family.

---

## 6. The three Phala CVMs

```sh
for f in celestia ethereum evolve; do
  phala deploy --name teeism-$f --compose deploy/docker-compose.$f.yml \
    --instance-type tdx.small --node-id 18 --image dstack-0.5.9 --no-dev-os --wait --json
done
```

Or `devnet/scripts/30-enclave-up.sh`, which does the same thing and records the urls and
identities where the later steps read them.

One CVM per origin family, each measuring its own compose file, so each has its own identity.
That is the whole point of the split: an ISM pins the identity of the enclave that attests
*its* origin, so changing Eden's code moves the evolve identity and leaves the other two
untouched. `app-id` and `instance-id` differ between instances and are deliberately not
pinned, which is what makes replacing a CVM a swap rather than a migration.

Three ways this goes wrong, all of them silent:

> **`--node-id 18`.** Auto-selection sometimes lands on prod5, whose teepod reports
> `tproxy_base_domain: None`. The CVM runs, the gateway never registers it, and every request
> terminates TLS and then returns nothing. Node 26 is prod5; node 18 is prod9.

> **`--no-dev-os`.** If the CLI finds an SSH public key on the machine you deploy from it
> silently provisions `dstack-dev-0.5.9`, which permits shell access into the CVM and makes
> the measurements meaningless. It reports `os_image_hash de9c74f0…` instead of `bd369a8c…`.

> **Deploy fresh, never `phala cvms upgrade`.** The measured app-compose document carries a
> `name` field. A fresh deploy leaves it empty; an upgrade rewrites it to `app_<app_id>`,
> which is per-instance, so an upgraded enclave measures a compose hash no ISM pins. It cannot
> be undone in place, because the field derives from the app id.

Record the urls and identities, one file per family:

```sh
cd ~/tee-ism-nonzk/devnet
for f in celestia ethereum evolve; do
  printf '%s' "https://<$f-app-id>-8080.dstack-pha-prod9.phala.network" > .state/out/enclave-url-$f
  printf '%s' "<$f-app-id>" > .state/out/enclave-app-id-$f
done
```

Then check what each one measures:

```sh
for a in <celestia-app-id> <ethereum-app-id> <evolve-app-id>; do
  curl -s https://$a-8080.dstack-pha-prod9.phala.network/identity | jq -S .
done
```

`mr_td`, `os_image_hash` and `mr_kms` are the same for all three, because they share an OS, a
KMS and a base image. `compose_hash` is where they diverge, and that is what carries the
per-family image digest into the identity. The live deployment measures:

```
mr_td          f06dfda6dce1cf904d4e2bab1dc370634cf95cefa2ceb2de2eee127c9382698090d7a4a13e14c536ec6c9c3c8fa87077
os_image_hash  bd369a8c2f9edb2b52dad48ac8e0b32dde5f1337c423a506b48d07403a7d8033
mr_kms         92a4bf40c88734b0e56f54b09b1f0fe4b8d3e230047e9298f491968ada8dedf8

                celestia                                                           ethereum                                                           evolve
compose_hash    c502ed6b59d5b9fbcf2898e986a8fdb2387043c5f3ed54d31c3f0f7b7101d6f7   928dc64af2dbef87fedbf30cf4e291c76ca194fdd17028eace723d744b108137   e73f304585d61c3bd21643eb37a17b78667377facfd5f5a352b320bca0cc7704
identity        0x26ba429fdd51a3131520393c09a033423c2ec715a03094288f6d25ee55fdb66f 0xfe294574ecdc4f20d23226ed475ba711e08c23edbbc83365781cda29a5101bc3 0x259d450e50a8374a42b6a0e514cb0e23cb3ae1a40f962ca31db214b98ebac91e
measurements    0x8ec698fab68fd8d049fd04c302ac33908dc019ece98c65a2ee4a814a81c08542 0x4d87b27e1795b4f0f90be32d912fa11ff3b16f53f31ca8f4096e4502c283daf3 0x3d66f22ffbffcaa2007a61f867e7d929184bf074ba62829804bde8f4faa32575
```

The celestia family attests the Celestia origin, so the four EVM-side ISMs pin it. The
ethereum family attests Sepolia, Arbitrum and Base; the evolve family attests Eden.

`measurements` is what the EVM ISMs pin: `keccak(mr_td ++ mr_config_id ++ rtmr0..2)`. rtmr3 is
excluded because it carries the app id and instance id. `identity` is the same commitment in
the form `x/teeism` stores, which compares the five fields individually so a rejection names
the one that diverged.

---

## 7. Hyperlane core on the chain

```sh
./scripts/20-celestia-hyperlane.sh     # mailbox, merkle tree hook, noop hook
```

On the live chain those are:

```
chain id   teeism-local
domain     1297040299
mailbox    0x68797065726c616e650000000000000000000000000000000000000000000000
merkle     0x726f757465725f706f73745f6469737061746368000000030000000000000000
igp        0x726f757465725f706f73745f6469737061746368000000040000000000000002
```

Ids are minted at genesis, so they are stable only as long as the chain is. A fresh genesis
mints fresh ids and everything downstream must be redeployed.

---

## 8. The Automata DCAP stack, per EVM chain

Each EVM chain verifies TDX quotes through **our own** deployment of Automata's contracts.
Not for security reasons: writes to their FMSPC TCB DAO are gated on an `ATTESTER_ROLE` they
hold, so we could never publish the TCB record for our platform on chains that lack it.
Owning the deployment is the only way to publish our own collateral without waiting on a third
party. Their contracts are used unmodified; the only source change is `Salt.sol`, to
domain-separate the CREATE2 salts.

If you are deploying to the three chains already supported, copy the records instead:

```sh
cp <from the previous host>/pccs-*.json .state/out/
```

> `80-evm-isms.sh` prints `no PCCS on <chain>; skipping` and deploys nothing if these are
> missing. It does not fail.

Current addresses, identical on all three chains because CREATE2 through a shared deployer
makes the address a function of salt and bytecode alone:

```
AttestationEntrypoint   0x961D4408f512D4a169bD76433460d2981a70c71F
PCCSRouter              0xdA7336571D634bE002035Af6ec55F0816A2Ed263
V4QuoteVerifier         0xFFd8Ddff9b7e9ce124A7fdddcd817bA4d8B37ab7
PcsDao                  0x3c3fF9105e62228c7dA62C3bA04d24D320c4433C
EnclaveIdentityDao_20   0x426B9aC0e424dEcC66e4C3a7d9293839e16D8fc1
TcbEvalDao              0x03b1B658C34Bb7919A9cA2067d0055f7dD5C5495
DaoStorage              0xCFe415d68Ef55407B1cBA73C4bC267B28d916643
DaoStorageV2            0x9137457c28Ffef88E9B06BDeeC61a362FDC71651

FmspcTcbDao_20          0x7BDA83918CAAD9b5EC7F88A24660167E90053690   sepolia, arbitrum
                        0x06D080A8642803465500D6C9004Cc9CF48094EeD   base
```

Base differs because the versioned deploy had already placed the EnclaveIdentity DAO there,
and that script deploys both DAOs in one transaction: the collision on the first reverted the
second with it. It was redeployed by hand with plain CREATE.

### Bringing up a fourth EVM chain

Automata's own `new-network.sh` is not sufficient. The working order:

1. `make deploy-helpers`, then `make deploy-dao`
2. `forge script script/automata/DeployCrlV2.s.sol` - deploys `PccsDependencyConfig`, which
   every versioned DAO depends on and which their sequence never deploys
3. `deploy_versioned.sh` for `storage-v2`, `tcb-eval`, `versioned 20`, `fmspc-v2 20`
4. `grantRoles(deployer, 1)` on each versioned DAO and on the TcbEvalDao
5. Copy the PCCS record into the attestation repo's registry at
   `rust-crates/libraries/network-registry/deployment/current/<chain-id>/onchain_pccs.json`,
   aliasing the plain DAO names to the CrlV2 ones the deploy actually produced
6. `DeployRouter`, then `deployEntrypoint()`, then `DeployVerifier`. The verifier registers
   itself on whatever entrypoint `dcap.json` names, so point that at ours first or the whole
   broadcast reverts and nothing is deployed
7. `setQeIdDaoVersionedAddr(20, …)` and `setFmspcTcbDaoVersionedAddr(20, …)` on the router
8. `setCallerAuthorization(router, true)` on **both** storage contracts, or every read reverts
9. `devnet/scripts/seed-evm-collateral.sh <chain>`

Things that will otherwise cost you an afternoon:

- Without domain-separated salts, CREATE2 reproduces Automata's live addresses and collides.
  The repo also ships their deployment records pre-populated, so an unmodified run silently
  produces a stack that is half theirs.
- `SKIP_ESTIMATE` must be the string `true`. Anything else still runs estimation.
- Their `config_versioned.sh` reports `granted` while granting nothing on the V2 DAO.
- `cast` cannot encode a tuple containing a JSON string. QE identity, TCB info and evaluation
  numbers all need hand-built calldata.
- Sending transactions back to back races the node's nonce and returns `replacement
  transaction underpriced`, which looks exactly like a rejected upsert.

`owner` and `ATTESTER_ROLE` on every contract are the deploy key, which can repoint the router
at a different DAO. That is the strongest privilege anywhere in the EVM path, and it is also
the key that pays gas. Split them before this carries value.

---

## 9. The ISMs

An ISM pins **exactly one origin domain** and one enclave identity. Eight routes therefore
need eight ISMs, four on each side.

```sh
./scripts/80-evm-isms.sh
./scripts/85-celestia-isms.sh
```

`80-evm-isms.sh` deploys `TeeDcapIsm` on each EVM chain from `pccs-<chain>.json` plus the
identity of the family named by `ENCLAVE_FAMILY` (default `celestia`, which is right for
every Celestia-origin route).

> **Bringing up a subset.** `80-evm-isms.sh` and `90-evm-warp.sh` take `CHAINS`, and
> `85-celestia-isms.sh` takes `ORIGINS`. Set all three or none: an origin with an ISM from 85
> but no enrolled router from 90 gives you a route that attests cleanly and then fails every
> delivery with `no enrolled router found for origin <domain>`, forever, with nothing in the
> deploy output to say why.
>
> ```sh
> S="sepolia:11155111:0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"
> CHAINS="$S"  ./scripts/80-evm-isms.sh
> ORIGINS="sepolia:11155111:ethereum:0x0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d" \
>   ./scripts/85-celestia-isms.sh
> CHAINS="$S"  ./scripts/90-evm-warp.sh
> ```

`85-celestia-isms.sh` creates all four Celestia-side ISMs and the routing ISM that fans them
out, then points the mailbox and the warp tokens at it. It knows which family each origin
belongs to, so Sepolia, Arbitrum and Base pin the ethereum identity while Eden pins the
evolve one. Each reads a live checkpoint from its origin; Eden's uses `bootstrap-eden`
(step 11c). Set `ISM_GENESIS_<ORIGIN>` to anchor one at an older checkpoint instead, which is
the only way to carry in-flight messages across a redeployment. Live values:

| origin | ISM on `teeism-local` |
|---|---|
| Sepolia `11155111` | `0x726f757465725f69736d000000000000000000000000002b0000000000000031` |
| Arbitrum `421614` | `0x726f757465725f69736d000000000000000000000000002b0000000000000032` |
| Base `84532` | `0x726f757465725f69736d000000000000000000000000002b0000000000000033` |
| Eden `3735928814` | `0x726f757465725f69736d000000000000000000000000002b0000000000000034` |

| destination | `TeeDcapIsm` |
|---|---|
| Ethereum Sepolia | `0x5F6F12f4bA71417e9d4b782805CB7c7EFA8eBfB9` |
| Arbitrum Sepolia | `0x6104637C585a875402bF227e88c1ef281BA01E7b` |
| Base Sepolia | `0x1760b447664D270c594750d0E38b3fE38a156b50` |
| Eden | `0xC9287d04225493966ceA9A0f49D16D317D7920c5` |

### One ISM is not enough on the Celestia side

Four EVM origins deliver into one Celestia token, and each ISM pins one `origin_domain`, so a
single ISM rejects three of the four. A routing ISM fans them out:

```sh
celestia-appd tx hyperlane ism create-routing
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 11155111 $SEPOLIA_ISM
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 421614   $ARBITRUM_ISM
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 84532    $BASE_ISM
celestia-appd tx hyperlane ism set-routing-ism-domain $ROUTING 3735928814 $EDEN_ISM
celestia-appd tx warp set-token $TOKEN --ism-id $ROUTING
celestia-appd tx hyperlane mailbox set $MAILBOX --default-ism $ROUTING
```

Live: `0x726f757465725f69736d00000000000000000000000000010000000000000035`. It is both the
token's ISM and the mailbox default.

> Rotating a route is **remove then set**, not set. `set-routing-ism-domain` inserts a domain
> that is absent and silently leaves an existing one alone. The transaction succeeds, emits
> `EventSetRoutingIsmDomain` naming the new ISM, and changes nothing.

The Celestia-to-EVM direction needs no routing ISM, because there each destination has its
own.

---

## 10. Warp routes

```sh
./scripts/50-warp-celestia.sh    # the Celestia side of every asset
./scripts/90-evm-warp.sh         # the EVM side, and both enrolment directions
```

Two assets ship, deliberately pointing opposite ways so that lock/unlock and mint/burn are
both exercised:

| | home chain, collateral | synthetic elsewhere |
|---|---|---|
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE`<br>Arbitrum `0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE`<br>Base `0xf4197C55C944987E9b10e09C0A47915211769B78` |
| USDC | Sepolia `0xfb611B6f6CE92033960e99C2D65cee4237e64cDD`, wrapping Circle's `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` | Celestia `0x726f757465725f61707000000000000000000000000000020000000000000001`<br>Arbitrum `0xb9E5E3eb926EA22B951d2fb7392F9F3D6c704054`<br>Base `0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B` |

On Celestia a synthetic is a bank denom named `hyperlane/<token id>`. Sending from a
collateral router needs an ERC20 `approve` first; sending a synthetic needs none, because it
is burned rather than transferred.

Every route runs through Celestia. No EVM chain's ISM trusts another EVM chain, so an
EVM-to-EVM pair is two hops.

### Adding your own asset

This is the easy path, and nothing about the enclave or the ISMs changes: **ISMs are per
origin chain, not per token.** No new ISMs, no new measurements, no Phala redeploy, and no gas
oracle change either, because the oracle keys on chains rather than assets.

1. Add a row to `TOKENS` in `90-evm-warp.sh` and, if the asset is native to an EVM chain, an
   arm to `kind_for`. Add the Celestia side to `50-warp-celestia.sh`.
2. Re-run both scripts. They are idempotent and will repoint an enrolment that already exists,
   because the chain refuses to enroll a domain twice.
3. Point each EVM router at that chain's ISM and set the same aggregation hook the TIA router
   uses. **Skip the hook and the IGP quotes zero.**
4. Add both new routers to `routers` on both directions of each affected route in
   `coprocessor.toml` (step 12).
5. Add the `VITE_*` values to `bridge-app/.env.local` (step 14) and rebuild.
6. Fund the collateral side.

Done by hand rather than by script, the two-command form for an asset whose EVM router already
exists is:

```sh
$APPD tx warp create-synthetic-token $(cat .state/out/mailbox-id) $TX
$APPD tx warp set-token $TOKEN --ism-id $(cat .state/out/routing-ism-id) $TX
$APPD tx warp enroll-remote-router $TOKEN 11155111 <router, 32 bytes> 50000 $TX

cast send $ROUTER "setInterchainSecurityModule(address)" $(cat .state/out/ism-sepolia)
cast send $ROUTER "enrollRemoteRouter(uint32,bytes32)" 1297040299 $TOKEN
cast send $ROUTER "setHook(address)" <the same aggregation hook the TIA router uses>
```

> The router must be the **32-byte** form on the Celestia side. The CLI rejects a bare 20-byte
> address rather than padding it.

---

## 11. Paymaster and gas oracle

Create the IGP, derive and fund the `bridge` key the oracle signs with, transfer IGP ownership
to it, then set the mailbox hooks: `--required-hook` the merkle tree hook, `--default-hook` the
IGP.

> On the EVM side each router's hook must be a `TreeAndPaymasterHook`, **not** the IGP
> directly. A router picks exactly one post-dispatch hook and this bridge needs two: the merkle
> tree hook, or the message is never inserted and can never be attested, and the paymaster.
> Pointing a router straight at the IGP costs it the first, silently. The transfer succeeds and
> the message is unprovable forever.

> On Celestia no aggregation is needed, because `required_hook` is already the merkle tree
> hook, so `default_hook` can be the IGP and both run.

> `20-celestia-hyperlane.sh` wires it the other way round, `--default-hook` the merkle tree
> hook and `--required-hook` the noop hook, because a devnet has no IGP yet. Either order runs
> the merkle tree hook, which is the part attestation depends on. Only redo it as described
> here once the IGP exists.

Register this deployment's Celestia domain on each EVM IGP with `setDestinationGasConfigs`.
This is manual and the oracle service does not do it; without it `transferRemote` reverts with
`IGP: no gas oracle for domain 1297040299`.

> Do not fall back to Hyperlane's default. Its Sepolia default quotes **0.009 ETH** for
> Celestia's domain, about $22 a transfer, because the domain is not in its oracle. Ours quotes
> $0.0014.

Live paymasters:

| chain | paymaster | oracle |
|---|---|---|
| teeism-local | `0x726f757465725f706f73745f6469737061746368000000040000000000000002` | in-module |
| Sepolia | `0x48b1BF6CC2e45Ca52947E95Bb216C2eBdCB19c49` | `0x225B8488242c90085B7A8Ea33Ce8e39Ae9f79722` |
| Arbitrum Sepolia | `0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B` | `0xfA8036Cb092079B095ed60750d7b39c3C220F288` |
| Base Sepolia | `0x5591613C85E9bC95104980d4485c958ee80f6F76` | `0x7A7042C8784700618be87Aac7F9336620e216Bb9` |

Write `.state/gas-oracle.toml` from [gas-oracle.toml.example](gas-oracle.toml.example),
adjusting `igp_id` and `evm_key_file`. It keys on `[[destinations]]` by domain and
`[[evm_origins]]` by chain; nothing in it is per-asset.

```sh
gas-oracle --config .state/gas-oracle.toml --once   # one round, printed, then exit
```

---

## 11b. The faucet

The UI's Faucet tab grants a fixed 1000 TIA per address, once. It signs with `celestia-appd`
from its own account, deliberately not the relayer's: this is the one endpoint a stranger can
spend from, so what it can give away should be all it can reach. Draining it stops the faucet
and nothing else.

`10-celestia-up.sh` derives a `faucet` key at account **4** and funds it at genesis with
`FAUCET_COINS`, a thousand grants by default. Account 3 is reserved for the oracle's `bridge`
key, which the paymaster setup derives by hand; two names on one index fail with
`duplicated address created` at whichever runs second.

On a chain that already exists, derive and fund it instead:

```sh
A=.state/bin/celestia-appd; H="--home .state/celestia --keyring-backend test"
printf '%s\n' "$(cat .state/mnemonic)" | $A keys add faucet --recover --account 4 $H --output json
$A tx bank send validator "$($A keys show faucet -a $H)" 1000000000000utia $H \
  --chain-id teeism-local --node http://localhost:26657 --fees 200000utia --gas 200000 -y
```

> `keys add` takes `--output json`, not `-o json`. `-o` is rejected as an unknown shorthand.

The endpoint lives in the attestation API, so `teeism-api.service` needs a keyring to sign
with. Without `CELHOME` the endpoint reports itself unconfigured and the UI hides the tab
rather than offering a button that cannot work:

```
Environment=APPD=/home/chef/tee-ism-nonzk/devnet/.state/bin/celestia-appd
Environment=CELHOME=/home/chef/tee-ism-nonzk/devnet/.state/celestia
Environment=CELESTIA_CHAIN_ID=teeism-local
Environment=CELESTIA_RPC=http://localhost:26657
Environment=FAUCET_KEY=faucet
```

Claims are recorded under `<proof_dir>/.faucet/<address>`, one file each, created before the
send so two racing requests cannot both be paid. A failed send removes the marker so the
address can try again. Deleting the directory re-opens every claim.

Addresses are free to mint, so one-per-address bounds a careless user rather than a
determined one. The real bound is the funding account's balance.

---

## 11c. Eden, an evolve-stack chain

Eden differs from the other three in two ways that matter. It has **no canonical Hyperlane
deployment**, so its mailbox and merkle tree hook are ours. And as an *origin* it has no
consensus to run a light client against, so its state root is found in a sequencer-signed
header published to Celestia and then **re-executed by the enclave** before it is believed.
See MAINTAIN.md for what that does and does not buy.

```
chain id        3735928814 (0xdeadbfee)   ~10 blocks/s, 18 decimals
DA              mocha-5, namespace 0000000000000000000000000000000000005d2e074163aa3b4d9818
sequencer       ed25519 4366433b4309d4f077f0cc1f4370a525736df9a1dc9a205b8d2db1d630b68d51
chain id string edennet-2
```

The namespace and key are pinned in `origins/celestia_l2.rs`, not configured, for the same
reason the L2 anchors are. Neither came from a spec sheet: Eden's blocks are empty so its
state root is constant, and using that as a needle found a Celestia blob carrying 657 Eden
headers that all verify under this key.

**An evolve origin needs a DA node.** The consensus RPC gives a `data_hash` but not the row
roots behind it, so a celestia-node light node for mocha runs beside the chain:

```sh
IMG=ghcr.io/celestiaorg/celestia-node:v0.34.2-mocha
D=$PWD/.state/mocha-light && mkdir -p "$D"      # from devnet/
docker run --rm -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG celestia light init --p2p.network mocha
# `init` writes a config `start` then rejects. Both need fixing by hand:
#   add   [Share.LightAvailability] / SampleAmount = 16
#   set   Header.Syncer.PruningWindow = "800h0m0s"   (must be >= the 721h sampling window)
docker run -d --name mocha-light --restart unless-stopped -p 127.0.0.1:26658:26658 \
  -v $D:/home/celestia -u "$(id -u):$(id -g)" $IMG \
  celestia light start --p2p.network mocha --rpc.addr 0.0.0.0 --rpc.port 26658 --rpc.skip-auth
```

> Use a **-mocha** tagged release. `v0.26.0-arabica` speaks `/mocha-4/` protocol ids and can
> never sync mocha-5; it fails with "protocols not supported" and looks like a peering
> problem.

**Eden's own RPC serves `eth_getProof` for the `latest` tag only.** Not a pruning window: a
numbered block is refused even at head-minus-zero, because ten blocks a second means the
block has moved on before the request lands. The relayer therefore captures a proof each tick
and files it under its height, then attests once Celestia carries the signed header for that
same height. Nothing to configure; it is how `attest_eden` works. `bootstrap-eden` does the
same, anchoring only at a height it managed to capture.

```sh
tee-hyperlane bootstrap-eden --rpc https://rpc-mocha.pops.one \
  --da-rpc http://localhost:26658 --identity-digest <digest> \
  --out .state/proofs/eden-to-celestia/staging/attestation.json
```

**`l2_rpc` must serve `debug_executionWitness`.** That is what the enclave re-executes
against, and it is the one field on this route that a plain public endpoint will not answer.
Unlike `eth_getProof` it is served for historical blocks, so no capture-ahead is needed for
it. The relayer finds the blocks that changed Eden's state by bisecting on the state root
between the trusted height and the target, so a quiet stretch costs a handful of
`eth_getBlockByNumber` calls rather than one per block.

> A route that falls a long way behind will report that a span is "too long to re-execute in
> one step". That is not a stall: it steps forward through the backlog one attestation at a
> time, taking the newest height whose re-execution fits.

The rest is ordinary: the Automata stack from step 8, Hyperlane core from `DeployHyperlaneCore`
then `InitHyperlaneCore`, an ISM, and the two synthetic routers.

> Eden's TCB info only lands in the **V1** versioned FMSPC DAO; the V2 one reverts
> `0x331b9eaa` with the same calldata. Point `FmspcTcbDaoVersioned` and the router at V1
> there. The other three chains use V2.

> The Automata repos need submodules that are not vendored here: `forge-std`, `solady` and
> `openzeppelin-contracts` **pinned to v5.0.2**, since master needs Cancun while the project
> compiles for paris. The attestation repo additionally wants `risc0-ethereum`,
> `sp1-contracts`, and the pccs repo symlinked in as `lib/automata-on-chain-pccs`.

---

## 12. The relayer config

Write `.state/coprocessor.toml` from [coprocessor.toml.example](coprocessor.toml.example),
replacing every ISM and router address with this deployment's.

> **Only if you run the relayer under systemd, as step 13 does.** `make start` calls
> `60-start.sh`, which *generates* this same file from `.state/out/` on every run and
> overwrites whatever is there. On a devnet, let it; hand-editing is for the deployed host,
> where nothing regenerates it.

Per route, the fields that matter:

| field | meaning |
|---|---|
| `name` | route id, used in logs and by the dashboard |
| `tee_node_url` | which CVM serves this origin |
| `ism_id` | the destination ISM. An address on EVM, a 32-byte id on Celestia |
| `merkle_tree_address` | the **origin's** Hyperlane merkle tree hook |
| `attest_only = true` | selects the direct submit path. Without it the route uses the proof-carrying script and fails on a missing vkey |
| `routers` | which recipients trigger a batch early. See below |

**`routers` is the only per-asset field in this file.** Everything else is per chain pair. It
names the recipients you actually serve, so that other traffic on a shared mailbox does not
start a batch you have no message in. Get it wrong and nothing breaks: your transfers still
deliver, because merkle replay forces batch completeness. They simply wait for other traffic
to trigger a batch instead of triggering their own. Latency, never correctness. Left empty, the
route falls back to matching on destination domain, which is looser and more wasteful.

### Endpoints

No endpoint here needs an API key except Base. Verified working:

```
sepolia   execution + archive   https://rpc.sepolia.ethpandaops.io
sepolia   beacon                https://ethereum-sepolia-beacon-api.publicnode.com
arbitrum  destination           https://sepolia-rollup.arbitrum.io/rpc
arbitrum  logs                  https://arbitrum-sepolia-rpc.publicnode.com
arbitrum  l2 archive            https://api.zan.top/arb-sepolia
base      destination + logs    https://sepolia.base.org
base      l2 archive            a metered key; see below
```

> **Base origin is the one route that needs a paid archive.** It calls `eth_getProof` roughly
> 220k blocks back, the five day dispute window. Every free endpoint tried refuses with
> "distance to target block exceeds maximum permitted", and Base's own RPC does not serve the
> method at all. Put the key in `ALCHEMY_BASE_KEY` in `devnet/.env` and wire it to `base-to-celestia`'s
> `l2_rpc` **only**. A free Alchemy tier is enough: archive `eth_getProof` works, and the
> 10-block `eth_getLogs` cap does not matter because Base logs go to `sepolia.base.org`.

> **`archive_rpc` must never be a metered key.** `dispatched_messages` runs on the archive
> reader, so a large `eth_getLogs` sweep goes there. Pointing it at a rate-limited key
> exhausts the tier and backs off every route at once, including routes with nothing to do
> with Ethereum.

---

## 13. Services

```sh
sudo cp deploy/server/teeism-{relayer,api,gas-oracle}.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now teeism-relayer teeism-api teeism-gas-oracle
```

> The relayer unit reads `devnet/.env` directly as its `EnvironmentFile`, so there is no second
> copy of the key to write and keep in step. That is also why `EVM_PRIVATE_KEY` in that file
> needs its `0x`: systemd passes the value through untouched.

> Each unit puts `.state/bin` **first** on PATH. The API and the oracle shell out to
> `celestia-appd` by name, and any other build on the host will not have the teeism module.
> The symptom is `unknown command "teeism" for "query"` on every Celestia-origin route.

> The API and the oracle bind `0.0.0.0`, not localhost, or their dashboards are unreachable
> from anywhere but the host.

The relayer shells out to `cast` and `celestia-appd` to sign rather than reimplementing two
transaction formats, so both must stay on that PATH.

---

## 14. The UI and the gateway

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
VITE_EDEN_RPC=http://<host>:3000/evm/eden/
VITE_SEPOLIA_ISM=<TeeDcapIsm on sepolia>
VITE_ARBITRUM_ISM=<TeeDcapIsm on arbitrum>
VITE_BASE_ISM=<TeeDcapIsm on base>
VITE_EDEN_ISM=<TeeDcapIsm on eden>
VITE_SEPOLIA_TIA_ROUTER=<sepolia tia synthetic router>
VITE_ARBITRUM_TIA_ROUTER=<arbitrum tia synthetic router>
VITE_BASE_TIA_ROUTER=<base tia synthetic router>
VITE_EDEN_TIA_ROUTER=<eden tia synthetic router>
VITE_SEPOLIA_USDC_ROUTER=<sepolia usdc collateral router>
VITE_ARBITRUM_USDC_ROUTER=<arbitrum usdc synthetic router>
VITE_BASE_USDC_ROUTER=<base usdc synthetic router>
VITE_EDEN_USDC_ROUTER=<eden usdc synthetic router>
VITE_PROVING_SECONDS=30
ENV
VITE_DEVNET=1 npm install --silent && VITE_DEVNET=1 npm run build

cd ../devnet/gateway
UI_DIST=~/tee-ism-nonzk/bridge-app/dist docker compose up -d
```

An empty `VITE_*_ROUTER` is how a route reports itself as not deployed, so the UI hides the
control rather than offering one that cannot work. Leave one blank only if that asset
genuinely has no router on that chain.

Adding a chain to the UI is more than these values: `src/config.ts` has to gain a `CHAINS`
entry, `App.tsx` a name in `COUNTERPARTIES`, and `site.conf` an `/evm/<chain>/` proxy. Eden
also needed a `nativeCurrency`, because it pays gas in TIA rather than ETH and the default
tells MetaMask the wrong thing.

`site.conf` ships with `ALCHEMY_ETH/ARB/BASE` placeholders. Substitute them or point them at
the public endpoints, which is what this deployment does.

> A container rather than the host's nginx, deliberately: the host's nginx may serve unrelated
> sites that bind port 80, and if anything holds 80 it cannot start at all.

> Every proxied path must answer on both spellings. nginx redirects `/rpc` to `/rpc/` with a
> 301; CosmJS POSTs to the RPC root without the slash, parses the redirect's HTML body as
> JSON, and surfaces `Unexpected token '<'` the moment someone presses Bridge. The
> `rewrite ^/(rpc|rest|celestia|api|evm/[a-z]+)$ /$1/ last;` line handles it internally.

---

## 15. Adding a chain the bridge has never seen

A chain joins as an **origin** (its state is attested) or a **destination** (it stores an ISM).
Most need both. Adding one adds no new enclave logic beyond two functions:

```rust
fn get_<chain>_root(store) -> AttestedRoot            // verified head and state root
fn get_<chain>_merkle_tree(root, proof) -> MerkleTree // the origin's Hyperlane tree
```

**A new EVM rollup that settles to a chain we already attest** needs one function and no light
client, because it publishes an L2 commitment into that chain's L1 storage.

1. Find where the commitment lives. Two shapes cover most rollups. **OP Stack** stores an
   output root, `keccak(version ‖ stateRoot ‖ messagePasserStorageRoot ‖ blockHash)`, whose
   preimage contains the state root directly. **Arbitrum BoLD** stores an assertion hash whose
   preimage carries the L2 block hash, which is *not* the state root, so you also supply the L2
   block header RLP and take `header.stateRoot`.
2. Derive the slot from L1 rather than accepting it. Read `latestConfirmed` or the anchor out
   of storage and compute the mapping slot from it, so a relayer cannot point the enclave at a
   stale commitment. Watch for packed slots, and check the status: a pending assertion is still
   inside its challenge window.
3. Add the arm to `OriginInput` and the config block. Everything downstream is unchanged.

> **The anchor contract and its storage layout are compiled into the enclave, never taken from
> the request.** Anyone can deploy a contract whose storage mimics a rollup and prove it
> honestly against the real L1 state root. Putting the addresses under `compose_hash` means
> changing one is a redeploy of the whole identity, not a field in a JSON body.

> Verify a rollup is live before building against its layout. Arbitrum Sepolia is BoLD, and the
> canonical address `0xd808…81C8` is the *deprecated* pre-BoLD contract that still answers
> `latestConfirmed()` and has created no node in over eleven days. The live rollup is
> `inbox.bridge().rollup()` = `0x042B2E6C5E99d4c521bd49beeD5E99651D9B0Cf4`.

**An entirely new chain** (Solana, Move, another Cosmos) needs three things, and the rest
follows: a light client that is pure verification with no I/O and whose store fits or hashes
into the ISM's `state` field; a membership proof from the state root to Hyperlane's merkle tree
(MPT for EVM, ics23 for Cosmos, an account proof against the bank hash for Solana); and a
Hyperlane deployment whose merkle tree is the same incremental structure. Verify the last one
by reproducing a live root from raw state before trusting anything, and compare `(count, root)`
rather than the raw branch array, because implementations differ on unused branch levels.

A new destination needs a contract or module mirroring `TeeDcapIsm.sol`: store an opaque state
whose first 32 bytes are the root, verify a TDX quote against pinned measurements, authorise a
batch of message ids per root, and consume each id once.

Checklist:

- [ ] `get_<chain>_root` verifies consensus, or derives the root from a chain that does
- [ ] `get_<chain>_merkle_tree` proves the tree against that root
- [ ] a live root reproduced from raw state, in a test
- [ ] domain id registered, and carried in the ISM state so it cannot be replayed cross-origin
- [ ] `Origin` variant, `OriginInput` arm, config block
- [ ] ISM deployed with a genesis state naming the checkpoint and the current identity

---

## 16. Check it

```sh
B=http://<host>:3000
curl -s -o /dev/null -w '%{http_code}\n' $B/                       # UI              200
curl -s $B/rpc/status | jq -r .result.node_info.network            # chain id
curl -s -X POST $B/rpc -H 'content-type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"status"}' | head -c 40  # no slash, must be JSON
curl -s -o /dev/null -w '%{http_code}\n' $B/api/health             # attestation API 200
for c in sepolia arbitrum base; do curl -s -X POST $B/evm/$c/ \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'; done
curl -s http://<host>:3001/api/status | jq -r '.[] | "\(.name) \(.height)"'
```

The last one is the real check: every route must report a height and a state root. A route
showing none is usually the `celestia-appd` PATH problem in step 13.

Then bridge 0.1 TIA and watch it land. Celestia to an EVM chain is under half a minute; the
reverse waits on origin finality, and the two L2 origins wait on their dispute windows. See
[INTERACT.md](INTERACT.md) for what each route should take.

Finally, verify that what is deployed is what is in this checkout:

```sh
FAMILY=celestia deploy/verify-digest.sh <app-id> --ism <addr> --rpc <url>
```
