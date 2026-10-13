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
# TCB info and QE identity are published once per TCB evaluation number, into the data
# contracts the chain's PCCSRouter has registered for that number. The verifier reads the
# number Intel calls "standard", which moves to the next one twelve months after Intel
# releases it (21 on about 10 Feb 2027), so the next number's contracts have to be deployed
# (76-tcb-eval.sh) and seeded before then or every route into the chain stops that day.
#
#   usage: seed-evm-collateral.sh <chain>
#   env:   TCB_EVALS   evaluation numbers to publish, e.g. "20 21". Default: every number Intel
#                      publishes that the chain's router has data contracts for.
#          DRY_RUN=1   check everything and fetch from Intel, but send nothing
#          FMSPC (default 20a06f000000), PRIVATE_KEY or EVM_PRIVATE_KEY
#
# The key needs no ownership of anything: only ATTESTER_ROLE on the versioned DAOs and the
# TcbEvalDao, which the owner grants once and which can only upload Intel-signed data.
set -Eeuo pipefail

CHAIN="${1:?usage: seed-evm-collateral.sh <sepolia|arbitrum|base|eden>}"
FMSPC="${FMSPC:-20a06f000000}"
DRY_RUN="${DRY_RUN:-}"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror\033[0m %s\n' "$*" >&2; exit 1; }
trap 'printf "\033[1;31merror\033[0m %s:%s: \`%s\` failed (exit %s)\n" "${0##*/}" "${LINENO}" "${BASH_COMMAND}" "$?" >&2' ERR

# `lib.sh` exports EVM_PRIVATE_KEY, this script has always read PRIVATE_KEY. Under `set -u` an
# unattended run died on the unbound name several minutes in, at the first send, rather than
# saying up front which variable it wanted. Either name works, with or without the 0x.
PRIVATE_KEY="${PRIVATE_KEY:-${EVM_PRIVATE_KEY:-}}"
[ -n "${PRIVATE_KEY}" ] || die "set PRIVATE_KEY or EVM_PRIVATE_KEY: the key that seeds (it needs ATTESTER_ROLE, no ownership)"
case "${PRIVATE_KEY}" in 0x*) ;; *) PRIVATE_KEY="0x${PRIVATE_KEY}" ;; esac
SEEDER="$(cast wallet address --private-key "${PRIVATE_KEY}" 2>&1)" || die "PRIVATE_KEY is not a valid key: ${SEEDER}"
SGX=https://api.trustedservices.intel.com/sgx/certification/v4
TDX=https://api.trustedservices.intel.com/tdx/certification/v4
ZERO=0x0000000000000000000000000000000000000000
ATTESTER_ROLE=1

DEVNET_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="${STATE_DIR:-${DEVNET_DIR}/.state}"
ADDR_FILE="${STATE_DIR}/out/pccs-${CHAIN}.json"
[ -f "${ADDR_FILE}" ] || die "no PCCS addresses for ${CHAIN} at ${ADDR_FILE}; copy it from the host that deployed Automata there (DEPLOY appendix D)"

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
EVALDAO="$(field TcbEvalDao)"
ROUTER="$(field PCCSRouter)"

# The variable name and the record's name for it are split before the indirection, not
# inside it: `${!pair%%:*}` expands a variable literally called "RPC:rpc", finds nothing, and
# reports every field missing on a record that is perfectly complete.
missing=""
for pair in RPC:rpc PCS:PcsDao EVALDAO:TcbEvalDao ROUTER:PCCSRouter; do
  var="${pair%%:*}"
  [ -n "${!var:-}" ] || missing="${missing} ${pair#*:}"
done
[ -z "${missing}" ] || die "${ADDR_FILE} names no:${missing}. Nothing was published; add the missing address and re-run."

WORK="$(mktemp -d)"; trap 'rm -rf "${WORK}"' EXIT

# `--fail` matters: without it an Intel 5xx writes its error body into tcb.json, and the
# failure resurfaces minutes later as an opaque revert from the DAO instead of as a fetch
# error. `--retry` rides out their rate limiter, which is the common case for a scheduled run.
fetch() { curl -sS --fail --retry 3 --retry-delay 2 --retry-connrefused --max-time 60 "$@"; }

