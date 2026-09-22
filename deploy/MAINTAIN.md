# MAINTAIN

Keeping a deployed bridge alive: what expires, what breaks, how to tell the difference
between a route that is waiting and a route that is stuck, and what forces a redeploy.

[DEPLOY.md](DEPLOY.md) stands one up. [INTERACT.md](INTERACT.md) uses one.

---

## The only recurring job

**Republish Intel's collateral on the three EVM chains every month.**

```sh
devnet/scripts/seed-evm-collateral.sh sepolia
devnet/scripts/seed-evm-collateral.sh arbitrum
devnet/scripts/seed-evm-collateral.sh base
```

Idempotent. Reports `published`, `already current`, or `FAILED` with a reason, per artifact.
Nothing in it is trusted: every artifact is signed by Intel and the DAO verifies that
signature on upload, so a tampered or stale blob is rejected on chain rather than believed.

### What expires, and what does not

| | valid for | where |
|---|---|---|
| TCB info | 30 days | on chain, per EVM chain |
| QE identity | 30 days | on chain, per EVM chain |
| PCK CRLs (platform, processor) | 30 days | on chain, per EVM chain |
| Root CA CRL | ~1 year | on chain |
| Root and TCB signing certificates | effectively never | on chain |
| Enclave measurements | never; they change only when you rebuild | in every ISM, immutably |

**Celestia-destination routes are not affected by any of this.** Their collateral is fetched
fresh from Phala's PCCS mirror on every single attestation and carried inside the transaction,
because consensus cannot make network calls and every validator has to reach the same verdict
from the same bytes. Only the EVM-destination routes read collateral from chain, and only they
go stale.

When it lapses, the verifier returns `TCBR` or `PCKCRLH` and the three Celestia-to-EVM routes
stop. The other three keep running.

### Three tiers of failure

```
certs expired              the three commands above
the devnet was torn down   also make init, which redeploys the ISMs
Intel advanced the eval    also a new versioned DAO per chain, and repoint the router
```

The third will surprise you, because the symptom is `TCBR`, identical to ordinary expiry. The
versioned DAO pins its evaluation number:

```solidity
if (tcbInfo.evaluationDataNumber != TCB_EVALUATION_NUMBER)   // 20
    revert Invalid_Tcb_Evaluation_Data_Number();
```

All three chains currently report `standard(TDX) = 20`, and Intel already publishes 13 through
22. When the standard advances past 20, fresh TCB info arrives stamped 21, our eval-20 DAO
rejects it, and the router simultaneously looks up a DAO for eval 21 that does not exist.

---

## Replacing an enclave

An enclave is disposable. It holds no keys, no state and no disk: the destination chain's ISM
is the light client's only database, so a new CVM picks up exactly where the old one left off.

What must **not** change is its identity: the OS image, the container image and the KMS. The
app id and instance id are deliberately not pinned, which is what makes this a swap.

```sh
phala deploy --node-id 18 --image dstack-0.5.9 --no-dev-os -c deploy/docker-compose.yml
curl -s https://<new-app-id>-8080.dstack-pha-prod9.phala.network/identity | jq -r .
```

Compare against what the ISMs already trust:

```
identity  0x6fc758842ebcb3d8398ca8d77374356128779bd4c1d545e722b62b662dda3961
```

**If it matches**, there is nothing else to do. Edit `tee_node_url` for that route in
`.state/coprocessor.toml` and restart:

```sh
sudo systemctl restart teeism-relayer
curl -s localhost:3001/api/status | jq '.[] | {name, height}'
```

The heights should be the ones the old enclave left behind, and the next tick advances them.
Nothing is replayed, because the relayer derives its whole starting position from the ISM.

**If it does not match**, the image or the OS changed. That is a new identity, and no ISM can
be updated to trust it: the identity is `immutable` in `TeeDcapIsm.sol` and `x/teeism` has no
update message at all. The only path is fresh ISMs. See "What forces a redeploy" below.

