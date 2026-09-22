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
deploy/          the guides, the measured compose file, submit scripts, systemd units
```

## Docs

Three guides, all in [deploy/](deploy/):

- **[deploy/DEPLOY.md](deploy/DEPLOY.md)** - standing up a whole bridge from nothing: where
  every config lives and what goes in it, the enclave image, the Phala CVMs, the chain, the
  Automata DCAP stack per EVM chain, the ISMs, warp routes, adding your own asset, and adding
  a chain the bridge has never seen.
- **[deploy/MAINTAIN.md](deploy/MAINTAIN.md)** - the monthly collateral job, replacing an
  enclave, what forces a redeploy, telling a waiting route from a stuck one, and the trust
  model.
- **[deploy/INTERACT.md](deploy/INTERACT.md)** - Keplr and MetaMask, the CLI, what each route
  should take, what it costs, and how to check the chain rather than the UI.

Run `deploy/verify-digest.sh <app-id>` to check any value below yourself, without trusting
this file.

## Deployments

Enclaves - Phala Cloud `prod9`, image `ghcr.io/jonas089/tee-node`, OS `dstack-0.5.9`,
`tdx.small`. One image per origin family, because an ISM pins the identity of the enclave
that attests *its* origin: split this way, changing Eden's executor leaves the Ethereum side
alone.

| enclave | app id | attests | identity |
|---|---|---|---|
| `tee-celestia` | `1b7bcf07931b7db288e90e672108fd6f165f08ec` | Celestia origin | `0xcf3cded92281d34bf3b7e674546f5c9bdef660bd9980ca9db8bb1eeccb91aa00` |
| `tee-ethereum` | `ebe69e250ee15fa84b6602d3d327f2f7999a8f9c` | Sepolia, Arbitrum, Base origins | `0x25667afa13166bc49034490082f13e108da41c8dd0dd37f20a560574d4117e7d` |
| `tee-evolve` | `52b2e7b2773e1badd9f7f90efdaad4ebe5dc8f09` | Eden origin | `0x1a9c9d7e0a4f0c0e0043c472451ac56fdf9e7c566b4680dfd32828e75fd019cf` |

```
mr_td          f06dfda6dce1cf904d4e2bab1dc370634cf95cefa2ceb2de2eee127c9382698090d7a4a13e14c536ec6c9c3c8fa87077
os_image_hash  bd369a8c2f9edb2b52dad48ac8e0b32dde5f1337c423a506b48d07403a7d8033
mr_kms         92a4bf40c88734b0e56f54b09b1f0fe4b8d3e230047e9298f491968ada8dedf8
```

`mr_td`, the OS image and the KMS are shared; the three differ in `compose_hash`, which covers
each image digest. The EVM ISMs pin the Celestia enclave's measurements
`0x17c5f56c394b05f8d6b739beeda902ca936bc0866aceee102fd317de3ee5c108`, which is
`keccak(mr_td ++ mr_config_id ++ rtmr0..2)`. rtmr3 is excluded because it carries the app id
and the instance id, so pinning it would tie an ISM to one CVM rather than to the code it
runs.

Which image a family runs is `nix build .#image-<family>` and
`deploy/docker-compose.<family>.yml`. Adding a family - Solana, say - is an entry in the
`families` list in `flake.nix`, a cargo feature, an `origins/` module and a compose file.

There are no vkeys. Nothing is proved.

### Chain

Our own chain, not mocha. It runs continuously on the deployment host.

```
chain id   teeism-local
domain     1297040299
mailbox    0x68797065726c616e650000000000000000000000000000000000000000000000
merkle     0x726f757465725f706f73745f6469737061746368000000030000000000000000
igp        0x726f757465725f706f73745f6469737061746368000000040000000000000002
```

### ISMs

Eight, because an ISM pins exactly one origin domain.

Celestia-origin, one per destination. `TeeDcapIsm`, verifying through our own Automata DCAP
deployment on each chain:

| chain | ISM |
|---|---|
| Ethereum Sepolia | `0xa881b8bE4D0af85F3462Eb31D87210638a71aAC1` |
| Arbitrum Sepolia | `0x0eAAf9028a07162307CFB0Ce9808967fb25C659e` |
| Base Sepolia | `0x7A4c36D68b4fd07E533C2EBc5dfdCb86cc612c5a` |
| Eden | `0xB869FeF5F756C369b14A2357Be613630a1245709` |

