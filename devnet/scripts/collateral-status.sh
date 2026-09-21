#!/usr/bin/env bash
# What Intel collateral each EVM chain's PCCS is holding, and when it goes stale.
#
# Read only. No key, no gas, no transactions: every call here is an `eth_call` against a
# public view function, so this is safe to run from anywhere, on a timer, or at 3am before
# deciding whether anything is actually wrong.
#
# The counterpart to seed-evm-collateral.sh, which is what you run when this says to. Intel's
# TCB info, QE identity, evaluation numbers and both PCK CRLs are valid for thirty days; once
# they lapse the on-chain verifier returns TCBR or PCKCRLH and the Celestia-to-EVM routes stop.
#
#   usage: collateral-status.sh [chain...]        default: every chain with a PCCS record
#   env:   FMSPC (default 20a06f000000), WARN_DAYS (default 14),
#          RPC_<CHAIN> to override one endpoint, e.g. RPC_BASE=https://...
#   exit:  0  everything valid for more than WARN_DAYS
#          1  something is expiring, already expired, missing, or the evaluation number moved
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need cast
need curl

FMSPC="${FMSPC:-20a06f000000}"
WARN_DAYS="${WARN_DAYS:-14}"

CHAINS=("$@")
if [ ${#CHAINS[@]} -eq 0 ]; then
  for f in "${OUT_DIR}"/pccs-*.json; do
    [ -f "$f" ] || continue
    name="$(basename "$f" .json)"; CHAINS+=("${name#pccs-}")
  done
fi
[ ${#CHAINS[@]} -gt 0 ] || die "no PCCS records in ${OUT_DIR}; run step 8 of DEPLOY.md first"

# What Intel is serving right now, for the evaluation-number check below. Advisory only: a
# reachability problem with Intel is not a reason to call the on-chain state bad, so this
# failing leaves INTEL_EVAL empty and the check is skipped rather than reported as a fault.
INTEL_EVAL="$(curl -sS --fail --max-time 20 \
  "https://api.trustedservices.intel.com/tdx/certification/v4/tcb?fmspc=${FMSPC}" 2>/dev/null \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["tcbInfo"]["tcbEvaluationDataNumber"])' 2>/dev/null || true)"

# rpc_for <chain> <record> - the endpoint to read through.
#
# A free public endpoint by default, not the one in the PCCS record. Those name a metered key
# chosen for deploying, and this script fires sixty-odd eth_calls a run: pointing a status
# check at a quota is how the quota gets spent on status checks. It also means anyone can run
# this without holding a key at all. RPC_<CHAIN> overrides, and a chain with no default here
# falls back to the record.
rpc_for() {
  local chain="$1" record="$2" override
  override="RPC_$(printf '%s' "${chain}" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!override:-}" ]; then printf '%s' "${!override}"; return; fi
  case "${chain}" in
    sepolia)  printf '%s' "https://ethereum-sepolia-rpc.publicnode.com" ;;
    arbitrum) printf '%s' "https://sepolia-rollup.arbitrum.io/rpc" ;;
    base)     printf '%s' "https://sepolia.base.org" ;;
    *)        field "${record}" rpc ;;
  esac
}

# field <file> <name> - one address out of a PCCS record, empty if the record omits it.
#
# Not every record carries every name: pccs-arbitrum.json has no TcbEvalDao, so reading these
# with a bare subscript aborts the run on a KeyError. A missing name is a gap in the record,
# not a fault on chain, and is reported as its own line rather than as an outage.
field() {
  python3 -c "import json,sys;print(json.load(open(sys.argv[1])).get(sys.argv[2],''))" "$1" "$2"
}

# key <address> <signature> <args...> - call a *_KEY view to derive a storage key.
#
# Derived on chain rather than recomputed here on purpose: the versioned DAOs override these
# to fold the evaluation number in, so a local keccak would silently look up the wrong slot
# and report healthy collateral as missing.
key() {
  local addr="$1" sig="$2"; shift 2
  cast call "${addr}" "${sig}" "$@" --rpc-url "${RPC}" 2>/dev/null | head -1
}

# validity <label> <dao> <key> - append "label|notBefore|notAfter" to ROWS.
validity() {
  local label="$1" dao="$2" k="$3" out nb na
  if [ -z "${k}" ] || [ "${k}" = "0x" ]; then
    ROWS+=("${label}|err|could not derive the storage key")
    return
  fi
  out="$(cast call "${dao}" "getCollateralValidity(bytes32)(uint64,uint64)" "${k}" \
    --rpc-url "${RPC}" 2>&1)" || { ROWS+=("${label}|err|$(printf '%s' "${out}" | head -1 | cut -c1-60)"); return; }
  nb="$(printf '%s' "${out}" | sed -n 1p | awk '{print $1}')"
  na="$(printf '%s' "${out}" | sed -n 2p | awk '{print $1}')"
  ROWS+=("${label}|${nb:-0}|${na:-0}")
}

FAILED=0