# A read that fails is reported as what it was trying to learn, not as a bare cast error.
call() { # <what> <to> <sig> [args...]
  local what="$1" out; shift
  out="$(cast call "$@" --rpc-url "${RPC}" 2>&1)" \
    || die "could not read ${what} from ${1} on ${CHAIN} (${RPC}): $(printf '%s' "${out}" | tail -1)"
  printf '%s' "${out}" | cut -d' ' -f1
}
lower() { printf '%s' "$1" | tr 'A-F' 'a-f'; }

# ---------------------------------------------------------------- check before sending
say "checking ${CHAIN}: router ${ROUTER}, seeder ${SEEDER}"
code="$(cast code "${ROUTER}" --rpc-url "${RPC}" 2>&1)" || die "could not reach ${CHAIN} at ${RPC}: $(printf '%s' "${code}" | tail -1)"
[ "${code}" != 0x ] || die "no contract at PCCSRouter ${ROUTER} on ${CHAIN}; ${ADDR_FILE} is for another deployment"

fetch -o "${WORK}/eval-tdx.json" "${TDX}/tcbevaluationdatanumbers" \
  || die "could not fetch Intel's TDX evaluation numbers from ${TDX}/tcbevaluationdatanumbers"
INTEL_EVALS="$(python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))["tcbEvaluationDataNumbers"]["tcbEvalNumbers"]
print(" ".join(str(x["tcbEvaluationDataNumber"]) for x in d))' "${WORK}/eval-tdx.json")" \
  || die "Intel's evaluation-number list did not parse; see ${TDX}/tcbevaluationdatanumbers"
fetch -D "${WORK}/tcb.h" -o "${WORK}/tcb.json" "${TDX}/tcb?fmspc=${FMSPC}" \
  || die "could not fetch TCB info for fmspc ${FMSPC} from Intel"
STANDARD="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["tcbInfo"]["tcbEvaluationDataNumber"])' "${WORK}/tcb.json")" \
  || die "Intel's TCB info for fmspc ${FMSPC} did not parse"

if [ -z "${TCB_EVALS:-}" ]; then
  TCB_EVALS=""
  for n in ${INTEL_EVALS}; do
    [ "$(lower "$(call "the evaluation-${n} TCB data contract" "${ROUTER}" 'fmspcTcbDaoVersionedAddr(uint32)(address)' "${n}")")" = "${ZERO}" ] \
      || TCB_EVALS="${TCB_EVALS} ${n}"
  done
  [ -n "${TCB_EVALS}" ] || die "the PCCSRouter on ${CHAIN} has data contracts for none of Intel's evaluation numbers (${INTEL_EVALS}). Deploy them: ./scripts/76-tcb-eval.sh ${CHAIN} ${STANDARD}"
fi

# true, false, or unknown for a contract without roles, which anyone may upload to.
has_role() {
  cast call "$1" 'hasAnyRole(address,uint256)(bool)' "${SEEDER}" "${ATTESTER_ROLE}" --rpc-url "${RPC}" 2>/dev/null || echo unknown
}

# One "n:qe:tcb" entry per evaluation number; plain words rather than an associative array,
# which the bash macOS ships does not have.
PLAN=""
problems=()
case " ${TCB_EVALS} " in *" ${STANDARD} "*) ;; *)
  problems+=("evaluation ${STANDARD} is Intel's standard today, which is what the verifier reads, but it is not among the numbers to publish (${TCB_EVALS# }). Add it to TCB_EVALS, or, if the router has no contracts for it, deploy them: ./scripts/76-tcb-eval.sh ${CHAIN} ${STANDARD}") ;;
esac
for n in ${TCB_EVALS}; do
  case " ${INTEL_EVALS} " in *" ${n} "*) ;; *)
    problems+=("Intel does not publish TDX evaluation ${n} (it publishes: ${INTEL_EVALS})"); continue ;;
  esac
  qe="$(call "the evaluation-${n} QE identity contract" "${ROUTER}" 'qeIdDaoVersionedAddr(uint32)(address)' "${n}")"
  tcb="$(call "the evaluation-${n} TCB data contract" "${ROUTER}" 'fmspcTcbDaoVersionedAddr(uint32)(address)' "${n}")"
  if [ "$(lower "${qe}")" = "${ZERO}" ] || [ "$(lower "${tcb}")" = "${ZERO}" ]; then
    problems+=("the PCCSRouter on ${CHAIN} has no data contracts registered for evaluation ${n} (QE identity ${qe}, TCB info ${tcb}). Deploy and register them: ./scripts/76-tcb-eval.sh ${CHAIN} ${n}")
    continue
  fi
  for dao in "${qe}" "${tcb}"; do
    got="$(call "the evaluation number of ${dao}" "${dao}" 'TCB_EVALUATION_NUMBER()(uint32)')"
    [ "${got}" = "${n}" ] || problems+=("${dao}, registered on the router for evaluation ${n}, is built for evaluation ${got}; register the right contract")
    [ "$(has_role "${dao}")" != false ] || problems+=("${SEEDER} lacks ATTESTER_ROLE on ${dao} (evaluation ${n}). The owner grants it: cast send ${dao} 'grantRoles(address,uint256)' ${SEEDER} ${ATTESTER_ROLE} --rpc-url ${RPC} \$SIGN")
  done
  PLAN="${PLAN} ${n}:${qe}:${tcb}"