---

## Rebuilding the enclave image

Nothing updates automatically, and that is deliberate. The CVMs keep running the pinned digest
until someone edits it. There is no silent update path.

The moment you do re-pin, this happens:

```
new image digest -> new app_compose -> new compose_hash -> new mr_config_id
                 -> new identity -> every ISM rejects every attestation
```

Not degraded. Halted. And unfixable in place, because both sides pin the identity immutably:

- `TeeDcapIsm.sol`: `bytes32 public immutable identityDigest`, set in the constructor and
  re-checked on every transition with `revert IdentityChanged()`. No setter exists.
- `x/teeism`: the proto has only `CreateInterchainSecurityModule` and `SubmitAttestation`.

The constructor even refuses to deploy an ISM whose genesis state does not already name the
new identity:

```solidity
if (_readBytes32(_genesisState, 84) != _identityDigest) revert IdentityChanged();
```

So the full cost of an enclave code change is: build and push, re-pin the digest in
`deploy/docker-compose.yml`, redeploy both CVMs, deploy **six new ISMs across four chains**,
re-point the Celestia routing ISM and the EVM warp routers, and update `coprocessor.toml`. The
old ISMs keep serving anything already in flight.

Budget for it. Do not do it casually.

> The Nix source filter means editing the coprocessor, the gas oracle or a test **cannot** move
> the digest. Only `crates/tee-node`, `crates/hyperlane-types`, `tee-circuit/tee-attestation`
> and the manifests do. Check with
> `nix eval --raw .#packages.x86_64-linux.image.drvPath` before and after.

## Rebuilding the chain image

Nothing measures `celestia-app-teeism:local`, so this is an ordinary container upgrade: build,
recreate, done. Two caveats.

The tag is mutable and the binary carries no version, so `:local` today and `:local` next
month are different code with nothing inside them to tell you apart. Record the commit, or add
`-ldflags "-X …Version=$(git describe) -X …Commit=$(git rev-parse HEAD)"` to the build.

A consensus-relevant change to `x/teeism` is a chain upgrade, not a container restart.

---

## Verifying what is actually running

```sh
deploy/verify-digest.sh <app-id>                            # the compose chain
deploy/verify-digest.sh <app-id> --ism <addr> --rpc <url>   # also what the chain accepts
deploy/verify-digest.sh <app-id> --rebuild                  # also the image, ~35 min
```

It reproduces `compose_hash` from the `app_compose` the enclave hands over, checks that the
same hash appears in `mr_config_id` **inside the signed quote** rather than only in the
unsigned `info` block, and diffs the embedded compose against the file in this checkout.

To confirm the image build is reproducible rather than merely repeatable on one machine:

```sh
nix build .#image --rebuild   # exit 0 means bit-identical
```

> **`os_image_hash` must be `bd369a8c…`.** That is the production dstack OS. `de9c74f0…` means
> a dev image was provisioned, which permits shell access into the CVM and makes the
> measurements meaningless.

---

## Reading a rejection

Automata returns a four-letter code. `TeeDcapIsm` passes it through unchanged in
`QuoteRejected(bytes)` and can expand it for free off chain:

```sh
cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)
```

All 28 codes are mapped and a test asserts none falls through to "unrecognised". Codes naming
collateral - `TCBR`, `TCBCH`, `QEIDCH`, `PCKCRLM`, `PCKCRLH`, `ROOTCRLH`, `ROOTH`, `SIGNH` -
mean republish. Everything else means the quote or the enclave is wrong.

> One case produces no reason at all. A quote whose signature fails to parse inside Automata's
> verifier reverts with empty returndata rather than returning `(false, code)`, so there is no
> `QuoteRejected` to expand and `eth_call` reports a bare `execution reverted`. A *truncated*
> quote does come back as `QuoteRejected("QHS")`. A rejection with no data at all means the
> quote bytes are damaged, not that the enclave is wrong.

