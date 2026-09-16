# TEE ISMs

Hyperlane bridging between Celestia and (EVM) Chains, where messages are
authorised by a **light client running inside a TDX enclave**, not by a validator multisig.
Two enclaves cover four networks.

The destination verifies the enclave's TDX quote directly. There is no zero-knowledge proof
anywhere in this path.

## How it works

An enclave verifies an origin chain's consensus, derives its state root, proves the origin
Hyperlane merkle tree under that root, and confirms a claimed batch of message ids is exactly
the new leaves. It signs all of that into one TDX quote.

That quote goes to the destination as it is. The destination checks Intel's signature chain
itself, confirms the quote came from the enclave it pins, and advances its trusted state and
authorises the batch in one transaction. Two verifiers do this, because the two destinations
cannot share one: `TeeDcapIsm.sol` calls our own Automata DCAP stack on each EVM chain, and
Celestia's `x/teeism` calls `google/go-tdx-guest` in consensus, offline, from collateral the
transaction carries.

```
enclave (Phala CVM, stateless)          coprocessor (untrusted)
  verify consensus                        submitAttestation(quote, payload)
  derive state root                       process(message)
  prove Hyperlane tree
  attest via dstack GetQuote            one transaction, no proving
```

The enclave keeps nothing. Its light-client state lives in the ISM's `state` field on the
destination chain, so the chain is the light client's database and a restarted enclave loses
nothing.

## Layout

```
tee-circuit/     enclave identity check, and the SP1 programs the testnet path used
tee-hyperlane/   the enclave, the coprocessor, TeeDcapIsm.sol, the CLI
bridge-app/      React UI, MetaMask + Keplr
devnet/          the whole stack against a local chain, and the gateway
deploy/          compose file, submit scripts, systemd and nginx
docs/            integration guide
```

## Docs

- [docs/integrations.md](docs/integrations.md) - adding a chain: EVM rollups, evolve-stack,
  and entirely new chains like Solana.
- [deploy/DEPLOYMENT.md](deploy/DEPLOYMENT.md) - live addresses and the deployment footguns.
- [deploy/E2E.md](deploy/E2E.md) - the four testnet transfers, with what each cost.
- [deploy/server/TEEISM-SERVER.md](deploy/server/TEEISM-SERVER.md) - the live deployment:
  routers, the routing ISM, the paymaster and the oracle.
- [deploy/server/README.md](deploy/server/README.md) - the Groth16 deployment this replaced.
- [bridge-app/README.md](bridge-app/README.md) - the UI.
- [docs/verify-deployment.md](docs/verify-deployment.md) - checking that what is deployed is
  what is in this repo, without taking its word for it.
- [docs/redeploy.md](docs/redeploy.md) - replacing an enclave.

See [docs/verify-deployment.md](docs/verify-deployment.md) to check any of the values
below yourself, without trusting this file.

## Deployments

Enclaves - Phala Cloud `prod9`, image `ghcr.io/jonas089/tee-node`, OS `dstack-0.5.9`,
`tdx.small`. $0.0608/hr each.

| node | app id | serves |
|---|---|---|
| `tee-nonzk-cel` | `e2bd52eb17e3ca77c99f572e307b3ecb9e2e667a` | the three Celestia-origin routes |
| `tee-nonzk-eth` | `14d61a110ffad00678229e4ee8f6981a1a7244b8` | Sepolia, Arbitrum and Base origins |

Reach them at `https://<app-id>-8080.dstack-pha-prod9.phala.network`.

The pinned identity constrains the **OS image, the container image and the KMS** - never the
app id or instance id. That is deliberate: instances get replaced and providers may change,
and an identity tied to one instance would have to be re-pinned every time. Both nodes above
were deployed independently and produce byte-identical measurements, which is what lets one
ISM accept either.

```
image          ghcr.io/jonas089/tee-node@sha256:77283ad03dd5f2dfbbbb718e4b08f63397ce829c48a4e3cc7b3635d18fe3e0d8
mr_td          f06dfda6dce1cf904d4e2bab1dc370634cf95cefa2ceb2de2eee127c93826980...
os_image_hash  bd369a8c2f9edb2b52dad48ac8e0b32dde5f1337c423a506b48d07403a7d8033
compose_hash   f6ad454d9125b4512c72309e55b4e4fe7bab3b527327dd1c12199ad06cd80f5f
mr_kms         92a4bf40c88734b0e56f54b09b1f0fe4b8d3e230047e9298f491968ada8dedf8

identity       0xd803bb1e4068d907f8a1343df8cc4aecbcec29a287ba1d1f029588f84a641905
measurements   0xbccc1d1a0259eb0fc7476eb9812eb37d6a516ecd74f2de9595f24fbd4c37b8cd
```

`measurements` is what the EVM ISMs pin: `keccak(mr_td ++ mr_config_id ++ rtmr0..2)`. rtmr3 is
excluded because it carries the app id and the instance id, so pinning it would tie an ISM to
one CVM rather than to the code it runs. `identity` is the same commitment in the form
`x/teeism` stores, which compares the five fields individually so a rejection names the one
that diverged.

