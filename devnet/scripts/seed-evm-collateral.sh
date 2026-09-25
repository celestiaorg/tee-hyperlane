#!/usr/bin/env bash
# Publish Intel's signed DCAP collateral into a PCCS we own.
#
# This is the monthly job. Intel's TCB info, QE identity and PCK CRLs are valid for thirty
# days; once they lapse the on-chain verifier starts returning TCBR or PCKCRLH and the
# Celestia-to-EVM routes stop. Re-running this republishes all of it.
#
# Nothing here is trusted: every artifact is signed by Intel and the DAO verifies that
# signature on upload, so a tampered or stale blob is rejected on chain rather than believed.
#
#   usage: seed-evm-collateral.sh <chain>
#   env:   FMSPC (default 20a06f000000), PRIVATE_KEY or EVM_PRIVATE_KEY, and the per-chain RPC
set -euo pipefail

CHAIN="${1:?usage: seed-evm-collateral.sh <sepolia|arbitrum|base|eden>}"
FMSPC="${FMSPC:-20a06f000000}"

# `lib.sh` exports EVM_PRIVATE_KEY, this script has always read PRIVATE_KEY. Under `set -u` an
# unattended run died on the unbound name several minutes in, at the first send, rather than
# saying up front which variable it wanted. Either name works, with or without the 0x.
PRIVATE_KEY="${PRIVATE_KEY:-${EVM_PRIVATE_KEY:-}}"
: "${PRIVATE_KEY:?set PRIVATE_KEY or EVM_PRIVATE_KEY}"
case "${PRIVATE_KEY}" in 0x*) ;; *) PRIVATE_KEY="0x${PRIVATE_KEY}" ;; esac
SGX=https://api.trustedservices.intel.com/sgx/certification/v4
TDX=https://api.trustedservices.intel.com/tdx/certification/v4

DEVNET_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="${STATE_DIR:-${DEVNET_DIR}/.state}"
ADDR_FILE="${STATE_DIR}/out/pccs-${CHAIN}.json"
[ -f "${ADDR_FILE}" ] || { echo "no PCCS addresses for ${CHAIN} at ${ADDR_FILE}" >&2; exit 1; }

# Every address up front, and every one checked, before the first transaction.
#
# These used to be read where they were first needed, with a bare subscript. A record missing
# one name then raised KeyError two thirds of the way in, after the certificates, the CRLs and
# the QE identity had already been published and paid for, leaving a partial seed behind.
# pccs-arbitrum.json was missing TcbEvalDao and would have done exactly that.
field() {
  python3 -c "import json,sys;print(json.load(open(sys.argv[1])).get(sys.argv[2],''))" "${ADDR_FILE}" "$1"
}
RPC="$(field rpc)"
PCS="$(field PcsDao)"
QEID="$(field EnclaveIdentityDaoVersioned)"
EVALDAO="$(field TcbEvalDao)"
TCBDAO="$(field FmspcTcbDaoVersioned)"

# The variable name and the record's name for it are split before the indirection, not
# inside it: `${!pair%%:*}` expands a variable literally called "RPC:rpc", finds nothing, and
# reports every field missing on a record that is perfectly complete.
missing=""
for pair in RPC:rpc PCS:PcsDao QEID:EnclaveIdentityDaoVersioned EVALDAO:TcbEvalDao TCBDAO:FmspcTcbDaoVersioned; do
  var="${pair%%:*}"
  [ -n "${!var:-}" ] || missing="${missing} ${pair#*:}"
done
[ -z "${missing}" ] || {
  echo "${ADDR_FILE} names no:${missing}" >&2
  echo "nothing was published. Add the missing address and re-run." >&2
  exit 1
}

WORK="$(mktemp -d)"; trap 'rm -rf "${WORK}"' EXIT
say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

# `--fail` matters: without it an Intel 5xx writes its error body into tcb.json, and the
# failure resurfaces minutes later as an opaque revert from the DAO instead of as a fetch
# error. `--retry` rides out their rate limiter, which is the common case for a scheduled run.
fetch() { curl -sS --fail --retry 3 --retry-delay 2 --retry-connrefused --max-time 60 "$@"; }

# ---------------------------------------------------------------- fetch from Intel
say "fetching collateral from Intel for fmspc ${FMSPC}"
fetch -D "${WORK}/tcb.h"  -o "${WORK}/tcb.json"      "${TDX}/tcb?fmspc=${FMSPC}"
fetch -D "${WORK}/qe.h"   -o "${WORK}/qe.json"       "${TDX}/qe/identity"
fetch -D "${WORK}/plat.h" -o "${WORK}/plat.crl.der"  "${SGX}/pckcrl?ca=platform&encoding=der"
fetch -D "${WORK}/proc.h" -o "${WORK}/proc.crl.der"  "${SGX}/pckcrl?ca=processor&encoding=der"