EVM-origin, all on `teeism-local`, one per origin:

| origin | ISM |
|---|---|
| Sepolia (11155111) | `0x726f757465725f69736d000000000000000000000000002b000000000000001c` |
| Arbitrum (421614) | `0x726f757465725f69736d000000000000000000000000002b000000000000001d` |
| Base (84532) | `0x726f757465725f69736d000000000000000000000000002b000000000000001e` |
| Eden (3735928814) | `0x726f757465725f69736d000000000000000000000000002b0000000000000021` |

Four origins deliver into one Celestia token, so a routing ISM fans them out by origin domain.
It is the token's ISM and the mailbox default:

```
routing ism  0x726f757465725f69736d00000000000000000000000000010000000000000020
```

### Eden

An evolve-stack chain: an EVM chain with no consensus of its own, whose sequencer signs each
header and publishes it to Celestia. Unlike the other three it had no Hyperlane deployment, so
the mailbox and hook are ours.

```
chain id        3735928814      ~10 blocks/s, 18 decimals
mailbox         0x1D32350f3440BEa7f7E450Aa085f63E0d7E38729
merkle hook     0xCfBE7016D123d52A7Db4fc7D087cCb5421dbF8db   (branch at slot 151)
DA              mocha-5, namespace 0000000000000000000000000000000000005d2e074163aa3b4d9818
sequencer       4366433b4309d4f077f0cc1f4370a525736df9a1dc9a205b8d2db1d630b68d51
```

The enclave re-executes Eden's blocks and rebuilds the state root for itself, so a signature
is how a root is *found* rather than why it is believed. The sequencer still decides which
transactions run and in what order; it cannot invent a state they would not reach.
[deploy/MAINTAIN.md](deploy/MAINTAIN.md) has what that does and does not buy.

### Tokens

Two assets, opposite shapes. TIA is Celestia-native, so its collateral sits on Celestia and
every EVM side is a synthetic. USDC is Circle's real Sepolia token, so the collateral sits on
Sepolia and everywhere else holds a synthetic.

| | home chain, collateral | synthetic elsewhere |
|---|---|---|
| TIA | Celestia `0x726f757465725f61707000000000000000000000000000010000000000000000` | Sepolia `0x9822eE81C82138F88D759faef1AC168aDfEe1467`<br>Arbitrum `0x41f992F671D04c5C26350E64FFA3E1D90bc33bcB`<br>Base `0xF50470146B36c638b981e437AB37DfEd9a02FAb3`<br>Eden `0xD2babc9BE1055551b7AB98c440222862a1646158` |
| USDC | Sepolia `0xfb611B6f6CE92033960e99C2D65cee4237e64cDD`, wrapping Circle's `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` | Celestia `0x726f757465725f61707000000000000000000000000000020000000000000001`<br>Arbitrum `0x8C87fd144006C651430450df8b61A15EeB3FF436`<br>Base `0x285b590ee43A1374AA131e7390D0CA687Be43DF9`<br>Eden `0xc09fbf8F17E96ce746D39f9d11a9dD1813F2d220` |

Every route runs through Celestia. No EVM chain's ISM trusts another, so an EVM-to-EVM pair is
two hops.

### Services

One host runs everything that is not an enclave, including the chain and a mocha light node.

```
:3000  bridge UI, and the only public way to reach the chain
:3001  relayer dashboard and API
:3002  gas oracle dashboard and API

teeism-celestia     docker   the chain
mocha-light         docker   celestia-node, for Eden's blob proofs
teeism-gateway      docker   UI, chain proxy, transaction view
teeism-relayer      systemd  eight routes
teeism-api          systemd  attestation lookup
teeism-gas-oracle   systemd  paymaster upkeep
```

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

`make init` does steps 1 to 5. Every step, including 6 and 7, is in
[deploy/DEPLOY.md](deploy/DEPLOY.md).

Until step 3, `require_enclave = false` builds a circuit that accepts any genuine non-debug
TDX enclave on an acceptable TCB level. That is enough to develop and test against, and it is
not silent: such a build warns at compile time and produces a distinct
`ANY-DEVELOPMENT-ONLY` identity digest that is visible in the ISM state on chain.

Two things bite here, both recorded in `deploy/DEPLOY.md`: `phala deploy` picks a *dev*
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
Celestia -> Arbitrum    11-18 s
Celestia -> Base        13-18 s
Celestia -> Sepolia     14-29 s   the spread is one Sepolia block
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
