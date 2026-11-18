# TEE ISMs

Hyperlane bridging between Celestia and EVM chains. Messages are authorised by a **TDX enclave that verifies the origin chain**, checked on the destination via its TDX quote. No zero-knowledge proofs.

| doc | for |
|---|---|
| [deploy/DEPLOY.md](deploy/DEPLOY.md) | standing up the bridge |
| [deploy/MAINTAIN.md](deploy/MAINTAIN.md) | monthly job, rollouts, diagnosing a route |
| [deploy/ADMIN.md](deploy/ADMIN.md) | owner controls and commands, Intel breakage |
| [deploy/INTERACT.md](deploy/INTERACT.md) | wallets, sending, arrival, latency, cost |

## Layout

```
tee-hyperlane/crates/tee-node/          the enclave
tee-hyperlane/crates/tee-coprocessor/   the service: every route, the API, the faucet
tee-hyperlane/crates/hyperlane-types/   Hyperlane's message, merkle tree and delivery metadata, shared by enclave and relayer
tee-hyperlane/crates/gas-oracle/        keeps the Celestia paymaster's gas prices current
tee-hyperlane/contracts/                TeeDcapIsm.sol; Automata's PCCS and DCAP verifier are fetched at pinned commits by `teeism build`
cli/                                    teeism: deploys, rotates, inspects and tears down everything
bridge-app/                             React UI, MetaMask + Keplr
devnet/                                 the chain's and the gateway's compose files
deploy/                                 the guides, the measured compose files, systemd units
```

- `l1/`: light-client chains (Celestia, Ethereum). `l2/`: sequencer-signed rollups (Base, Arbitrum, Eden).
- Per chain: `tee-node/src/chains/l2/base.rs` (verification), `tee-coprocessor/src/origin/l2/base.rs` (fetching).
- Features mean one thing: which chains are compiled in. `celestia`, `ethereum`, `base`, `arbitrum`, `eden`, named alike in `tee-node` and `tee-coprocessor`, each coprocessor feature forwarding to the enclave's. `default` builds every chain, for development, tests and the devnet. An enclave image is built with one (`flake.nix`); a relayer with the chains it serves. The CLI's `ledger` is the one feature that is not a chain. Check every combination: `cargo hack check -p tee-coprocessor --each-feature --exclude-no-default-features`.

## Deployments

Redeployed from scratch on 2026-11-18 (fresh devnet, CVM, ISMs, routers and PCCS); every id below is from then. `teeism status` prints the live ids. Verify: `teeism enclaves verify`.

**Enclaves.** Phala Cloud (node picked by Phala), `tdx.small`, dstack `0.6.4`, image `ghcr.io/jonas089/tee-node`. All five in one CVM (`deploy/docker-compose.all.yml`); `teeism enclaves up --chain <chain>` runs one alone.

| enclave | port | attests |
|---|---|---|
| `celestia` | 8080 | Celestia |
| `ethereum` | 8081 | Sepolia |
| `base` | 8082 | Base Sepolia |
| `arbitrum` | 8083 | Arbitrum Sepolia |
| `eden` | 8084 | Eden |

```
app id         62646bc710e60574a073e6a690e9fe504e5dd1d9
urls           https://62646bc710e60574a073e6a690e9fe504e5dd1d9-<port>.dstack-pha1-prod9.phala.network
mr_td          0x2a674327c50218dba880066b349b8d559d749ed68dce33fd651c184a877d084b07a9e583767a7ad5da13ed91deec2b70
compose_hash   0xdd5b22deda6bcd8e621ab3b80691691e24bd06d1788f299f31d7814fd251db60
os_image_hash  0xfeff3645893ad910d4f0e06d8f553cf80a7c9fede2ce0698fa63116283260366
rtmr0          0x68102e7b524af310f7b7d426ce75481e36c40f5d513a9009c046e9d37e31551f0134d954b496a3357fd61d03f07ffe96
rtmr1          0x0d390cdff163d981adbe90bb1d3666463be9de93a83de9b09ad0fe233974fdddc475731ba40d0bdc523efe8887cf82b8
rtmr2          0xdf939336955fec7a8d316b4ee4fe7b7e0810194c6911aa61c559882ad17984bfb4a7fc7c3112d25a4968f23bd6b907be
identity       0xa03fe78c186b5badc7cd2d5d52540a877470bd9e959c898d434d1d3ac03274c3
measurements   0xf5dda62549cd9fc7465b45e7aef8d341b7164577bdae11f1cba38f8dd7e86561
```