# The issuer chains ride in response headers as URL-escaped PEM: intermediate, then root.
python3 - "${WORK}" <<'PY'
import os, re, sys, urllib.parse, subprocess
work = sys.argv[1]
def chain(header_file, name):
    for line in open(os.path.join(work, header_file), encoding='utf-8', errors='replace'):
        if line.lower().startswith(name.lower() + ':'):
            pem = urllib.parse.unquote(line.split(':', 1)[1].strip())
            blocks = re.findall(r'-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----', pem, re.S)
            return blocks
    return []
def der(pem_text, out):
    p = subprocess.run(['openssl','x509','-outform','DER'], input=pem_text.encode(),
                       capture_output=True)
    open(os.path.join(work, out), 'wb').write(p.stdout)

tcb = chain('tcb.h', 'TCB-Info-Issuer-Chain')
plat = chain('plat.h', 'SGX-PCK-CRL-Issuer-Chain')
proc = chain('proc.h', 'SGX-PCK-CRL-Issuer-Chain')
if len(tcb) >= 2:
    der(tcb[0], 'signing.der')   # TCB signing cert
    der(tcb[1], 'root.der')      # Intel SGX root CA
if len(plat) >= 2:
    der(plat[0], 'pckplat.der')  # PCK platform CA
# The processor CRL is issued by a different CA, so its cert has to be published too or
# upsertPckCrl(PROCESSOR) has nothing to verify the signature against.
if len(proc) >= 2:
    der(proc[0], 'pckproc.der')
print(f"  extracted: signing={len(tcb)>=2} root={len(tcb)>=2} "
      f"pck_platform={len(plat)>=2} pck_processor={len(proc)>=2}")
PY

# The root CA CRL is named by a distribution point inside the root certificate.
# `|| true` throughout: under `set -e` a grep that matches nothing kills the script, and the
# extension is not always present. Intel's published location is the fallback.
CRL_URL="$(openssl x509 -inform DER -in "${WORK}/root.der" -noout -ext crlDistributionPoints 2>/dev/null \
  | grep -oE 'https?://[^ ,]+' | head -1 || true)"
CRL_URL="${CRL_URL:-https://certificates.trustedservices.intel.com/IntelSGXRootCA.der}"
say "root CA CRL from ${CRL_URL}"
fetch -o "${WORK}/rootca.crl.der" "${CRL_URL}" || warn_no_crl=1

# Counted, not just printed: this ran to completion with a zero exit status while every
# artifact reported FAILED, so anything scheduling it saw a success.
FAILURES=0

# Sends are serialised with a pause. Firing these back to back races the node's nonce
# tracking and returns "replacement transaction underpriced", which looks identical to a
# rejected upsert and silently skips collateral.
#
# A rejection is also not always a failure: the DAOs refuse an artifact identical to the one
# already stored, so a re-run reports duplicates. That is reported separately, because
# "already current" and "could not publish" need very different responses at 3am.
send() {
  local out rc=0
  # `|| rc=$?` is required: under `set -e` a failing command substitution terminates the
  # script before the status can be inspected, which is how this silently stopped after the
  # first artifact.
  out="$(cast send "$1" "$2" "${@:3}" --rpc-url "${RPC}" --private-key "${PRIVATE_KEY}" --json 2>&1)" || rc=$?
  if [ $rc -eq 0 ]; then echo "    published"
  elif printf '%s' "$out" | grep -qiE "duplicate|already"; then echo "    already current"
  else
    echo "    FAILED: $(printf '%s' "$out" | grep -oiE "[a-z_ ]*(underpriced|revert|insufficient|unauthorized)[a-z_ ]*" | head -1 | tr -d '\n')"
    # An assignment, never `((FAILURES++))`: that returns 1 on the first increment and `set -e`
    # would kill the run at the very failure it is trying to record.
    FAILURES=$((FAILURES + 1))
  fi
  sleep 3
}
hexof() { printf '0x%s' "$(xxd -p -c 999999 "$1")"; }

# ---------------------------------------------------------------- publish
# CA enum: ROOT=0, PROCESSOR=1, PLATFORM=2, SIGNING=3
say "publishing certificates"
printf '  root CA        '; send "${PCS}" "upsertPcsCertificates(uint8,bytes)" 0 "$(hexof "${WORK}/root.der")"
printf '  TCB signing    '; send "${PCS}" "upsertPcsCertificates(uint8,bytes)" 3 "$(hexof "${WORK}/signing.der")"
printf '  PCK platform   '; send "${PCS}" "upsertPcsCertificates(uint8,bytes)" 2 "$(hexof "${WORK}/pckplat.der")"
printf '  PCK processor  '; send "${PCS}" "upsertPcsCertificates(uint8,bytes)" 1 "$(hexof "${WORK}/pckproc.der")"

