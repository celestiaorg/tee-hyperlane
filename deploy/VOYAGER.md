# Voyager

Two local voyager chains, `voyager-a` (domain 1987016033) and `voyager-b` (1987016034), bridged by
the same enclave, relayer and ISM rules as every other route. Voyager's side is the bridge lane on
voyager-turbo's `jonas/bridge` branch (`crates/bridge`).

## How it is attested

- Voyager's sequencer signs a header for every slot that carries a bridge lane (at least once a
  second): `keccak("voyager-bridge header v1" domain height timestamp keccak(count branch))`, ed25519.
- That root is the ISM's state root. The tree proof is the tree itself (count and branch),
  rehashed under it. No EVM state, no contract: the tree's address is the constant
  `voyager/hyperlane/merkle-tree`, left-padded.
- Trusted: each chain's sequencer key, pinned in `tee-node/src/chains/l2/voyager.rs` like Base's
  `unsafeBlockSigner`. A key rotation needs a new image.
- As a destination, voyager checks the quote at admission (dcap-qvl, fresh collateral carried in
  the request, as for `x/teeism`) against the enclave pinned in the node's `[bridge.enclave]`, and
  the lane checks the transition exactly as `TeeDcapIsm` does.

## Pieces

| | |
|---|---|
| `tee-node/src/chains/l2/voyager.rs` | the origin: both chains, each with its pinned sequencer key |
| `tee-coprocessor/src/origin/l2/voyager.rs` | indexer and the node's `/bridge` client |
| `tee-coprocessor/src/destination/voyager.rs` | `/bridge/attest`, `/bridge/process` |
| `deploy/docker-compose.voyager.yml` | the image; it only ever runs alone (`ALONE_ENCLAVES`), never in the shared CVM |

Adding voyager changed `origin.rs` and `tee-node/Cargo.toml`, which every image shares: the next
`teeism images` moves every digest, and redeploying the shared CVM re-pins all its ISMs.

## Coprocessor config

```toml
[chains.voyager-a]
kind = "voyager"
domain = 1987016033
enclave_chain = "voyager-a"
rpc = "http://127.0.0.1:3101"

[chains.voyager-b]
kind = "voyager"
domain = 1987016034
enclave_chain = "voyager-b"
rpc = "http://127.0.0.1:3102"

[[routes]]
name = "voyager-a-to-voyager-b"
from = "voyager-a"
to = "voyager-b"
enclave = "<the voyager enclave's URL>"
# A voyager ISM is named by its origin's domain.
ism = "1987016033"
```

## Standing it up

1. Run both nodes with a `[bridge]` section (domain, sequencer key, owner, log, routes; see
   voyager-turbo's `BridgeConfig`). The sequencer seeds' public keys must be the ones pinned in
   `voyager.rs`: `voyager-node bridge key --seed <file>`.
2. `teeism enclaves up --chain voyager` (`--unpinned` for a dev image), and copy its identity
   (`mr_td`, `rtmr0..2`, `os_image_hash`, `compose_hash`, `key_provider`) into the destination
   node's `[bridge.enclave]`; restart it.
3. Pin the origin's ISM on the destination, from the origin's latest header, signed by the
   destination's owner: `voyager-node bridge set-ism --node <dest> --key <owner seed> --state <hex>`.
4. Run the coprocessor. `voyager-node bridge transfer-remote ...` on the origin; it arrives as a
   credit out of the destination route's escrow.

Without a TEE, `tests/voyager_live.rs` runs the relayer's gather and the enclave's attest code
in-process against a live node.
