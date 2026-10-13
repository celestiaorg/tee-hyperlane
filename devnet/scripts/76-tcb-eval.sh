#!/usr/bin/env bash
# Add a TCB evaluation number to an EVM chain's PCCS: deploy its QE identity and TCB info data
# contracts, let the seeding key upload to them, register them on the PCCSRouter, and seed.
#
# The verifier reads Intel's "standard" evaluation number, chosen by block time: it moves to
# a new number twelve months after Intel releases it (21 on about 10 Feb 2027, 22 on about
# 11 Aug 2027). A chain whose router has no contracts for the new number stops verifying that
# day, so run this for the next number well before, and keep seeding both until the old one
# is no longer standard. Owner transactions sign with EVM_SIGNER (a Ledger or EVM_PRIVATE_KEY).
#
#   usage: 76-tcb-eval.sh <chain> <evaluation number>     e.g. 76-tcb-eval.sh eden 21
#   env:   SEEDER   the address that seeds collateral each month (default: EVM_PRIVATE_KEY's)
#          DRY_RUN=1  check everything and say what would be sent, sending nothing
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need cast
need forge
need jq

CHAIN="${1:-}"; N="${2:-}"
[ -n "${CHAIN}" ] && [ -n "${N}" ] || die "usage: 76-tcb-eval.sh <chain> <evaluation number>, e.g. 76-tcb-eval.sh eden 21"
case "${N}" in *[!0-9]*) die "the evaluation number must be a number, not '${N}'" ;; esac

ADDR_FILE="${OUT_DIR}/pccs-${CHAIN}.json"
[ -f "${ADDR_FILE}" ] || die "no PCCS record for ${CHAIN} at ${ADDR_FILE}; Automata has to be deployed there first (DEPLOY appendix D)"
RPC="$(jq -r '.rpc // empty' "${ADDR_FILE}")"
ROUTER="$(jq -r '.PCCSRouter // empty' "${ADDR_FILE}")"
[ -n "${RPC}" ] && [ -n "${ROUTER}" ] || die "${ADDR_FILE} names no rpc or PCCSRouter"
AUTOMATA="${DEVNET_DIR}/automata/pccs"
ZERO=0x0000000000000000000000000000000000000000
lower() { printf '%s' "$1" | tr 'A-F' 'a-f'; }

# A read that fails is reported as what it was trying to learn.
call() { # <what> <to> <sig> [args...]
  local what="$1" out; shift
  out="$(cast call "$@" --rpc-url "${RPC}" 2>&1)" \
    || die "could not read ${what} from ${1} on ${CHAIN} (${RPC}): $(printf '%s' "${out}" | tail -1)"
  printf '%s' "${out}" | cut -d' ' -f1
}
# An owner transaction, reported by what it was for when it fails.
owner_send() { # <what> <to> <sig> [args...]
  local what="$1" out; shift
  if [ -n "${DRY_RUN:-}" ]; then say "DRY_RUN: would send ${what}: $*"; return; fi
  out="$(cast send "$@" --rpc-url "${RPC}" "${EVM_SIGN[@]}" 2>&1)" \
    || die "${what} failed on ${CHAIN}: $(printf '%s' "${out}" | grep -v '^[[:space:]]*$' | tail -1)"
  sleep 4
}

# ---------------------------------------------------------------- check before sending
numbers="$(curl -sS --fail --max-time 60 https://api.trustedservices.intel.com/tdx/certification/v4/tcbevaluationdatanumbers)" \
  || die "could not fetch Intel's TDX evaluation numbers"
INTEL="$(printf '%s' "${numbers}" | jq -r '[.tcbEvaluationDataNumbers.tcbEvalNumbers[].tcbEvaluationDataNumber] | map(tostring) | join(" ")')" \
  || die "Intel's evaluation-number list did not parse"
case " ${INTEL} " in *" ${N} "*) ;; *) die "Intel does not publish TDX evaluation ${N} (it publishes: ${INTEL})" ;; esac