done
[ "$(has_role "${EVALDAO}")" != false ] || problems+=("${SEEDER} lacks ATTESTER_ROLE on the TcbEvalDao ${EVALDAO}. The owner grants it: cast send ${EVALDAO} 'grantRoles(address,uint256)' ${SEEDER} ${ATTESTER_ROLE} --rpc-url ${RPC} \$SIGN")
if [ "${#problems[@]}" -gt 0 ]; then
  printf '\033[1;31merror\033[0m nothing was published on %s:\n' "${CHAIN}" >&2
  printf '  - %s\n' "${problems[@]}" >&2
  exit 1
fi
say "publishing evaluations:${TCB_EVALS} (Intel's standard is ${STANDARD})"

# ---------------------------------------------------------------- fetch from Intel
say "fetching collateral from Intel for fmspc ${FMSPC}"
fetch -D "${WORK}/plat.h" -o "${WORK}/plat.crl.der"  "${SGX}/pckcrl?ca=platform&encoding=der" \
  || die "could not fetch the PCK platform CRL from Intel"
fetch -D "${WORK}/proc.h" -o "${WORK}/proc.crl.der"  "${SGX}/pckcrl?ca=processor&encoding=der" \
  || die "could not fetch the PCK processor CRL from Intel"
for n in ${TCB_EVALS}; do
  fetch -o "${WORK}/tcb-${n}.json" "${TDX}/tcb?fmspc=${FMSPC}&tcbEvaluationDataNumber=${n}" \
    || die "Intel would not serve evaluation-${n} TCB info for fmspc ${FMSPC}; it may have retired that number"
  fetch -o "${WORK}/qe-${n}.json" "${TDX}/qe/identity?tcbEvaluationDataNumber=${n}" \
    || die "Intel would not serve the evaluation-${n} QE identity; it may have retired that number"
done

# The issuer chains ride in response headers as URL-escaped PEM: intermediate, then root.
python3 - "${WORK}" <<'PY' || die "the certificates Intel sent could not be extracted; nothing was published"
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
                       capture_output=True, check=True)
    open(os.path.join(work, out), 'wb').write(p.stdout)

tcb = chain('tcb.h', 'TCB-Info-Issuer-Chain')
plat = chain('plat.h', 'SGX-PCK-CRL-Issuer-Chain')
proc = chain('proc.h', 'SGX-PCK-CRL-Issuer-Chain')
missing = [n for n, c in (("TCB info", tcb), ("PCK platform CRL", plat), ("PCK processor CRL", proc)) if len(c) < 2]
if missing:
    sys.exit("Intel's " + " and ".join(missing) + " response carried no issuer chain")
der(tcb[0], 'signing.der')   # TCB signing cert
der(tcb[1], 'root.der')      # Intel SGX root CA
der(plat[0], 'pckplat.der')  # PCK platform CA
# The processor CRL is issued by a different CA, so its cert has to be published too or
# upsertPckCrl(PROCESSOR) has nothing to verify the signature against.
der(proc[0], 'pckproc.der')
PY

# The root CA CRL is named by a distribution point inside the root certificate.
# `|| true` throughout: under `set -e` a grep that matches nothing kills the script, and the
# extension is not always present. Intel's published location is the fallback.
CRL_URL="$(openssl x509 -inform DER -in "${WORK}/root.der" -noout -ext crlDistributionPoints 2>/dev/null \
  | grep -oE 'https?://[^ ,]+' | head -1 || true)"
CRL_URL="${CRL_URL:-https://certificates.trustedservices.intel.com/IntelSGXRootCA.der}"
say "root CA CRL from ${CRL_URL}"
fetch -o "${WORK}/rootca.crl.der" "${CRL_URL}" \
  || warn "could not fetch the root CA CRL from ${CRL_URL}; publishing everything else"