for CHAIN in "${CHAINS[@]}"; do
  ADDR_FILE="${OUT_DIR}/pccs-${CHAIN}.json"
  if [ ! -f "${ADDR_FILE}" ]; then
    warn "no PCCS record for ${CHAIN} at ${ADDR_FILE}; skipping"
    FAILED=1
    continue
  fi

  RPC="$(rpc_for "${CHAIN}" "${ADDR_FILE}")"
  PCS="$(field "${ADDR_FILE}" PcsDao)"
  QEID="$(field "${ADDR_FILE}" EnclaveIdentityDaoVersioned)"
  TCBDAO="$(field "${ADDR_FILE}" FmspcTcbDaoVersioned)"
  EVALDAO="$(field "${ADDR_FILE}" TcbEvalDao)"
  for required in RPC PCS QEID TCBDAO; do
    [ -n "${!required}" ] || die "${ADDR_FILE} names no ${required}; the record is incomplete"
  done

  say "${CHAIN}"
  ROWS=()

  # The four artifacts on a thirty day clock.
  validity "TDX TCB info v3"   "${TCBDAO}" "$(key "${TCBDAO}" 'FMSPC_TCB_KEY(uint8,bytes6,uint32)(bytes32)' 1 "0x${FMSPC}" 3)"
  validity "TD_QE identity v4" "${QEID}"   "$(key "${QEID}"   'ENCLAVE_ID_KEY(uint256,uint256)(bytes32)' 2 4)"
  if [ -n "${EVALDAO}" ]; then
    validity "SGX eval numbers"  "${EVALDAO}" "$(key "${EVALDAO}" 'TCB_EVAL_KEY(uint8)(bytes32)' 0)"
    validity "TDX eval numbers"  "${EVALDAO}" "$(key "${EVALDAO}" 'TCB_EVAL_KEY(uint8)(bytes32)' 1)"
  else
    ROWS+=("SGX eval numbers|err|no TcbEvalDao in ${ADDR_FILE##*/}")
    ROWS+=("TDX eval numbers|err|no TcbEvalDao in ${ADDR_FILE##*/}")
  fi

  # CA enum: ROOT=0, PROCESSOR=1, PLATFORM=2, SIGNING=3. The CRLs move, the certificates do
  # not, but both are listed: a missing certificate is the same outage as an expired CRL and
  # reads identically from a rejected quote.
  validity "root CA CRL"       "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 0 true)"
  validity "PCK processor CRL" "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 1 true)"
  validity "PCK platform CRL"  "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 2 true)"
  validity "root CA cert"      "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 0 false)"
  validity "TCB signing cert"  "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 3 false)"
  validity "PCK processor cert" "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 1 false)"
  validity "PCK platform cert" "${PCS}" "$(key "${PCS}" 'PCS_KEY(uint8,bool)(bytes32)' 2 false)"

  # Formatted in python3 rather than with `date`: the flag for "seconds since epoch" is -r on
  # BSD and -d @ on GNU, and this script has to read the same on a laptop and on the server.
  printf '%s\n' "${ROWS[@]}" | python3 -c '
import sys, time
warn_days = int(sys.argv[1])
now = time.time()
worst = 0          # 0 ok, 1 expiring, 2 expired or missing
for line in sys.stdin:
    label, a, b = line.rstrip("\n").split("|", 2)
    if a == "err":
        print("  %-19s %-8s %s" % (label, "ERROR", b)); worst = max(worst, 2); continue
    nb, na = int(a), int(b)
    if na == 0:
        print("  %-19s %-8s nothing published" % (label, "MISSING")); worst = max(worst, 2); continue
    left = (na - now) / 86400
    when = time.strftime("%Y-%m-%d %H:%MZ", time.gmtime(na))
    if left < 0:
        state, sev = "EXPIRED", 2
    elif left < warn_days:
        state, sev = "EXPIRING", 1
    else:
        state, sev = "ok", 0
    worst = max(worst, sev)
    print("  %-19s %-8s %s  (%dd)" % (label, state, when, left))
sys.exit(worst)
' "${WARN_DAYS}" || FAILED=1

  # The evaluation number is the failure that looks exactly like ordinary expiry and is not.
  # The versioned DAOs pin one; when Intel's standard feed moves past it, fresh TCB info
  # arrives stamped with the next number and the DAO rejects it, while the router looks up a
  # DAO for that number which does not exist. Symptom: TCBR. Fix: a new versioned DAO per
  # chain and a router repoint, not a re-seed.
  if [ -n "${INTEL_EVAL}" ]; then
    pinned="$(cast call "${TCBDAO}" "TCB_EVALUATION_NUMBER()(uint32)" --rpc-url "${RPC}" 2>/dev/null | head -1)"
    if [ -n "${pinned}" ] && [ "${pinned}" != "${INTEL_EVAL}" ]; then
      printf '  %-19s %-8s pinned %s, Intel now serves %s\n' "eval number" "MOVED" "${pinned}" "${INTEL_EVAL}"
      warn "a re-seed will not fix this; see 'Bringing up a fourth EVM chain' in DEPLOY.md"
      FAILED=1
    else
      printf '  %-19s %-8s %s\n' "eval number" "ok" "${pinned:-unknown}"
    fi
  fi
  echo
done

if [ "${FAILED}" -ne 0 ]; then
  # Deliberately not "run this to fix it". EXPIRING and EXPIRED are what a re-seed fixes;
  # ERROR, MISSING and MOVED are not, and saying otherwise sends someone to spend gas on a
  # job that cannot help.
  say "action needed"
  echo "  EXPIRING / EXPIRED    republish, one command per chain:"
  for CHAIN in "${CHAINS[@]}"; do echo "                          devnet/scripts/seed-evm-collateral.sh ${CHAIN}"; done
  echo "  MISSING               that artifact was never published; republish also covers it"
  echo "  ERROR                 the record or the endpoint is wrong, not the collateral"
  echo "  MOVED                 Intel advanced the evaluation number; a re-seed cannot fix it"
  exit 1
fi

say "all collateral valid for more than ${WARN_DAYS} days"