evm_signer
CHAIN_ID="$(cast chain-id --rpc-url "${RPC}" 2>&1)" || die "could not reach ${CHAIN} at ${RPC}: ${CHAIN_ID}"
router_owner="$(call "the PCCSRouter's owner" "${ROUTER}" 'owner()(address)')"
[ "$(lower "${router_owner}")" = "$(lower "${EVM_ADDRESS}")" ] \
  || die "the PCCSRouter ${ROUTER} on ${CHAIN} is owned by ${router_owner}, not by this signer ${EVM_ADDRESS}; set EVM_SIGNER (and EVM_LEDGER_PATH) to the owner"
if [ -z "${SEEDER:-}" ]; then
  [ -n "${EVM_PRIVATE_KEY:-}" ] || die "set SEEDER to the address that seeds collateral each month, or EVM_PRIVATE_KEY"
  SEEDER="$(cast wallet address --private-key "${EVM_PRIVATE_KEY}")"
fi

qe="$(call "the evaluation-${N} QE identity contract" "${ROUTER}" 'qeIdDaoVersionedAddr(uint32)(address)' "${N}")"
tcb="$(call "the evaluation-${N} TCB data contract" "${ROUTER}" 'fmspcTcbDaoVersionedAddr(uint32)(address)' "${N}")"

# ---------------------------------------------------------------- deploy
if [ "$(lower "${qe}")" = "${ZERO}" ] || [ "$(lower "${tcb}")" = "${ZERO}" ]; then
  record="${AUTOMATA}/deployment/${CHAIN_ID}.json"
  [ -f "${record}" ] || die "no Automata deployment record at ${record}. deploy_versioned.sh reads the storage, PCS DAO and helper addresses from it: copy it from the host that deployed Automata on ${CHAIN}"
  for k in AutomataDaoStorage AutomataPcsDao EnclaveIdentityHelper FmspcTcbHelper PCKHelper X509CRLHelper; do
    [ -n "$(jq -r --arg k "${k}" '.[$k] // empty' "${record}")" ] || die "${record} names no ${k}; it is not the record of a complete Automata deployment"
  done
  storage="$(jq -r .AutomataDaoStorage "${record}")"
  storage_owner="$(call "the PCCS storage's owner" "${storage}" 'owner()(address)')"
  [ "$(lower "${storage_owner}")" = "$(lower "${EVM_ADDRESS}")" ] \
    || die "the PCCS storage ${storage} is owned by ${storage_owner}, not ${EVM_ADDRESS}; only its owner can let the new contracts write to it"

  if [ "${EVM_SIGNER:-key}" = ledger ]; then
    signer_env=(LEDGER=true OWNER="${EVM_ADDRESS}" LEDGER_PATH="${EVM_LEDGER_PATH:-}")
  else
    signer_env=(PRIVATE_KEY="${EVM_PRIVATE_KEY}")
  fi
  if [ -n "${DRY_RUN:-}" ]; then
    say "DRY_RUN: would deploy the evaluation-${N} data contracts with deploy_versioned.sh, grant ${SEEDER} ATTESTER_ROLE on them, register them on ${ROUTER} and seed"
    exit 0
  fi
  log="${STATE_DIR}/tcb-eval-${CHAIN}-${N}.log"
  say "deploying the evaluation-${N} data contracts on ${CHAIN} (log: ${log})"
  ( cd "${AUTOMATA}" && env RPC_URL="${RPC}" "${signer_env[@]}" ./script/automata/versioned/deploy_versioned.sh versioned "${N}" ) \
    > "${log}" 2>&1 \
    || { tail -15 "${log}" >&2; die "deploying the evaluation-${N} contracts failed; the log above is the end of ${log}"; }

  qe="$(jq -r --arg a "AutomataEnclaveIdentityDaoVersionedCrlV2_tcbeval_${N}" --arg b "AutomataEnclaveIdentityDaoVersioned_tcbeval_${N}" '.[$a] // .[$b] // empty' "${record}")"
  tcb="$(jq -r --arg a "AutomataFmspcTcbDaoVersioned_tcbeval_${N}" '.[$a] // empty' "${record}")"
  [ -n "${qe}" ] && [ -n "${tcb}" ] \
    || die "the deployment reported success but ${record} names no evaluation-${N} contracts; see ${log}"
