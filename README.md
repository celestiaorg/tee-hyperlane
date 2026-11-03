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

First deployed with `teeism` on 2026-10-15; redeployed from a fresh devnet on 2026-10-26 (dstack 0.6.1 enclave, identity v3 pinning RTMR0-2). On 2026-10-30 the enclave moved to root attestation: new CVM, the four EVM ISMs replaced (`TeeDcapIsm` `VERSION` 5) from their predecessors' last state and the routers re-pointed, the four Celestia ISMs re-pinned in place, the chain restarted on celestia-app `1c3d993`. Every id below is from those two days. `teeism status` prints the live ids. Verify: `teeism enclaves verify`.

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
compose_hash   0x362d94314eaeafc7ade7a4ffe2a78ec53637c9251264a5b9b4e2fe5bd4abb079
os_image_hash  0xec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6
rtmr0          0x68102e7b524af310f7b7d426ce75481e36c40f5d513a9009c046e9d37e31551f0134d954b496a3357fd61d03f07ffe96
rtmr1          0x89ef86c5125b986c325f0a94acfa87718a4ef3a4d4f237dbb72dda3cf777fd2c054555fb4407eb53338d7d67329a7333
rtmr2          0xe9000e338f33233e8f51b7fd031e1c158bdcf8572eab041c0bc8eb7fef48b23259a6a556e6c8e2af99999fc39c793179
identity       0xe21706c3de5bfee4a04f2e000d3ca48439e680004f8fb522ed5ab2eeea2cece9
measurements   0x61420e690f48afa6ad93a1fa608ea901fe9cfcdaaf3c16c85106defae339bb0c
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
| Ethereum Sepolia | `0x2780675BB83A455F7f3731869933b39339B37D3E` |
| Arbitrum Sepolia | `0xF77063b029d5F4D962D7f2a8F6623FBaF79190ED` |
| Base Sepolia | `0x5fdbaD4f8e209601f5F811Ff81BF1162c7Aa066e` |
| Eden | `0x3616c0b280076Ec2bCb2EAE624D1C99D51c88D76` |

Revision 5 (root attestation). Their revision-4 predecessors (`0x888774…`, `0x2E2036…`, `0xB5A922…`, `0x875F62…`) keep their last state and nothing points at them.

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
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0x2022b3702c5ab233ce9Da34197ee9b5942AD11fD`<br>Arbitrum `0x7a09FC80cAFaff73335811B431a41dE3d06d0CC2`<br>Base `0x0068917588ceE1afCe257422A6Ee10eAef445f10`<br>Eden `0x62AD8dEbAE1895aEdB0f4Fc2AcaB1B3FAE67Fe02` |
| teeUSD | none: minted on Celestia, as the synthetic `0x726f757465725f61707000000000000000000000000000020000000000000001` (denom `hyperlane/<that id>`) | Sepolia `0xc381f89241D03C7DD62eFe7E9c1e456Cf9f0904f`<br>Arbitrum `0x316006Ed5A096E1F1Ab32b436696B576A7645F0c`<br>Base `0xB151c781c0DfB74807Aa67b7198eB0a0A55e142A`<br>Eden `0x4592f8D742E8b3881B3B94C00260C95aFcc298c0` |

**Eden.** Evolve-stack; mailbox and hook are ours.

```
chain id     3735928814      ~10 blocks/s
mailbox      0xbFbc80F66CD2E3534779d4952CC669c86F298479
merkle hook  0x7fE7582d7BE76f4053c291C30d38284cb4935A1a   (tree at slot 151)
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
- Re-pin in place: `TeeDcapIsm.setEnclave` (EVM, revision 3+), `tx teeism update-identity` (Celestia). Address, root, height kept; no router moves. Only a `TeeDcapIsm` `VERSION` change replaces an EVM ISM, from its predecessor's last state, and re-points the routers.
- `tx teeism set-verifier-overrides`: trust a rotated Intel root CA. Nothing relaxes Intel's verification.
- `tx teeism update-owner`: hand an ISM to a multisig. EVM controls sit with the Automata owner. See [ADMIN.md](deploy/ADMIN.md).
- Rollups: enclave checks the sequencer signature, takes the signed state root, proves the tree. No re-execution, no L1 wait.
- The enclave attests the origin tree's root, not a list of messages. Each message proves itself at delivery with a merkle branch in its metadata against any root the ISM has attested, so an origin block of any size is one attestation and a message dispatched during an outage stays deliverable.
- Heartbeat: a quiet route attests daily with no new leaves, so its ISM stays near its origin's head. Both ISMs refuse an attestation that leaves the tree root unchanged and moves the origin clock less than 30 minutes.

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