A new CVM is always a re-pin, even on an unchanged image: dstack's app compose carries the CVM's name and the image digests, so `compose_hash` moves.

Image digests per chain: `deploy/images.lock`.

EVM ISMs pin `measurements`; Celestia ISMs pin the fields behind `identity`.

**Chain.** Own devnet (not mocha), pruning off.

```
chain id   teeism-local
domain     1297040299
mailbox    0x68797065726c616e650000000000000000000000000000000000000000000000
merkle     0x726f757465725f706f73745f6469737061746368000000030000000000000000
igp        0x726f757465725f706f73745f6469737061746368000000040000000000000002
```

**ISMs.**

| Celestia-origin, `TeeDcapIsm` on | address |
|---|---|
| Ethereum Sepolia | `0xB37ea65A1379d013752f6e1976740c58bca9Bfa4` |
| Arbitrum Sepolia | `0xE2CED40FD618Bd78A0b34Fef37b491d20D3e9Cfc` |
| Base Sepolia | `0xDE2f4FFc5BE037c7fA463e875Ef774B6eBF28B91` |
| Eden | `0x0056C714170B4B69CDb4B80fA12BCEdb1BaA3443` |

Revision 1. Every router points straight at its chain's ISM and dispatches through the mailbox's default hook.

| EVM-origin, `x/teeism` on `teeism-local` | id |
|---|---|
| Sepolia `11155111` | `0x726f757465725f69736d000000000000000000000000002b0000000000000001` |
| Arbitrum `421614` | `0x726f757465725f69736d000000000000000000000000002b0000000000000002` |
| Base `84532` | `0x726f757465725f69736d000000000000000000000000002b0000000000000003` |
| Eden `3735928814` | `0x726f757465725f69736d000000000000000000000000002b0000000000000004` |
| routing ISM over all four | `0x726f757465725f69736d00000000000000000000000000010000000000000005` |

**Tokens.** Synthetic on every EVM chain. EVM to EVM is two hops via Celestia.

| | collateral | synthetic |
|---|---|---|
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0x079ffCD6D8b4EBCee45dc6CFf21cE8bDE6b312bf`<br>Arbitrum `0x57bc2BeE5E77d41E5eAc454085c0Fa01Cca242d6`<br>Base `0x1379A7dcC7cb39F9B3c2370cACe5EBed6cdc6116`<br>Eden `0xC155A1Ad822Ec25E2251cB5CCB2a2b07c265d5b8` |
| teeUSD | none: minted on Celestia, as the synthetic `0x726f757465725f61707000000000000000000000000000020000000000000001` (denom `hyperlane/<that id>`) | Sepolia `0x0d9CDDe969C3dE77B404904F1cfB07978A16636c`<br>Arbitrum `0x3acacD5c4A4017F90710ed9957D7932Af95B2988`<br>Base `0x541de526c22795117054bE44a00b94584ecb52Cd`<br>Eden `0xA05038CDcD5552A5FAc03AF2324b1DC3e0C9bb86` |

**Eden.** Evolve-stack; mailbox and hook are ours.

```
chain id     3735928814      ~10 blocks/s
mailbox      0x0F6E896Cd58f2B296224B40487A52cC110F525fb
merkle hook  0x27aBE755196D607ad0faE9846d75629e628a24F4
DA           mocha-5, namespace 0000000000000000000000000000000000005d2e074163aa3b4d9818
sequencer    ed25519 4366433b4309d4f077f0cc1f4370a525736df9a1dc9a205b8d2db1d630b68d51
```

**Base and Arbitrum.** Canonical Hyperlane deployments. Pinned sequencer keys:

```
base       0xb830b99c95Ea32300039624Cb567d324D4b1D83C   SystemConfig 0xf272670eb55e895584501d564AfEB048bEd26194 unsafeBlockSigner
arbitrum   0x9396c22161c821231ad4ae8fcf991b4beee39990   SequencerInbox 0x6c97864CE4bEf387dE0b3310A44230f7E3F1be0D isSequencer
```

**Intel collateral (PCCS).** Ours, Automata's contracts at the same CREATE2 addresses on all four EVM chains (namespace `tee-isms/v2`, owner `0x318d22faa1e0f29eac7ef644a8fac676f6688d1e`):

```
entrypoint   0xdd3bEbec40398671D053AF7ACa6AC601F7f15b76
router       0x27fbb1f96ec0dda0b1052c49a9a118fdec7a264f
v4 verifier  0xc4cef0bf439998add1d845c674921d128a987e08
v5 verifier  0xf04869c04328d1f8e2742479e915a3f4aafbcf90
storage      0x7a58de47eb15fa9bcac84e1d917046c3792e85c9
```

The DAOs and helpers: `devnet/.state/deployment.json`, `evm.<chain>.pccs`.

**Host.** `ark` runs everything except the enclaves.

```
:3000  UI, and the only public way to reach the chain (/rpc /rest /api /evm/<chain>/ /tx/<hash>)
:3001  relayer dashboard and API
:3002  gas oracle dashboard and API