if [ -n "${DRY_RUN}" ]; then
  say "DRY_RUN: every check passed and Intel served every artifact; nothing was sent"
  exit 0
fi

# Counted, not just printed: this ran to completion with a zero exit status while every
# artifact reported FAILED, so anything scheduling it saw a success.
FAILURES=0

# What a revert means, for the reverts an operator can act on. Solady's Unauthorized is what a
# missing role raises; FORBIDDEN is the storage refusing a DAO it was never told to trust.
explain() {
  case "$1" in
    *0x82b42900*|*Unauthorized*) echo " (the sending key lacks ATTESTER_ROLE on this contract)" ;;
    *FORBIDDEN*) echo " (the storage contract does not trust this DAO: its owner must grantDao it)" ;;
    *xpired*) echo " (Intel's artifact or its issuer is outside its validity window)" ;;
    *Out_Of_Date*) echo " (the contract already holds a newer artifact)" ;;
    *Missing_Issuer*|*Missing_TCB_Cert*) echo " (its issuing certificate is not published yet)" ;;
    *nsufficient*) echo " (the sending key cannot pay for gas)" ;;
    *) ;;
  esac
}

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
    echo "    FAILED: $(printf '%s' "$out" | grep -v '^[[:space:]]*$' | tail -1 | cut -c1-240)$(explain "${out}")"
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
if [ -s "${WORK}/rootca.crl.der" ]; then
  printf '  root CA CRL    '; send "${PCS}" "upsertRootCACrl(bytes)" "$(hexof "${WORK}/rootca.crl.der")"
fi
printf '  PCK platform   '; send "${PCS}" "upsertPckCrl(uint8,bytes)" 2 "$(hexof "${WORK}/plat.crl.der")"
printf '  PCK processor  '; send "${PCS}" "upsertPckCrl(uint8,bytes)" 1 "$(hexof "${WORK}/proc.crl.der")"

# cast cannot encode a tuple whose member is a JSON string: its argument parser trips over
# the embedded quotes. These are encoded by hand instead. The signature covers the exact bytes
# Intel served, so the object is lifted from the response as a substring and never
# re-serialised. With a <prefix>, the tuple follows those words (the QE identity's id and
# version) rather than standing alone.
encode_tuple() { # <selector> <json-file> <top-level-key> [prefix words...]
  python3 - "$@" <<'PY'
import json, sys
sel, path, key, prefix = sys.argv[1], sys.argv[2], sys.argv[3], [int(x) for x in sys.argv[4:]]
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
head = b"".join(u(p) for p in prefix) + u(0x20 * (len(prefix) + 1))
print("0x" + (bytes.fromhex(sel[2:]) + head + tup).hex())
PY
}

say "publishing TCB evaluation data numbers"
EVAL_SEL="$(cast sig 'upsertTcbEvaluationData((string,bytes))')"
fetch -o "${WORK}/eval-sgx.json" "https://api.trustedservices.intel.com/sgx/certification/v4/tcbevaluationdatanumbers" \
  || die "could not fetch Intel's SGX evaluation numbers"
for plat in sgx tdx; do
  printf '  %s eval numbers  ' "${plat}"
  send "${EVALDAO}" "$(encode_tuple "${EVAL_SEL}" "${WORK}/eval-${plat}.json" tcbEvaluationDataNumbers)"
done

# EnclaveId enum is QE=0, QVE=1, TD_QE=2; TDX quotes need TD_QE, and PCS api version 4.
QE_SEL="$(cast sig 'upsertEnclaveIdentity(uint256,uint256,(string,bytes))')"
TCB_SEL="$(cast sig 'upsertFmspcTcb((string,bytes))')"
for entry in ${PLAN}; do
  n="${entry%%:*}"; rest="${entry#*:}"; qe="${rest%%:*}"; tcb="${rest#*:}"
  say "publishing evaluation ${n}"
  printf '  TDX QE identity  '; send "${qe}" "$(encode_tuple "${QE_SEL}" "${WORK}/qe-${n}.json" enclaveIdentity 2 4)"
  printf '  TDX TCB info     '; send "${tcb}" "$(encode_tuple "${TCB_SEL}" "${WORK}/tcb-${n}.json" tcbInfo)"
done

[ "${FAILURES}" -eq 0 ] || die "${FAILURES} artifact(s) could not be published on ${CHAIN}; the reasons are above"

say "done. re-run this when the thirty day validity window lapses."
