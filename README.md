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
cli/                                    teeism: deploys, rotates, inspects and tears down everything
bridge-app/                             React UI, MetaMask + Keplr
devnet/                                 the chain's and the gateway's compose files, vendored Automata
deploy/                                 the guides, the measured compose files, systemd units
```

- `l1/`: light-client chains (Celestia, Ethereum). `l2/`: sequencer-signed rollups (Base, Arbitrum, Eden).
- Per chain: `tee-node/src/chains/l2/base.rs` (verification), `tee-coprocessor/src/origin/l2/base.rs` (fetching).

## Deployments

Deployed with `teeism` on 2026-10-15; enclave rotated to dstack 0.6.1 on 2026-10-26 (every ISM re-pinned in place). `teeism status` prints the live ids. Verify: `teeism enclaves verify`.

**Enclaves.** Phala Cloud (node picked by Phala), `tdx.small`, dstack `0.6.1`, image `ghcr.io/jonas089/tee-node`. All five in one CVM (`deploy/docker-compose.all.yml`); `teeism enclaves up --chain <chain>` runs one alone.

| enclave | port | attests |
|---|---|---|
| `celestia` | 8080 | Celestia |
| `ethereum` | 8081 | Sepolia |
| `base` | 8082 | Base Sepolia |
| `arbitrum` | 8083 | Arbitrum Sepolia |
| `eden` | 8084 | Eden |

```
app id         4de9f0be7b151f67fd717d8f06ac45da5ff70d84
urls           https://4de9f0be7b151f67fd717d8f06ac45da5ff70d84-<port>.dstack-pha-prod5.phala.network
mr_td          0x2a674327c50218dba880066b349b8d559d749ed68dce33fd651c184a877d084b07a9e583767a7ad5da13ed91deec2b70
compose_hash   0xee935eba5e1771db4a3ce71900bed00f77213335a0abbc24d622f64a419bc543
identity       0x423eea99100f13e07760480ada94552fc50718d36e5ba78e967497f9c840f201
measurements   0x929f72ee86af73bba9c4ae2664e57f1a08630d5f2b77ddfb896cdbcae48e1cb4
```

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
| Ethereum Sepolia | `0x8DDAB7d98b7C7FC40eb4D54Ae10Da60734f2c5AF` |
| Arbitrum Sepolia | `0xE7eAeb624AB993f2b058E041dd2b762a9F0c038B` |
| Base Sepolia | `0x96D3A82398545651117e0B754b55a2cF00A0aA1c` |
| Eden | `0xB06dE0F56E05f5af1431708af6B92888d18A9abd` |

| EVM-origin, `x/teeism` on `teeism-local` | id |
|---|---|
| Sepolia `11155111` | `0x726f757465725f69736d000000000000000000000000002b0000000000000001` |
| Arbitrum `421614` | `0x726f757465725f69736d000000000000000000000000002b0000000000000005` |
| Base `84532` | `0x726f757465725f69736d000000000000000000000000002b0000000000000002` |
| Eden `3735928814` | `0x726f757465725f69736d000000000000000000000000002b0000000000000003` |
| routing ISM over all four | `0x726f757465725f69736d00000000000000000000000000010000000000000004` |

**Tokens.** Synthetic on every EVM chain. EVM to EVM is two hops via Celestia.

| | collateral | synthetic |
|---|---|---|
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0x7F751c18467C6D00f423BBc8183C38B01898D07c`<br>Arbitrum `0xDED207d4d9fad5aC89E755D77029453fFa834F2f`<br>Base `0x5F37c2B101527997436de2BB7C8636B356621d7b`<br>Eden `0xad4471aC85d64E6ea99426EedCB15097EF3EC850` |
| teeUSD | none: minted on Celestia, as the synthetic `0x726f757465725f61707000000000000000000000000000020000000000000001` (denom `hyperlane/<that id>`) | Sepolia `0xB2fab15FdC9185b6F15eB383fe57E12cc7Ea5e99`<br>Arbitrum `0xE7d728A1884C840b4153A0BcAb766e8ebb5352dF`<br>Base `0xdA22F5ebE5e8dF2cbA5a4487622B9EF80BA413e7`<br>Eden `0x254Fd6b521ac0f6911Ef10846D6fc932D9ed3191` |

**Eden.** Evolve-stack; mailbox and hook are ours.

```
chain id     3735928814      ~10 blocks/s
mailbox      0x4D37EeeAE9fA656a85D15366f60dc55aa73a1907
merkle hook  0xDF59A3c135B159a7Ac81b0140425C3B4Ba0407a9   (tree at slot 151)
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
entrypoint   0x1AeAc1f7280fb57c1A2d82bD7f76875a7C32A9d3
router       0x46ccfafa9413dc904eb6dd07e5b3ef1ad6dc8cee
v4 verifier  0xbdde242942c2c6dbd8194235d4011d8296aa6b08
v5 verifier  0xccc63af1c2ed8777c31ed90a1acec8ef1c08e83f
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
- Re-pin in place: `TeeDcapIsm.setEnclave` (EVM, revision 3), `tx teeism update-identity` (Celestia). Address, root, height kept; no router moves.
- `tx teeism set-verifier-overrides`: trust a rotated Intel root CA. Nothing relaxes Intel's verification.
- `tx teeism update-owner`: hand an ISM to a multisig. EVM controls sit with the Automata owner. See [ADMIN.md](deploy/ADMIN.md).
- Rollups: enclave checks the sequencer signature, takes the signed state root, proves the tree. No re-execution, no L1 wait.
- Heartbeat: a quiet route attests daily with no messages, so its ISM stays near its origin's head. An empty batch cannot skip a message, and the enclave and both ISMs refuse one that moves the origin clock less than 30 minutes.

## Trust

- **Trusted:** Intel TDX, pinned identity and its re-pin key, each ISM's starting state, our Automata contracts (EVM), Base/Arbitrum/Eden sequencer keys.
- **Not trusted:** RPCs, the relayer (can stall, not forge), Phala beyond uptime, enclave inputs.
- **Risks:** a TDX break forges anything. The owner can re-pin any ISM to an enclave of its choice. One EOA owns ISMs, routers and Automata roles. Moves to a multisig before mainnet (`update-owner` on Celestia, `transferOwnership` on EVM).

## Tests

```sh
cd tee-hyperlane && cargo test                        # state proofs, trees, L2 roots, Eden execution
cd tee-hyperlane/contracts && forge test              # TeeDcapIsm.sol
cd cli && cargo test                                  # teeism
cd ../celestia-app-local && go test ./x/teeism/...    # the Celestia verifier
```