teeism-celestia    docker   the chain
mocha-light        docker   celestia-node light node, for Eden
teeism-gateway     docker   UI and proxies
teeism-relayer     systemd  every route, the API, the faucet
teeism-gas-oracle  systemd  paymaster upkeep
```

## Design

- Enclave is stateless; its state lives in the ISM.
- Shared CVM = one identity: any image change re-pins all 8 ISMs. A chain in its own CVM re-pins only its ISMs.
- Each ISM pins a set of 1 to 8 enclaves, any one of which may attest; the state carries the set's digest. Two enclaves keep a route up while one is down, and a rotation pins old and new together so either is accepted while the relayer moves; a batch in flight across a re-pin is refused as a trusted-state mismatch and rebuilt.
- Re-pin in place: `TeeDcapIsm.setEnclaves` (EVM), `tx teeism set-identities` (Celestia). Address, root, height kept; no router moves. Only a `TeeDcapIsm` `VERSION` change replaces an EVM ISM, from its predecessor's last state, and re-points the routers.
- `tx teeism set-verifier-overrides`: trust a rotated Intel root CA. Nothing relaxes Intel's verification.
- `tx teeism update-owner`: hand an ISM to a multisig. EVM controls sit with the Automata owner. See [ADMIN.md](deploy/ADMIN.md).
- Rollups: enclave checks the sequencer signature, takes the signed state root, proves the tree. No re-execution, no L1 wait.
- The enclave attests the origin tree's root, not a list of messages. Each message proves itself at delivery with a merkle branch in its metadata against any root in the ISM's retention window (its newest 32 attested roots by default; the owner sets 1 to 256), so an origin block of any size is one attestation. A message whose root has been pruned is re-proved by the relayer against a root the ISM holds, so a message dispatched during an outage stays deliverable.
- Anyone may submit an attestation, so the ISM can move past messages the relayer has not indexed. The relayer tracks the origin height it has accounted for separately from the ISM's, and delivers whatever lies between them.
- Heartbeat: a quiet route attests daily with no new leaves, so its ISM stays near its origin's head. Both ISMs refuse an attestation that leaves the tree root unchanged and moves the origin clock less than 30 minutes.

## Trust

- **Trusted:** Intel TDX, pinned identity and its re-pin key, each ISM's starting state, our Automata contracts (EVM), Base/Arbitrum/Eden sequencer keys.
- **Not trusted:** RPCs, the relayer (can stall, not forge), Phala beyond uptime, enclave inputs.
- **Launch:** both ISMs refuse a quote from a TD launched in debug mode or migratable, or with a service TD bound (non-zero `mr_servicetd`): its host, or the migration TD that holds its export keys, could read and change it while it keeps quoting our measurements. Our enclave launches with only `SEPT_VE_DISABLE` set.
- **Risks:** a TDX break forges anything. The owner can re-pin any ISM to an enclave of its choice. One EOA owns ISMs, routers and Automata roles. Moves to a multisig before mainnet (`update-owner` on Celestia, `transferOwnership` on EVM).

## Tests

```sh
cd tee-hyperlane && cargo test                        # state proofs, trees, L2 roots, Eden execution
cd tee-hyperlane/contracts && forge test              # TeeDcapIsm.sol
cd cli && cargo test                                  # teeism
cd ../celestia-app-local && go test ./x/teeism/...    # the Celestia verifier
```
