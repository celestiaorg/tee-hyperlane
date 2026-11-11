# Vendored Automata

Plain copies of two Automata repositories, not submodules, kept out of this project's `src/` so
`forge build` and the tests here never see them. `teeism build` compiles them and
`teeism pccs deploy` deploys the PCCS and the DCAP verifier from their artifacts through the
CREATE2 deployer (`cli/src/steps/pccs/`). Their own `lib/` checkouts are gitignored and fetched
by `teeism build` at the revisions `cli/src/steps/build.rs` pins.

| directory | repository | commit | date |
|---|---|---|---|
| `pccs/` | `automata-network/automata-on-chain-pccs` | `fd799ed` | 2026-11-05 |
| `dcap/` | `automata-network/automata-dcap-attestation` | `41aedff` | 2026-09-03 |

`dcap/` stays at `41aedff` on purpose. Every later upstream commit belongs to Automata's DCAP V2
path, which rewrites the shared bases (`X509ChainBase`, `QuoteVerifierBase`, `TdxQuoteBase`) and the
v4 and v5 quote verifiers we have deployed, needs SP1 v6 verifier sources the build does not pin,
and calls the strict-parser entry point on the PCK helper. Taking it is a new PCCS deployment, not
a refresh.

## Our changes

Everything else is upstream verbatim. Keep these on a refresh.

- `pccs/src/helpers/PCKHelper.sol` stays at the source the live PCCS was deployed from: upstream
  `4105c6f`, without the `parsePckExtensionWithIdentity` entry point added in `f1406ef`. The
  deploy step derives every address from bytecode and takes the helper's address into the
  constructors of the DAOs and the router, so a changed helper would make `teeism pccs deploy`
  stand up a second PCCS instead of finding the live one. Only the DCAP V2 path, which we do not
  deploy, calls the new entry point. Take upstream's helper when the PCCS is redeployed anyway.
- `dcap/evm/foundry.toml` sets `bytecode_hash = "none"` and `cbor_metadata = false`, as upstream's
  PCCS tree does. With the metadata hash in, the bytecode depended on the build environment and
  the live router, entrypoint and verifiers do not reproduce from any checkout; `teeism pccs
  deploy` therefore finds a recorded deployment by its record and the code on chain first
  (`create2_or`), and only computes CREATE2 addresses for what the record lacks. Without the
  hash, a new deployment gives the same addresses from any checkout.
- Ledger signing (`LEDGER`, `LEDGER_PATH`) in `pccs/Makefile`, `dcap/evm/Makefile`,
  `pccs/script/automata/versioned/{config,deploy}_versioned.sh`.
- `GAS_ESTIMATE_MULTIPLIER` in `pccs/script/crl-v2/_common.sh`.
- Salts prefixed `tee-isms/v1:` in `pccs/script/utils/Salt.sol` and
  `dcap/evm/forge-script/utils/Salt.sol`. These are the pre-`teeism` forge scripts and are
  not run any more; the CLI computes its own salts from `DEFAULT_NAMESPACE` in
  `cli/src/steps/pccs/mod.rs`.
- `pccs/deployment/*.json` hold our v1 addresses, Automata's originals under
  `pccs/deployment/_automata_reference/`; likewise the `421614` and `84532` records under
  `dcap/rust-crates/libraries/network-registry/deployment/current/` with `.automata.bak`
  copies. Nothing reads them; the live records are in `devnet/.state/deployment.json`.

## Refreshing

1. Find the upstream commits the trees match (diff `src/` and `evm/contracts/` against upstream
   history), diff the trees against those commits to get our changes as a patch.
2. Copy upstream head in, re-apply the patch, restore `PCKHelper.sol` from git. For `dcap/`, see
   above: a refresh past `41aedff` is a new deployment.
3. `forge build` both trees before and after and compare every artifact's `deployedBytecode`
   and `bytecode`: nothing the deploy step creates may change unless a PCCS redeploy is
   planned, because changed initcode means new CREATE2 addresses and ISMs with an immutable
   verifier address need replacing.
4. `teeism check-secrets`, then commit and update the table above.