---

## A route that looks stuck but is not

Check this list before touching anything. Most "stuck" routes are working correctly.

**An L2 origin is waiting on its dispute window.** `base-to-celestia` and
`arbitrum-to-celestia` derive their root from the L2's dispute anchor on L1, so a transfer
cannot land until the game covering its block resolves. On Base Sepolia that is **exactly five
days plus about three minutes**, measured across consecutive games, and the anchor adopts a
game the moment it resolves. Arbitrum is roughly 31 minutes, which is the validator's posting
cadence rather than its 20-block challenge period.

To tell waiting from stuck, compare the live anchor against the dispatch block:

```sh
cast call 0x2fF5cC82dBf333Ea30D8ee462178ab1707315355 "getAnchorRoot()(bytes32,uint256)" \
  --rpc-url https://rpc.sepolia.ethpandaops.io     # Base Sepolia's anchor, as an L2 block
```

If the anchor is below the dispatch block, it is waiting. The anchor tracks the chain, 600 L2
blocks per game, one game about every 20.5 minutes, so the lag stays constant rather than
growing.

**An Ethereum origin is waiting on finality.** The enclave attests the *finalized* head, so a
message waits roughly two epochs, about 15 minutes, before it can be attested at all.

**A Celestia origin is waiting for a non-empty epoch boundary.** Light-client bootstrap only
exists for epoch-boundary checkpoint roots. A route sits until one lands, which is normal.

**`leaves=N` in the log is the batch size, not the tree size.** We reuse the canonical
Hyperlane deployments on the EVM chains, so their merkle tree hooks carry everyone's traffic.
A batch of nine leaves with zero deliveries means those nine belonged to other people.

---

## A route that is actually stuck

**Symptom: `TrustedStateMismatch` forever.** A staged batch went stale, usually because
something else advanced the ISM. The relayer detects this by comparing the payload's
`prev_state` against the on-chain state and discards it, but a batch staged before that
handling existed has to be removed by hand from `.state/proofs/<route>/staging/`.

**Symptom: the ISM height is far behind and never moves.** The light client needs the commit
at its trusted height, and a pruned chain no longer has it. Confirm `pruning = "nothing"` and
`min-retain-blocks = 0`. If the commit is genuinely gone, the ISM has to be redeployed; there
is no recovery.

**Symptom: `eth_getProof` fails on the archive endpoint.** Resuming from a trusted height more
than about 128 blocks back needs state proofs a public node has pruned. Set `archive_rpc` on
the origin, and never point it at a metered key.

**Symptom: `unknown command "teeism" for "query"` on every Celestia-origin route.** The unit's
PATH is not putting `.state/bin` first, so a different `celestia-appd` is being found.

**Re-anchoring.** If a route's trusted height is too old to prove forward from, it is
re-anchored by bootstrapping it to a recent checkpoint. That is a manual intervention and it
means the route was genuinely broken, not merely slow. Treat every re-anchor as an incident
worth a root cause, not routine upkeep.

---

## What forces a redeploy, and what does not

| change | cost |
|---|---|
| a new asset or warp route | scripts plus config. No ISM changes |
| a new EVM chain | PCCS stack plus one ISM per direction. Existing routes untouched |
| a relayer, oracle or UI change | rebuild and restart |
| collateral expiry | the monthly job |
| an enclave swap, same measurements | edit one URL, restart |
| **a new enclave image or OS** | **six new ISMs across four chains** |
| Intel advancing the TCB eval number | a new versioned DAO per chain, repoint the router |

---

## Secrets

`keys/` and `devnet/.state/` are gitignored and hold real credentials. Never commit them,
never copy them into a tracked file, never send them to an external service.

```sh
deploy/check-secrets.sh                                   # before every commit
ln -s ../../deploy/check-secrets.sh .git/hooks/pre-commit # or let the hook refuse it
```