There are no vkeys. Nothing is proved.

### Chain

The Celestia side is our own chain, not mocha. It runs continuously on the deployment host.

```
chain id   teeism-local
domain     1297040299
mailbox    0x68797065726c616e650000000000000000000000000000000000000000000000
merkle     0x726f757465725f706f73745f6469737061746368000000030000000000000000
igp        0x726f757465725f706f73745f6469737061746368000000040000000000000002
```

Genesis accounts derive from a fixed mnemonic, so the funded address survives a rebuild of
the chain. Keplr reaches it through the gateway, because the RPC port is not exposed.

### ISMs

Six, because an ISM pins exactly one origin domain.

Celestia-origin, one per EVM destination. Solidity, verified on Sourcify with `exact_match`
on both creation and runtime bytecode:

| chain | ISM |
|---|---|
| Ethereum Sepolia | `0xa3F11DD21ed9cf1dEe688c68F1393E6810B379ce` |
| Arbitrum Sepolia | `0xA3754E3358EF03D62A59da08B501cFb70357F4A2` |
| Base Sepolia | `0x218D41065A4599426c8dD8a3eb33d1b29F23A488` |

EVM-origin, all on `teeism-local`, one per origin:

| origin | ISM |
|---|---|
| Sepolia (11155111) | `0x726f757465725f69736d000000000000000000000000002b0000000000000001` |
| Arbitrum (421614) | `0x726f757465725f69736d000000000000000000000000002b0000000000000002` |
| Base (84532) | `0x726f757465725f69736d000000000000000000000000002b0000000000000003` |

Three origins deliver into one Celestia token, so a routing ISM fans them out by origin
domain. Without it two of the three would be checked against an ISM pinned to the wrong
origin and refused:

```
routing ism  0x726f757465725f69736d00000000000000000000000000010000000000000004
```

It is the token's ISM and the mailbox default.

The EVM side verifies through our own Automata DCAP deployment, not Automata's, so the
collateral is ours to keep current. Addresses and the monthly job are in
[devnet/DEPLOYMENT.md](devnet/DEPLOYMENT.md).

### Tokens

The warp routers predate this deployment and were reused. Only their ISM and their
enrollments changed.

| route | teeism-local | EVM side |
|---|---|---|
| TIA | collateral `0x726f757465725f61707000000000000000000000000000010000000000000000` | synthetic `0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE` (Sepolia)<br>synthetic `0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE` (Arbitrum Sepolia)<br>synthetic `0xf4197C55C944987E9b10e09C0A47915211769B78` (Base Sepolia) |

### Gas

Fees are charged in both directions. The relayer pays gas on the destination and is
reimbursed from the paymaster; on Celestia fees accrue in `utia` to the IGP above, and on the
EVM chains to the relayer key `0x318d22faa1e0f29eac7Ef644A8FaC676F6688d1e`.

| chain | paymaster | oracle |
|---|---|---|
| teeism-local | `0x726f757465725f706f73745f6469737061746368000000040000000000000002` | in-module |
| Sepolia | `0x48b1BF6CC2e45Ca52947E95Bb216C2eBdCB19c49` | `0x225B8488242c90085B7A8Ea33Ce8e39Ae9f79722` |
| Arbitrum Sepolia | `0x0ee6374a92ba4E11F920A23c6dd271b594D69A9B` | `0xfA8036Cb092079B095ed60750d7b39c3C220F288` |
| Base Sepolia | `0x5591613C85E9bC95104980d4485c958ee80f6F76` | `0x7A7042C8784700618be87Aac7F9336620e216Bb9` |

`crates/gas-oracle` keeps all of them current hourly. Each EVM paymaster also needs this
deployment's Celestia domain registered against its oracle, which is a manual step and is not
something the oracle service does; without it `transferRemote` reverts with `IGP: no gas
oracle for domain 1297040299`.

### Services

One host runs everything that is not an enclave, including the chain.

```
:3000  bridge UI, and the only public way to reach the chain
       /rpc  /rest  /api  /evm/{sepolia,arbitrum,base}  /tx/<hash>
:3001  relayer dashboard and API
:3002  gas oracle dashboard and API
```

Only 3000 and 80 are reachable from outside that host, and 80 is taken, which is why the
chain's own ports are proxied rather than exposed. Units and configuration are in
[deploy/server/](deploy/server/); [deploy/server/TEEISM-SERVER.md](deploy/server/TEEISM-SERVER.md)
is how to stand the whole thing up, including the parts `make init` does not cover.

```
teeism-celestia     docker   the chain
teeism-gateway      docker   UI, chain proxy, transaction view
teeism-relayer      systemd  six routes
teeism-api          systemd  attestation lookup
teeism-gas-oracle   systemd  paymaster upkeep
```

Full detail, including deployment footguns, in [deploy/DEPLOYMENT.md](deploy/DEPLOYMENT.md).

## Run the tests

