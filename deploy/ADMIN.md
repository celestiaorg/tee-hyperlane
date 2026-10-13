# Admin controls

Rotations: [MAINTAIN.md](MAINTAIN.md). Multisig before mainnet ([Celestia](#transfer-ownership), [EVM](#evm-transfer-ownership)).

## Who the owner is

| Where | Owner | Key |
|---|---|---|
| Celestia ISMs (`x/teeism`) | `relayer` | `--from relayer` (`.state/celestia`) |
| EVM ISMs (`TeeDcapIsm`), Automata | deployer | Ledger (`EVM_SIGNER=ledger`) or `EVM_PRIVATE_KEY` |

## What the owner controls

| Control | Celestia | EVM |
|---|---|---|
| Re-pin identity | `tx teeism update-identity` | `setEnclave` (ISM `VERSION` 3+) |
| Transfer ownership | `tx teeism update-owner` | `transferOwnership` |
| New Intel root CA | `set-verifier-overrides --trusted-root-cert` | new PCCS `PcsDao` |
| New quote version | v4, v5 built in; later: celestia-app upgrade | `setQuoteVerifier` (`75-dcap-verifiers.sh` adds v5) |
| Stop an ISM | re-pin to unused identity | `setEnclave` to unused measurements |

## Setup

On ark as `chef`:

```sh
cd ~/tee-ism-nonzk/devnet && . scripts/lib.sh
A=.state/bin/celestia-appd; H="--home .state/celestia --keyring-backend test"
TX="$H --chain-id teeism-local --node http://localhost:26657 --fees 200000utia --gas 400000 -y"
ORIGIN=base                            # any of sepolia, arbitrum, base, eden
ENCLAVE=$ORIGIN                        # the enclave that attests it: ethereum for sepolia
ISM=$(load ism-celestia-$ORIGIN)
```

Inspect:

```sh
$A q teeism ism $ISM --node http://localhost:26657 -o json | jq '.ism | {owner, identity, verifier_overrides}'
```

## Celestia

### Re-pin the enclave identity

**Payload:** `.state/out/identity-<enclave>.json` (`30-enclave-up.sh`) or live enclave:

```sh
../tee-hyperlane/target/release/tee-hyperlane identity --url $(load enclave-url-$ENCLAVE) --json /tmp/identity.json
.state/bin/teeism-identity -identity /tmp/identity.json      # digest, to compare with README.md
$A tx teeism update-identity $ISM /tmp/identity.json --from relayer $TX
```

### Transfer ownership

```sh
NEW=celestia1...                       # a multisig, see below
$A tx teeism update-owner $ISM $NEW --from relayer $TX
```

Multisig owner:

```sh
$A keys add admin --multisig signer1,signer2,signer3 --multisig-threshold 2 $H
$A tx teeism update-identity $ISM /tmp/identity.json --from admin --generate-only $TX > unsigned.json
$A tx sign unsigned.json --from signer1 --multisig $($A keys show admin -a $H) $TX > s1.json   # each signer
$A tx multisign unsigned.json admin s1.json s2.json $TX > signed.json
$A tx broadcast signed.json --node http://localhost:26657
```

**Warning:** `85-celestia-isms.sh` only re-pins `relayer`-owned ISMs. Re-pin from the new owner first.

### Update the Intel root CA

`--trusted-root-cert`: repeatable, PEM/DER, replaces prior. Only `Intel SGX Root CA`, P-256.

**Payload:** URL below, or last cert of a `teeism-collateral` issuer chain. Check fingerprint against Intel's announcement.

```sh
curl -sO https://certificates.trustedservices.intel.com/Intel_SGX_Provisioning_Certification_RootCA.cer
openssl x509 -inform der -in Intel_SGX_Provisioning_Certification_RootCA.cer -noout -subject -fingerprint -sha256
$A tx teeism set-verifier-overrides $ISM --trusted-root-cert Intel_SGX_Provisioning_Certification_RootCA.cer --from relayer $TX
$A tx teeism set-verifier-overrides $ISM --clear --from relayer $TX     # back to the built-in root
```

### Stop an ISM

Re-pin back to resume.

```sh
jq '.compose_hash = "0x" + ("00" * 32)' /tmp/identity.json > /tmp/stopped.json
$A tx teeism update-identity $ISM /tmp/stopped.json --from relayer $TX
```

## EVM

Scripts `80`/`90` sign likewise (`devnet/.env`).

```sh
SIGN="--ledger --from $(cast wallet address --ledger)"   # or: SIGN="--private-key $EVM_PRIVATE_KEY"
rpc() { python3 -c "import json;print(json.load(open('.state/out/pccs-$1.json'))['rpc'])"; }
ISM=$(load ism-base); RPC=$(rpc base)     # any of arbitrum, base, sepolia, eden
cast call $ISM "owner()(address)" --rpc-url $RPC
cast call $ISM "VERSION()(uint8)" --rpc-url $RPC      # reverts on revision 1
```

### Re-pin the enclave identity

`VERSION` < 3: `80-evm-isms.sh` replaces, `90-evm-warp.sh` re-points.

**Payload:** Celestia enclave `/identity`: `keccak256(mr_td ++ mr_config_id ++ rtmr0 ++ rtmr1 ++ rtmr2)`, identity digest.

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
cast send $ISM "setEnclave(bytes32,bytes32)" $MEASUREMENTS $IDENTITY --rpc-url $RPC $SIGN
```

### <a name="evm-transfer-ownership"></a>Transfer ownership

Also: factory, routers, Automata entrypoint, PCCS storage.

```sh
cast send $ISM "transferOwnership(address)" $SAFE --rpc-url $RPC $SIGN
```

**Warning:** `80-evm-isms.sh` only re-pins ISMs its signer owns. Re-pin from the Safe first.

### Intel's side on EVM

- **Collateral:** PCCS `upsert*`, monthly ([MAINTAIN](MAINTAIN.md#monthly-intel-collateral)). Needs `ATTESTER_ROLE`: `grantRoles(<key>, 1)` on each versioned DAO and TcbEvalDao.
- **TCB status:** only `UpToDate`, `SWHardeningNeeded`; else new ISM version.

## <a name="tcb-recovery"></a>TCB recovery (Intel moves to a new evaluation number)

```sh
curl -s https://api.trustedservices.intel.com/tdx/certification/v4/tcbevaluationdatanumbers \
  | jq '.tcbEvaluationDataNumbers.tcbEvalNumbers[] | {n: .tcbEvaluationDataNumber, released: .tcbRecoveryEventDate}'
curl -s "https://api.trustedservices.intel.com/tdx/certification/v4/tcb?fmspc=20a06f000000" | jq .tcbInfo.tcbEvaluationDataNumber   # standard today
```

Unpatched hosts go `OutOfDate`, routes stop. Re-pin won't help.

- **Both:** Phala patches the host, or move CVM. Ask early.
- **EVM, beforehand:** `./scripts/76-tcb-eval.sh <chain> <number>` (owner), then monthly seed ([MAINTAIN](MAINTAIN.md)).

## When something breaks live

| What happens | Celestia | EVM |
|---|---|---|
| New image, platform update, new CVM | re-pin | re-pin (replace if `VERSION` < 3) |
| Fresh collateral | nothing | monthly `seed-evm-collateral.sh` |
| PCS down, collateral expires | routes stop until Intel publishes again | routes stop 30 days after the last seed; seed when back |
| [TCB recovery](#tcb-recovery) | patch host | patch host, new DAOs |
| PCK certificate revoked | new CVM, re-pin | same |
| Root CA rotated, same name/key type | `--trusted-root-cert` | new `PcsDao` with new key hash, `grantDao` |
| Other root/name change; new collateral format (TCB info v4, QE identity v3) | **not fixable live:** celestia-app upgrade | new Automata verifier/DAOs |
| Quote v5 | nothing | `75-dcap-verifiers.sh` |
| Quote after v5 | **not fixable live:** celestia-app upgrade | `setQuoteVerifier` |
| TDX broken | stop the ISMs | same |
| Owner key compromised | transfer ownership | same |
| Owner key lost (unrecoverable) | new ISM, re-point route | new ISM, `setInterchainSecurityModule` |