An Alchemy key reached `main` in this public repo once and was scraped, which is the likely
reason that free tier ran out early. Rotation is the only fix for a key already pushed.

The relayer keys pay gas and can stall the bridge, but neither can make any chain accept a
message the enclave did not attest. The keys that matter more are the Automata `owner` and
`ATTESTER_ROLE`, which can repoint the router at a different DAO. That is the strongest
privilege anywhere in the EVM path, and today it is the same EOA that pays gas. Splitting
those, and moving the ISM and router owners to a multisig, is the most valuable hardening
left.

---

## What is trusted

**Trusted.** Intel TDX and the DCAP PKI, whose root CA is compiled into the enclave. The
pinned enclave measurements. The ISM's genesis state, which names the light-client checkpoint
and is public at creation. On the EVM side, our own Automata DCAP deployment and the Intel
collateral published into it.

**Not trusted.** Every RPC - beacon, execution, Celestia, PCCS - which are data sources only.
The coprocessor and the relayer, which can stall but never forge. Phala as operator, beyond
liveness. The host clock, which is bounded to the attested chain head's timestamp, so a
rewound clock cannot revive a TCB level Intel has revoked. Every field of an `AttestRequest`:
the endpoint is public and anyone can post to it.

That last one is the recurring source of bugs. Several past fixes were the same mistake in
different clothes - a value that looked like configuration was in fact a request field, and
proving something *about* it proved nothing about the bridge. Hence the merkle tree address,
the L2 anchor contract and its slot layout all being compiled in rather than accepted.

## One enclave per origin family

The identity an ISM pins is a hash of the enclave image, and the ISM cannot be told to trust a
different one: it is `immutable` in `TeeDcapIsm.sol` and `x/teeism` has no update message. So
every origin sharing one image meant every ISM sharing one identity, and a change to Eden's
executor re-deployed the Ethereum side too.

There are now three images, one per origin family:

| family | attests | pinned by |
|---|---|---|
| `celestia` | the Celestia origin | the four `TeeDcapIsm` on the EVM chains |
| `ethereum` | Sepolia, Arbitrum, Base | three Celestia-side ISMs |
| `evolve` | Eden | one Celestia-side ISM |

So a change to the evolve executor re-deploys one ISM, not eight. A change to shared code -
`attest.rs`, the tree verification, the state layout - still moves all three, which is correct:
they all run it.

This needed more than cargo features to be true. `buildRustPackage` is input-addressed and
rustc writes the source path into the binary, so while every family shared one filtered source
tree, any edit anywhere gave all three a new store path and a new digest even when the compiled
code was identical. `flake.nix` now gives each family its own filter, listing the origin files
the others must not see.

Measured both ways: before the filters, changing one error string in `evm/exec.rs` moved all
three digests; after them, it moves only evolve's.

**A dependency change still moves all three**, because `Cargo.lock` is in every image's source,
as are `attest.rs`, `hyperlane_state.rs` and `state_proofs.rs`. That is correct - all three
compile them - but it means the split bounds origin-specific changes, not every change.

Each is `nix build .#image-<family>` from the cargo feature of the same name, pinned by
`deploy/docker-compose.<family>.yml`. Adding a family is an entry in the `families` list in
`flake.nix`, a feature in `crates/tee-node/Cargo.toml`, a module under `origins/` and a compose
file; nothing in the build is per-family except the name.

`devnet/scripts/85-celestia-isms.sh` knows which family each origin belongs to, and
`80-evm-isms.sh` takes `ENCLAVE_FAMILY` (default `celestia`).

### Re-deployment moves the checkpoint

A new identity means a new ISM, and the new one is anchored at the origin's **current head**.
Anything dispatched and not yet delivered is below that anchor and never arrives. On Base,
where a transfer is in flight for five days, that is the normal case rather than the corner
one. It is accepted here; the testnet is not worth coupling every deployment to the last.