```sh
cd tee-circuit   && cargo test           # attestation, identity, byte compatibility
cd tee-hyperlane && cargo test           # state proofs, merkle tree, L2 roots
cd tee-hyperlane/contracts && forge test # TeeDcapIsm.sol and TeeIsm.sol
cd ../celestia-app-local && go test ./x/teeism/...   # the Celestia verifier
```

Much of the suite runs against live chains: Hyperlane's Sepolia merkle tree hook, a Celestia
store proof, a Base Sepolia output root and an Arbitrum Sepolia header are all checked-in
fixtures captured from those chains.

The enclave-rejection tests run against a mock DCAP verifier. That the real one rejects a
wrong enclave was checked separately, on chain: a throwaway ISM pinned to the measurements
with one bit flipped, fed a genuine quote, reverts `WrongEnclave`, while the same quote on a
correctly pinned ISM gets past that check.

## Read an enclave's identity

```sh
cd tee-circuit
cargo run -p circuit-tool -- identity --url https://<app-id>-8080.dstack-pha-prod9.phala.network
```

That is what an ISM pins. The SP1 programs are still in the tree and still build, but nothing
in this deployment uses them.

## Deploy

Bootstrapping has one ordering constraint: the enclave identity can only be pinned after an
enclave exists.

1. Build and publish the `tee-node` image; pin its digest in `docker-compose.yml`.
2. Deploy two CVMs (`tdx.small` is enough - the enclave is a verifier, not a prover).
3. `GET /policy` on one of them, write the measurements into
   `tee-circuit/tee-attestation/enclave-identity.toml`, set
   `require_enclave = true`, rebuild the circuits.
4. Deploy Hyperlane core on the Celestia chain, then the warp routes.
5. Deploy `TeeDcapIsm.sol` on each EVM chain and point the warp routers at it.
6. Create the per-origin ISMs on Celestia, and a routing ISM over them.
7. Create the paymaster and start the oracle.
8. `tee-hyperlane run`.

`make init` does steps 1 to 5. Steps 6 and 7 are in
[deploy/server/TEEISM-SERVER.md](deploy/server/TEEISM-SERVER.md).

Until step 3, `require_enclave = false` builds a circuit that accepts any genuine non-debug
TDX enclave on an acceptable TCB level. That is enough to develop and test against, and it is
not silent: such a build warns at compile time and produces a distinct
`ANY-DEVELOPMENT-ONLY` identity digest that is visible in the ISM state on chain.

Two things bite here, both recorded in `deploy/DEPLOYMENT.md`: `phala deploy` picks a *dev*
OS image unless you pass `--image`, and dev images allow SSH into the CVM. And a Phala node
whose teepod reports no gateway domain will accept TLS and then answer nothing, however
healthy the container is.

## Run the bridge

```sh
tee-hyperlane run   --config coprocessor.toml      # attest, prove, relay, every route
tee-hyperlane serve --proof-dir /var/lib/...       # attestations for the UI
```

Needs `cast` and `celestia-appd` on PATH: the relayer shells out to them to sign rather than
reimplementing two transaction formats.

## Send and check a transfer

```sh
tee-hyperlane send --route celestia-to-sepolia --token TIA --amount 1000000 --to 0x...
tee-hyperlane verify --message-id 0x...
tee-hyperlane status
```

## Measured cost

End to end on this deployment, not estimated. There is no proving, so the only wait is origin
finality plus one transaction.

```
Celestia -> Arbitrum    16-23 s
Celestia -> Base        19 s
Sepolia  -> Celestia    Ethereum finality, ~15 min when Sepolia is healthy
Arbitrum -> Celestia    ~31 min, the assertion cadence
Base     -> Celestia    days, the dispute window
```

Neither L2 figure is ours. Arbitrum's challenge period is 20 L1 blocks, 4 minutes; the wait is
how often the validator posts, measured at a 154 L1 block median, 30.8 minutes. Base is its
dispute-game schedule.

Gas, at 1.1 gwei on Sepolia, 0.006 on Base, 0.25 on Arbitrum:

```
                              gas        sepolia     base        arbitrum
attestation                5,331,900    0.005900    0.000031    0.001330
message delivery             124,000    0.000140    0.000001    0.000030
end to end                              0.006011    0.000032    0.001366
```

On Celestia an attestation is 343,592 gas and a delivery 117,337, carrying 7,785 bytes of
quote, event log and Intel collateral.

The enclaves are cheap because they prove nothing: 2 x `tdx.small` is $2.92/day.

## What is trusted

Intel TDX and the DCAP PKI; the pinned enclave measurements; the ISM's genesis state, which
names the light-client checkpoint and is public at creation; and, on the EVM side, our own
Automata DCAP deployment and the Intel collateral published into it.

That collateral is the one piece with an expiry. Intel's artifacts are valid for 30 days, and
a lapsed republish stops the EVM side with a four letter code rather than a wrong answer.

Not trusted: every RPC, the coprocessor, the relayer, Phala as operator, and the host clock -
which is bounded to the attested chain head's timestamp, so a rewound clock cannot revive a
TCB level Intel has revoked.
