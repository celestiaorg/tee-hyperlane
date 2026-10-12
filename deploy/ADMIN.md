# Admin controls

What the owner key can change on a live deployment, with the command for each and where its
inputs come from. [MAINTAIN.md](MAINTAIN.md) covers routine rotations; this file is the reference
for every owner control, and for what to do when something on Intel's side breaks.

The owner is a trust root. Every control below lets it change what an ISM accepts, and the
Intel-side overrides let it accept collateral Intel has let lapse or a root Intel has rotated.
Nothing lets it skip Intel's verification: every attestation must carry a quote that verifies
against a trusted root. Move ownership to a multisig before mainnet ([Celestia](#transfer-ownership), [EVM](#evm-transfer-ownership)).

## Who the owner is

| Where | Owner today | Key |
|---|---|---|
| Celestia ISMs (`x/teeism`) | the `relayer` account | `.state/celestia` keyring, `--from relayer` |
| EVM ISMs (`TeeDcapIsm`), Automata contracts | one EOA | `EVM_PRIVATE_KEY` |

## What the owner controls

| Control | Celestia | EVM |
|---|---|---|
| Re-pin the enclave identity (new image, platform update, replacement CVM) | `tx teeism update-identity` | `setEnclave` (ISM `VERSION` 3+) |
| Transfer ownership | `tx teeism update-owner` | `transferOwnership` |
| Trust a new Intel root CA | `set-verifier-overrides --trusted-root-cert` | new PCCS `PcsDao` (the root key hash is a constant in it) |
| Accept collateral Intel has not refreshed in time | `set-verifier-overrides --collateral-grace` | none: `seed-evm-collateral.sh` needs Intel's fresh collateral |
| Support a new quote version | needs a celestia-app upgrade | `setQuoteVerifier` (`75-dcap-verifiers.sh` adds v5) |
| Stop an ISM accepting anything new | re-pin to an identity no enclave has | `setEnclave` to measurements no enclave has |

Re-pinning is all a normal rotation needs. The overrides are for incidents.

## Setup

Run on ark as `chef`, from the devnet directory:

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh
A=.state/bin/celestia-appd; H="--home .state/celestia --keyring-backend test"
TX="$H --chain-id teeism-local --node http://localhost:26657 --fees 200000utia --gas 400000 -y"
ORIGIN=base                            # any of sepolia, arbitrum, base, eden
ENCLAVE=$ORIGIN                        # the enclave that attests it: ethereum for sepolia
ISM=$(load ism-celestia-$ORIGIN)
```

Read an ISM's owner, pinned identity and overrides at any time:

```sh
$A q teeism ism $ISM --node http://localhost:26657 -o json | jq '.ism | {owner, identity, verifier_overrides}'
```

## Celestia

### Re-pin the enclave identity

For a new image, a platform update under the CVM, or a replacement enclave. The ISM keeps its id,
root and height, so no route moves. `85-celestia-isms.sh` does this for every ISM the `relayer`
key owns.

**Payload:** the identity JSON of the enclave that attests the ISM's origin. `30-enclave-up.sh`
writes it to `.state/out/identity-<enclave>.json` when it brings the enclave up. To read it from
a running enclave instead, the enclave serves it at `<enclave-url>/identity` and the coprocessor
turns that into the file:

```sh
../tee-hyperlane/target/release/tee-hyperlane identity --url $(load enclave-url-$ENCLAVE) --json /tmp/identity.json
.state/bin/teeism-identity -identity /tmp/identity.json      # digest, to compare with README.md
$A tx teeism update-identity $ISM /tmp/identity.json --from relayer $TX
```

### Transfer ownership

Hands every owner control (re-pinning, overrides) to the new owner. The old owner keeps none.
There is no renouncing: an ISM always has an owner who can act in an emergency.

```sh
NEW=celestia1...                       # a multisig, see below
$A tx teeism update-owner $ISM $NEW --from relayer $TX
```

A multisig owner is an ordinary multisig account. Create it from the signers' public keys,
then each owner action is generated unsigned, signed by enough signers, and broadcast:

```sh
$A keys add admin --multisig signer1,signer2,signer3 --multisig-threshold 2 $H
$A tx teeism update-identity $ISM /tmp/identity.json --from admin --generate-only $TX > unsigned.json
$A tx sign unsigned.json --from signer1 --multisig $($A keys show admin -a $H) $TX > s1.json   # each signer
$A tx multisign unsigned.json admin s1.json s2.json $TX > signed.json
$A tx broadcast signed.json --node http://localhost:26657
```

**Once the relayer no longer owns an ISM, the rotation scripts stop re-pinning it.**
`85-celestia-isms.sh` re-pins only ISMs the `relayer` key owns; for any other it creates a
replacement ISM, owned by `relayer`, and re-points the route. After a transfer, re-pin from the
multisig as above before running the script, so it finds the ISM already on the new identity.

### Override Intel's side of verification

One command sets all overrides at once; flags left out are switched off, and `--clear` switches
everything off. Every change emits `EventSetVerifierOverrides` with the before and after.

```sh
$A tx teeism set-verifier-overrides $ISM --collateral-grace 72h --from relayer $TX
$A tx teeism set-verifier-overrides $ISM --clear --from relayer $TX
```

**`--collateral-grace <duration>`**, at most `720h`. A quote that fails at block time is verified
again as of that long ago, so collateral and certificates that lapsed within the window still
pass. For an Intel PCS outage or a lag in publishing collateral. No payload. It also means a
platform Intel revoked within the window is still accepted, so keep it as short as the outage.

**`--trusted-root-cert <file>`**, repeatable, PEM or DER. Replaces the Intel SGX Root CA built
into the verifier. For a root rotation where Intel keeps the root's name and key type
(`Intel SGX Root CA`, P-256); the verifier still requires both.

Payload: Intel publishes the root CA at
<https://certificates.trustedservices.intel.com/Intel_SGX_Provisioning_Certification_RootCA.cer>
(DER). It is also the last certificate of every issuer chain `teeism-collateral` returns (the
chain is URL-encoded PEM). Check the fingerprint against Intel's announcement before trusting it:

```sh
curl -sO https://certificates.trustedservices.intel.com/Intel_SGX_Provisioning_Certification_RootCA.cer
openssl x509 -inform der -in Intel_SGX_Provisioning_Certification_RootCA.cer -noout -subject -fingerprint -sha256
$A tx teeism set-verifier-overrides $ISM --trusted-root-cert Intel_SGX_Provisioning_Certification_RootCA.cer --from relayer $TX
```

There is no override that skips Intel's verification. A platform Intel rates below `UpToDate`
cannot be let through by the owner; see [TCB recovery](#tcb-recovery).

### Stop an ISM

Re-pinning to an identity no enclave has makes every new attestation fail the identity check,
which stops the ISM advancing. Messages it has already authorized stay deliverable. Re-pin back
to resume.

```sh
jq '.compose_hash = "0x" + ("00" * 32)' /tmp/identity.json > /tmp/stopped.json
$A tx teeism update-identity $ISM /tmp/stopped.json --from relayer $TX
```

## EVM

```sh
rpc() { python3 -c "import json;print(json.load(open('.state/out/pccs-$1.json'))['rpc'])"; }
ISM=$(load ism-base); RPC=$(rpc base)     # any of arbitrum, base, sepolia, eden
cast call $ISM "owner()(address)" --rpc-url $RPC
cast call $ISM "VERSION()(uint8)" --rpc-url $RPC      # reverts on revision 1
```

### Re-pin the enclave identity

`setEnclave` exists from `VERSION` 3. An older ISM cannot be re-pinned; `80-evm-isms.sh`
replaces it, carrying its state over, and `90-evm-warp.sh` re-points the routers.

**Payload:** two values from the Celestia enclave, which attests every EVM ISM's origin. The
measurements are `keccak256(mr_td ++ mr_config_id ++ rtmr0 ++ rtmr1 ++ rtmr2)` cut from the quote
its `/identity` endpoint returns (rtmr3 is left out because it differs per CVM); the identity
digest is the same kind the Celestia ISMs pin.

```sh
URL=$(load enclave-url-celestia)
curl -sS "$URL/identity" -o /tmp/enclave-identity.json
MEASUREMENTS=$(python3 - /tmp/enclave-identity.json <<'PY'
import json, subprocess, sys
q = json.load(open(sys.argv[1]))["quote"]
b = bytes.fromhex(q[2:] if q.startswith("0x") else q)[48:]
print(subprocess.run(["cast", "keccak", "0x" + (b[136:232] + b[328:472]).hex()],
                     capture_output=True, text=True, check=True).stdout.strip())
PY
)
../tee-hyperlane/target/release/tee-hyperlane identity --url $URL --json /tmp/identity.json
IDENTITY=$(.state/bin/teeism-identity -identity /tmp/identity.json)
cast send $ISM "setEnclave(bytes32,bytes32)" $MEASUREMENTS $IDENTITY --rpc-url $RPC --private-key $EVM_PRIVATE_KEY
```

### <a name="evm-transfer-ownership"></a>Transfer ownership

Every contract the owner holds is `Ownable`: the ISMs, the token factory, the warp routers and
the Automata entrypoint and PCCS storage.

```sh
cast send $ISM "transferOwnership(address)" $SAFE --rpc-url $RPC --private-key $EVM_PRIVATE_KEY
```

As on Celestia, `80-evm-isms.sh` re-pins only ISMs the `EVM_PRIVATE_KEY` owns and replaces the
rest, so re-pin from the Safe first.

### Intel's side on EVM

Verification runs through the Automata contracts. There is no per-ISM override; the owner of
the Automata deployment controls it instead:

- **Fresh collateral** (CRLs, TCB info, QE identity) goes in through the PCCS `upsert*`
  functions, which check Intel's signatures rather than the caller. `seed-evm-collateral.sh`
  does it each month ([MAINTAIN](MAINTAIN.md#monthly-intel-collateral)); without it the
  Celestia → EVM routes stop 30 days after the last seed.
- **A new quote version** needs a verifier for it registered on the entrypoint
  (`setQuoteVerifier`, owner only). `75-dcap-verifiers.sh` does this for v5.
- **A new Intel root key** cannot be upserted: `PcsDao` pins the root's public key hash as a
  constant. The storage owner deploys a `PcsDao` with the new hash and `grantDao`s it.
- **TCB status**: `TeeDcapIsm` accepts `UpToDate` and `SWHardeningNeeded` only, as constants.
  Accepting anything else needs a new ISM version.

## <a name="tcb-recovery"></a>TCB recovery (Intel moves to a new evaluation number)

Intel rates each platform against a numbered set of TCB requirements. A new number (a "TCB
recovery") raises the bar; verifiers use the "standard" number, which moves to a new one 12 months
after Intel releases it. Check where things stand:

```sh
curl -s https://api.trustedservices.intel.com/tdx/certification/v4/tcbevaluationdatanumbers \
  | jq '.tcbEvaluationDataNumbers.tcbEvalNumbers[] | {n: .tcbEvaluationDataNumber, released: .tcbRecoveryEventDate}'
curl -s "https://api.trustedservices.intel.com/tdx/certification/v4/tcb?fmspc=20a06f000000" | jq .tcbInfo.tcbEvaluationDataNumber   # standard today
```

When the standard number moves, a platform that has not taken the recovery's microcode and TDX
module updates is rated `OutOfDate`, and both chains refuse its quotes. The enclave's measurements
do not change, so re-pinning does not help.

- **Fix:** the host is patched (Phala updates the machine, or the CVM moves to a patched one).
  On Celestia nothing else is needed. On EVM, the PCCS DAOs are bound to one evaluation number
  (`TCB_EVALUATION_NUMBER()`), so deploy the DAOs for the new number, register them on the
  `PCCSRouter` (owner only) and seed them.
- **Celestia only, to buy time:** the ISM checks that Intel signed the TCB info, not which
  number it is, and Intel keeps serving older numbers. A relayer that fetches the previous number
  keeps verifying. `teeism-collateral` does not take a number yet.
- **EVM:** no equivalent. The ISM verifies against Automata's standard number, chosen by block
  time, so the routes stop until the host is patched and the new DAOs are wired.

## When something breaks live

What can be fixed by the owner on a running deployment, and what needs a new release.

| What happens | Celestia | EVM |
|---|---|---|
| New enclave image, OS or platform update, replacement CVM | re-pin | re-pin (`VERSION` 3+), else replace and re-point |
| Intel publishes fresh collateral as usual | nothing to do: the relayer ships it with every submission | the monthly `seed-evm-collateral.sh` |
| Intel's PCS is down or late, collateral expires | `--collateral-grace` (up to 30 days), then routes stop until Intel is back | routes stop 30 days after the last seed; seed once Intel is back |
| A TCB recovery leaves the platform `OutOfDate` ([details](#tcb-recovery)) | routes stop until the host is patched; a relayer fetching the previous evaluation number bridges it | routes stop until the host is patched and the new DAOs are deployed and wired |
| The platform's PCK certificate is revoked | move the enclave to another machine (new CVM) and re-pin | same |
| Intel rotates the root CA, same name and key type | `--trusted-root-cert` | new `PcsDao` with the new key hash, `grantDao` |
| Intel's root or certificate names change otherwise | **routes stop until a celestia-app upgrade** (new go-tdx-guest) | new Automata verifier/DAOs |
| New collateral format (TCB info v4, QE identity v3) | **routes stop until a celestia-app upgrade** (new go-tdx-guest) | new Automata verifier/DAOs |
| Phala hosts switch to quote v5 | **not fixable live**: a celestia-app upgrade must parse v5 | `75-dcap-verifiers.sh` (v5 verifier) |
| TDX itself is broken | stop the ISMs (re-pin to an unused identity) | stop the ISMs (`setEnclave` to unused measurements) |
| The owner key is compromised | transfer ownership first, from the old key, if still possible | same |
| The owner key is lost | the ISM can never be re-pinned again; create a new ISM and re-point the route to it (warp route owner) | same: deploy a new ISM, `setInterchainSecurityModule` on the routers |
