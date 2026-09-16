# EVM attestation stack

The devnet verifies TDX quotes directly on every chain: `google/go-tdx-guest` inside
celestia-app on the local chain, and Automata's deployed Solidity verifier on the three EVM
testnets. No zero-knowledge proofs are involved anywhere in this devnet.

We run **our own instances of Automata's contracts** rather than using theirs. Not for
security reasons, and not because theirs are deficient: writes to their FMSPC TCB DAO are
gated on an `ATTESTER_ROLE` they hold, so we could never publish the TCB record for our
platform on the chains that lack it. Owning the deployment is the only way to publish our own
collateral without waiting on a third party.

Their contracts are used unmodified. The only source change is `Salt.sol`, and only to
domain-separate the CREATE2 salts.

## Addresses

Identical on all three chains, because CREATE2 through a shared deployer makes the address a
function of salt and bytecode alone.

```
AttestationEntrypoint   0x961D4408f512D4a169bD76433460d2981a70c71F
PCCSRouter              0xdA7336571D634bE002035Af6ec55F0816A2Ed263
V4QuoteVerifier         0xFFd8Ddff9b7e9ce124A7fdddcd817bA4d8B37ab7
PcsDao                  0x3c3fF9105e62228c7dA62C3bA04d24D320c4433C
EnclaveIdentityDao_20   0x426B9aC0e424dEcC66e4C3a7d9293839e16D8fc1
TcbEvalDao              0x03b1B658C34Bb7919A9cA2067d0055f7dD5C5495
DaoStorage              0xCFe415d68Ef55407B1cBA73C4bC267B28d916643
DaoStorageV2            0x9137457c28Ffef88E9B06BDeeC61a362FDC71651

FmspcTcbDao_20          0x7BDA83918CAAD9b5EC7F88A24660167E90053690  arbitrum, sepolia
                        0x06D080A8642803465500D6C9004Cc9CF48094EeD  base
```

Base's differs because the versioned deploy had already placed the EnclaveIdentity DAO there,
and that script deploys both DAOs in one transaction: the collision on the first reverted the
second with it. It was redeployed by hand with plain CREATE, whose address derives from the
sender's nonce rather than a salt.

`owner` and `ATTESTER_ROLE` on every contract are the deploy key. That key can repoint the
router at a different DAO, which is the strongest privilege anywhere in the EVM path. It is
also the key that pays gas. Splitting those roles is worth doing before this carries value.

## Deploying to a new chain

Their `new-network.sh` is not sufficient. The working order:

1. `make deploy-helpers` then `make deploy-dao`
2. **`forge script script/automata/DeployCrlV2.s.sol`** - deploys `PccsDependencyConfig`,
   which every versioned DAO depends on and which their sequence never deploys
3. `deploy_versioned.sh` for `storage-v2`, `tcb-eval`, `versioned 20`, `fmspc-v2 20`
4. `grantRoles(deployer, 1)` on each versioned DAO and on the TcbEvalDao
5. Copy the PCCS deployment record into the attestation repo's registry at
   `rust-crates/libraries/network-registry/deployment/current/<chain-id>/onchain_pccs.json`,
   and alias the plain DAO names to the CrlV2 ones the deploy actually produced
6. `DeployRouter`, then `deployEntrypoint()`, then `DeployVerifier`. The verifier registers
   itself on whatever entrypoint `dcap.json` names, so point that at ours first or the whole
   broadcast reverts and nothing is deployed
7. `setQeIdDaoVersionedAddr(20, ...)` and `setFmspcTcbDaoVersionedAddr(20, ...)` on the router
8. `setCallerAuthorization(router, true)` on **both** storage contracts, or every read reverts
9. `seed-evm-collateral.sh <chain>`

Things that will waste an afternoon otherwise:

- Without domain-separated salts, CREATE2 reproduces Automata's live addresses and collides.
  The repo also ships their deployment records pre-populated, so an unmodified run silently
  produces a stack that is half theirs.