say "publishing revocation lists"
[ -s "${WORK}/rootca.crl.der" ] && { printf '  root CA CRL    '; send "${PCS}" "upsertRootCACrl(bytes)" "$(hexof "${WORK}/rootca.crl.der")"; }
printf '  PCK platform   '; send "${PCS}" "upsertPckCrl(uint8,bytes)" 2 "$(hexof "${WORK}/plat.crl.der")"
printf '  PCK processor  '; send "${PCS}" "upsertPckCrl(uint8,bytes)" 1 "$(hexof "${WORK}/proc.crl.der")"

# cast cannot encode a tuple whose member is a JSON string: its argument parser trips over
# the embedded quotes. These three are encoded by hand instead. The signature covers the exact
# bytes Intel served, so the object is lifted from the response as a substring and never
# re-serialised.
encode_tuple() { # <selector> <json-file> <top-level-key>
  python3 - "$1" "$2" "$3" <<'PY'
import json, sys
sel, path, key = sys.argv[1], sys.argv[2], sys.argv[3]
def u(n): return n.to_bytes(32, "big")
def dyn(b): return u(len(b)) + b + b"\x00" * ((32 - len(b) % 32) % 32)
raw = open(path).read()
i = raw.index(f'"{key}"'); j = raw.index("{", i); depth = 0; k = j
while True:
    if raw[k] == "{": depth += 1
    elif raw[k] == "}":
        depth -= 1
        if depth == 0: break
    k += 1
body = raw[j:k + 1].encode()
sig = bytes.fromhex(json.loads(raw)["signature"])
tup = u(0x40) + u(0x40 + len(dyn(body))) + dyn(body) + dyn(sig)
print("0x" + (bytes.fromhex(sel[2:]) + u(0x20) + tup).hex())
PY
}

say "publishing QE identity"
QE_SEL="$(cast sig 'upsertEnclaveIdentity(uint256,uint256,(string,bytes))')"
# EnclaveId enum is QE=0, QVE=1, TD_QE=2; TDX quotes need TD_QE, and PCS api version 4.
python3 - "$QE_SEL" "${WORK}/qe.json" > "${WORK}/qe.calldata" <<'PY'
import json, sys
sel, path = sys.argv[1], sys.argv[2]
def u(n): return n.to_bytes(32, "big")
def dyn(b): return u(len(b)) + b + b"\x00" * ((32 - len(b) % 32) % 32)
raw = open(path).read()
i = raw.index('"enclaveIdentity"'); j = raw.index("{", i); depth = 0; k = j
while True:
    if raw[k] == "{": depth += 1
    elif raw[k] == "}":
        depth -= 1
        if depth == 0: break
    k += 1
body = raw[j:k + 1].encode()
sig = bytes.fromhex(json.loads(raw)["signature"])
tup = u(0x40) + u(0x40 + len(dyn(body))) + dyn(body) + dyn(sig)
print("0x" + (bytes.fromhex(sel[2:]) + u(2) + u(4) + u(0x60) + tup).hex())
PY
printf '  TDX QE identity  '; send "${QEID}" "$(cat "${WORK}/qe.calldata")"

say "publishing TCB evaluation data numbers"
EVAL_SEL="$(cast sig 'upsertTcbEvaluationData((string,bytes))')"
for plat in sgx tdx; do
  fetch -o "${WORK}/eval-${plat}.json" "https://api.trustedservices.intel.com/${plat}/certification/v4/tcbevaluationdatanumbers"
  printf '  %s eval numbers  ' "${plat}"
  send "${EVALDAO}" "$(encode_tuple "${EVAL_SEL}" "${WORK}/eval-${plat}.json" tcbEvaluationDataNumbers)"
done

say "publishing TCB info for ${FMSPC}"
TCB_SEL="$(cast sig 'upsertFmspcTcb((string,bytes))')"
printf '  TDX TCB info     '; send "${TCBDAO}" "$(encode_tuple "${TCB_SEL}" "${WORK}/tcb.json" tcbInfo)"

if [ "${FAILURES}" -gt 0 ]; then
  say "${FAILURES} artifact(s) could not be published on ${CHAIN}"
  exit 1
fi

say "done. re-run this when the thirty day validity window lapses."