To recover such a message, anchor the replacement at the old checkpoint instead:

```sh
cast call <old-ism> "state()(bytes)" --rpc-url <rpc>
tee-hyperlane rotate-state --state <that> --identity-digest <new identity>
ISM_GENESIS=<the genesis state it prints> ./devnet/scripts/80-evm-isms.sh
```

`rotate-state` keeps the root, height, timestamp and store commitment and changes only the
identity, which is the one field the ISM checks against itself. The route then replays the gap
and delivers what the old one had seen. `ISM_GENESIS_<ORIGIN>` does the same for the
Celestia-side ISMs.

## Eden, and what it costs in trust

Eden is an evolve-stack chain: an EVM chain with no consensus of its own, whose single
sequencer signs each block header and publishes it as a blob in one Celestia namespace on
mocha. Three things have to hold before the enclave will attest a root from it:

1. the blob was in a Celestia block the light client verified,
2. the pinned sequencer key signed the header in it, and
3. **the enclave re-executed the blocks that produced that root** and reached the same root,
   starting from the state the ISM already trusts.

The third is the one that matters. A signature says who claimed a root, not whether it is the
root executing the chain produces, so without re-execution a dishonest sequencer could sign a
header naming any state it liked and mint whatever it wanted on the far side. With it the
sequencer keeps the powers a sequencer must have - deciding which transactions run and in
what order - and loses the one it must not, which is inventing a state those transactions
would never reach. It cannot sign other people's transactions, so it cannot move their funds.

Only the blocks that changed the state are executed. That is not a shortcut: the executions
chain by state root and the chain has to arrive at the root the sequencer signed for the
target height, so a block left out is an effect missing from the result and the roots stop
matching. Eden makes ten blocks a second and nearly all of them are empty, which is the
difference between verifying a handful of blocks per batch and verifying a million.

The executor lives in `tee-hyperlane/crates/tee-node/src/evm/`: revm for execution, and a
merkle-patricia trie that reads and rewrites itself through the witness rather than a
database. It is held to Eden's own answers by `tests/eden_exec.rs`, which replays real blocks
off the chain, and to a trie built from scratch by `tests/eden_trie.rs`.

**One quirk worth knowing**: Eden does not burn the base fee, it pays it to the block's
beneficiary along with the priority fee. revm burns it, following Ethereum, so the executor
credits it back. The first run of it came out short on exactly one account by exactly
`base_fee * gas_used`, which is how this was found. If Eden ever changes that rule, every
Eden attestation stops rather than starts lying.

What is left to trust: the sequencer can still censor and reorder, and it can still stop.
Neither takes anyone's funds, and both are visible.

### Residual risks

- A TDX break, or an unrevoked but vulnerable TCB, forges any root. This is the irreducible
  assumption. The TCB-status allowlist is the only lever, and it trades liveness for safety on
  TCB-recovery days.
- Message inclusion is enclave-verified, not proven on chain. This widens nothing in practice,
  because a compromised enclave already owns the root and could forge a proof under it, but it
  means a TDX break forges messages directly rather than in two steps.
- The enclave operator can withhold attestations. Funds are never at risk, but a bridge that
  does not advance is a bridge that is down.
- Celestia does no freshness check of its own. The EVM side enforces `maxStateAge`; Celestia
  cannot reject a stale but well-formed state.
- Light-client security is the standard model: a fork needs a third of the *trusted* validator
  set or sync committee to equivocate.
- L2 roots are trustless only once confirmed, gated on each chain's challenge window.
- ISM and warp router owners are single EOAs today.
- Eden's sequencer can censor and reorder, and can stop. It cannot forge a state, because the
  enclave re-executes; see above.
- Eden's executor is ours rather than reth's, so a transaction it disagrees with halts Eden's
  routes. That is the safe direction - a disagreement is a refusal, never an acceptance - but
  it is a liveness risk the other origins do not carry.