else
  say "${CHAIN}'s router already has evaluation-${N} contracts: QE identity ${qe}, TCB info ${tcb}"
fi

for dao in "${qe}" "${tcb}"; do
  code="$(cast code "${dao}" --rpc-url "${RPC}" 2>&1)" || die "could not read ${dao}'s code on ${CHAIN}: ${code}"
  [ "${code}" != 0x ] || die "no contract at ${dao} on ${CHAIN}; the deployment did not land"
  got="$(call "the evaluation number of ${dao}" "${dao}" 'TCB_EVALUATION_NUMBER()(uint32)')"
  [ "${got}" = "${N}" ] || die "${dao} is built for evaluation ${got}, not ${N}"
done

# ---------------------------------------------------------------- roles and registration
for dao in "${qe}" "${tcb}"; do
  if [ "$(call "the seeder's role on ${dao}" "${dao}" 'hasAnyRole(address,uint256)(bool)' "${SEEDER}" 1)" != true ]; then
    say "granting ${SEEDER} ATTESTER_ROLE on ${dao}"
    owner_send "granting ATTESTER_ROLE on ${dao}" "${dao}" 'grantRoles(address,uint256)' "${SEEDER}" 1
  fi
done
if [ "$(lower "$(call "the router's evaluation-${N} QE identity contract" "${ROUTER}" 'qeIdDaoVersionedAddr(uint32)(address)' "${N}")")" != "$(lower "${qe}")" ]; then
  say "registering ${qe} on the router for evaluation ${N}"
  owner_send "registering the evaluation-${N} QE identity contract" "${ROUTER}" 'setQeIdDaoVersionedAddr(uint32,address)' "${N}" "${qe}"
fi
if [ "$(lower "$(call "the router's evaluation-${N} TCB data contract" "${ROUTER}" 'fmspcTcbDaoVersionedAddr(uint32)(address)' "${N}")")" != "$(lower "${tcb}")" ]; then
  say "registering ${tcb} on the router for evaluation ${N}"
  owner_send "registering the evaluation-${N} TCB data contract" "${ROUTER}" 'setFmspcTcbDaoVersionedAddr(uint32,address)' "${N}" "${tcb}"
fi
[ "$(lower "$(call "the router's evaluation-${N} QE identity contract" "${ROUTER}" 'qeIdDaoVersionedAddr(uint32)(address)' "${N}")")" = "$(lower "${qe}")" ] \
  && [ "$(lower "$(call "the router's evaluation-${N} TCB data contract" "${ROUTER}" 'fmspcTcbDaoVersionedAddr(uint32)(address)' "${N}")")" = "$(lower "${tcb}")" ] \
  || die "the router on ${CHAIN} does not report the evaluation-${N} contracts after registering them"

jq --arg q "${qe}" --arg t "${tcb}" --arg n "${N}" \
  '.["EnclaveIdentityDaoVersioned_" + $n] = $q | .["FmspcTcbDaoVersioned_" + $n] = $t' \
  "${ADDR_FILE}" > "${ADDR_FILE}.new" && mv "${ADDR_FILE}.new" "${ADDR_FILE}"

# ---------------------------------------------------------------- seed
if [ -n "${DRY_RUN:-}" ]; then
  DRY_RUN=1 PRIVATE_KEY="${SEED_PRIVATE_KEY:-${EVM_PRIVATE_KEY:-}}" "${DEVNET_DIR}/scripts/seed-evm-collateral.sh" "${CHAIN}"
  exit
fi
say "seeding every evaluation the router has contracts for"
PRIVATE_KEY="${SEED_PRIVATE_KEY:-${EVM_PRIVATE_KEY:-}}" "${DEVNET_DIR}/scripts/seed-evm-collateral.sh" "${CHAIN}" \
  || die "the evaluation-${N} contracts are registered, but seeding failed (above). Re-run: ./scripts/seed-evm-collateral.sh ${CHAIN}"
say "evaluation ${N} is ready on ${CHAIN}: QE identity ${qe}, TCB info ${tcb}"
