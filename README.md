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
tee-hyperlane/contracts/                TeeDcapIsm.sol
bridge-app/                             React UI, MetaMask + Keplr
devnet/                                 scripts that deploy everything, and the gateway
deploy/                                 the guides, the measured compose files, systemd units
```

- `l1/`: light-client chains (Celestia, Ethereum). `l2/`: sequencer-signed rollups (Base, Arbitrum, Eden).
- Per chain: `tee-node/src/chains/l2/base.rs` (verification), `tee-coprocessor/src/origin/l2/base.rs` (fetching).

## Deployments

Verify: `TARGET=all deploy/verify-digest.sh <app-id>`

**Enclaves.** Phala Cloud `prod9`, `tdx.small`, `dstack-0.5.9`, image `ghcr.io/jonas089/tee-node`. All five in one CVM (`deploy/docker-compose.all.yml`); `30-enclave-up.sh --<chain>` runs one alone.

| enclave | port | attests |
|---|---|---|
| `celestia` | 8080 | Celestia |
| `ethereum` | 8081 | Sepolia |
| `base` | 8082 | Base Sepolia |
| `arbitrum` | 8083 | Arbitrum Sepolia |
| `eden` | 8084 | Eden |

Until the shared CVM rollout, the per-family CVMs (`e6a9dc0d…`, `6479fb4b…`, `c3789ece…`) serve.

```
app id         set at rollout
identity       set at rollout
compose_hash   set at rollout
measurements   set at rollout
```

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
| Ethereum Sepolia | `0x865264E0ae7b173943e34c493E7851B895F37E3F` |
| Arbitrum Sepolia | `0x39b7Cefe6523c474A31652cc297bA2b06ac53AB3` |
| Base Sepolia | `0x94cC258254B4e9ef6675E6f3a2Ee023D3D21bFcC` |
| Eden | `0xe686F221B7A2B641574340F43Ad5cD59707791Ca` |

| EVM-origin, `x/teeism` on `teeism-local` | id |
|---|---|
| Sepolia `11155111` | `0x726f757465725f69736d000000000000000000000000002b000000000000003c` |
| Arbitrum `421614` | `0x726f757465725f69736d000000000000000000000000002b000000000000003d` |
| Base `84532` | `0x726f757465725f69736d000000000000000000000000002b000000000000003e` |
| Eden `3735928814` | `0x726f757465725f69736d000000000000000000000000002b000000000000003f` |
| routing ISM over all four | `0x726f757465725f69736d00000000000000000000000000010000000000000035` |

**Tokens.** Synthetic on every EVM chain. EVM to EVM is two hops via Celestia.

| | collateral | synthetic |
|---|---|---|
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0x9822eE81C82138F88D759faef1AC168aDfEe1467`<br>Arbitrum `0x41f992F671D04c5C26350E64FFA3E1D90bc33bcB`<br>Base `0xF50470146B36c638b981e437AB37DfEd9a02FAb3`<br>Eden `0xD2babc9BE1055551b7AB98c440222862a1646158` |
| teeUSD | none: minted on Celestia, as the synthetic `0x726f757465725f61707000000000000000000000000000020000000000000002` (denom `hyperlane/<that id>`) | Sepolia `0x1be2055d49c0a350C137605a62861f58Aa750297`<br>Arbitrum `0xF699EB10087A1e01993ed628C198fe6e231830eD`<br>Base `0xa8cc6ec4855b1415c03f3aE51953a3643f7991a5`<br>Eden `0xa4697140E90F39D94F3C64D40175FA6F207bc8EC` |

**Pools.** Uniswap v3 TIA/teeUSD, 0.3%, full range, seeded 1M TIA / 500k teeUSD. Owner `0x318d22faa1e0f29eac7Ef644A8FaC676F6688d1e`.

| chain | pool |
|---|---|
| Sepolia | `0x52E470FBe0B120FAcb9a8B6Efbfe58a6263d909D` |
| Base | `0x6E828f3B976d456fe673d26bC0CBEA45838dD018` |
| Arbitrum | `0xe4d1fc54f8B8AfFcD7e2ffF7dC00d000A5CC4FbF` |

**Eden.** Evolve-stack; mailbox and hook are ours.

```
chain id     3735928814      ~10 blocks/s
mailbox      0x1D32350f3440BEa7f7E450Aa085f63E0d7E38729
merkle hook  0xCfBE7016D123d52A7Db4fc7D087cCb5421dbF8db   (tree at slot 151)
DA           mocha-5, namespace 0000000000000000000000000000000000005d2e074163aa3b4d9818
sequencer    ed25519 4366433b4309d4f077f0cc1f4370a525736df9a1dc9a205b8d2db1d630b68d51
```

**Base and Arbitrum.** Canonical Hyperlane deployments. Pinned sequencer keys:

```
base       0xb830b99c95Ea32300039624Cb567d324D4b1D83C   SystemConfig 0xf272670eb55e895584501d564AfEB048bEd26194 unsafeBlockSigner
arbitrum   0x9396c22161c821231ad4ae8fcf991b4beee39990   SequencerInbox 0x6c97864CE4bEf387dE0b3310A44230f7E3F1be0D isSequencer
```

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
- Re-pin in place: `TeeDcapIsm.setEnclave` (EVM, revision 3), `tx teeism update-identity` (Celestia). Address, root, height kept; no router moves.
- `tx teeism set-verifier-overrides`: trust a rotated Intel root CA. Nothing relaxes Intel's verification.
- `tx teeism update-owner`: hand an ISM to a multisig. EVM controls sit with the Automata owner. See [ADMIN.md](deploy/ADMIN.md).
- Rollups: enclave checks the sequencer signature, takes the signed state root, proves the tree. No re-execution, no L1 wait.

## Trust

- **Trusted:** Intel TDX, pinned identity and its re-pin key, each ISM's starting state, our Automata contracts (EVM), Base/Arbitrum/Eden sequencer keys.
- **Not trusted:** RPCs, the relayer (can stall, not forge), Phala beyond uptime, enclave inputs.
- **Risks:** a TDX break forges anything. The owner can re-pin any ISM to an enclave of its choice. One EOA owns ISMs, routers and Automata roles. Moves to a multisig before mainnet (`update-owner` on Celestia, `transferOwnership` on EVM).

## Tests

```sh
cd tee-hyperlane && cargo test                        # state proofs, trees, L2 roots, Eden execution
cd tee-hyperlane/contracts && forge test              # TeeDcapIsm.sol
cd ../celestia-app-local && go test ./x/teeism/...    # the Celestia verifier
```