- `SKIP_ESTIMATE` must be the string `true`. Anything else still runs estimation.
- Their `config_versioned.sh` reports `granted` while granting nothing on the V2 DAO.
- `cast` cannot encode a tuple containing a JSON string; QE identity, TCB info and evaluation
  numbers all need hand-built calldata.
- Sending transactions back to back races the node's nonce and returns
  `replacement transaction underpriced`, which looks exactly like a rejected upsert.

## The monthly job

Intel's TCB info, QE identity and both PCK CRLs are valid for thirty days. When they lapse the
verifier returns `TCBR` or `PCKCRLH` and the affected route stops.

```
seed-evm-collateral.sh arbitrum
seed-evm-collateral.sh base
seed-evm-collateral.sh sepolia
```

It is idempotent and reports `published`, `already current`, or `FAILED` with a reason per
artifact. The root CA CRL is yearly; the root and TCB signing certificates effectively never
change.

Three tiers of failure, in increasing order of work:

```
certs expired             the three commands above
devnet was torn down      also make init, which redeploys the four ISMs
Intel advanced the eval   also a new versioned DAO per chain, and repoint the router
```

The third is the one that will surprise you. The versioned DAO pins its evaluation number:

```solidity
if (tcbInfo.evaluationDataNumber != TCB_EVALUATION_NUMBER)   // 20
    revert Invalid_Tcb_Evaluation_Data_Number();
```

All three chains currently report `standard(TDX) = 20`, and Intel already publishes 13 through
22. When the standard advances past 20, fresh TCB info arrives stamped 21 and our eval-20 DAO
rejects it, while the router simultaneously looks up a DAO for eval 21 that does not exist.
The symptom is `TCBR`, identical to ordinary expiry.

## What each cycle costs

Measured on 2026-09-16 at 1.1 gwei on Sepolia, 0.006 gwei on Base and 0.25 gwei on Arbitrum.
Gas is stable across chains; the spread in ETH is entirely gas price, so Base is three orders
of magnitude cheaper than Sepolia for the same work.

```
                              gas          sepolia      base         arbitrum
one attestation            5,331,900     0.005900     0.000031     0.001330
one message delivery         124,000     0.000140     0.000001     0.000030
end to end, one transfer                 0.006011     0.000032     0.001366

make init (ISM + router)                 0.005950     0.000034     0.001378
```

`make init` takes about 200 seconds wall clock, most of it waiting for the Phala CVM to boot.
Attestation itself is 1.5 seconds, which is the round trip to the enclave and nothing else:
there is no proving on this path.

The PCCS and Automata stack underneath is deployed once and survives `make stop`. It cost
about 0.034 ETH on Arbitrum for all 17 contracts plus the first collateral seed.

Only the four ISMs and the three warp routers are rebuilt per cycle, because the ISM pins the
enclave measurements and the origin checkpoint, and a router's security module is fixed at
initialize time. The mailboxes, hooks, IGPs and the whole PCCS stack are untouched.

## Diagnosing a rejection

Automata returns a four-letter code. `TeeDcapIsm` passes it through unchanged in
`QuoteRejected(bytes)` and can expand it for free off chain:

```
cast call <ism> "describeQuoteError(bytes)(string)" $(cast from-utf8 TCBR)
```

All 28 codes are mapped, and a test asserts none falls through to "unrecognised". Codes
naming collateral (`TCBR`, `TCBCH`, `QEIDCH`, `PCKCRLM`, `PCKCRLH`, `ROOTCRLH`, `ROOTH`,
`SIGNH`) mean republish. Everything else means the quote or the enclave is wrong.

One case produces no reason at all. A quote whose signature fails to parse inside Automata's
verifier reverts with empty returndata rather than returning `(false, code)`, so there is no
`QuoteRejected` to expand and `eth_call` reports a bare `execution reverted`. A truncated or
structurally invalid quote does come back as `QuoteRejected("QHS")`, which
`describeQuoteError` reads as a size mismatch. If a rejection ever arrives with no data, the
quote bytes themselves are damaged rather than the enclave being wrong.
