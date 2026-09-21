#!/usr/bin/env bash
# Deploy a TeeDcapIsm on each EVM chain, pinned to the enclave this devnet just created.
#
# Every input is read live rather than from a checked-in constant, because all of them change
# when `make init` runs again: the enclave is a fresh CVM, and the origin checkpoint comes
# from a chain with a brand new genesis. That is why these are redeployed each cycle while the
# PCCS stack underneath them persists.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need cast
need forge

CONTRACTS="${REPO_DIR}/tee-hyperlane/contracts"
MAX_QUOTE_SKEW="${MAX_QUOTE_SKEW:-86400}"
: "${EVM_PRIVATE_KEY:?set EVM_PRIVATE_KEY}"

has enclave-url || die "no enclave; run 'make init' first"
has merkle-hook-id || die "no local hyperlane deployment; run 'make init' first"

# ---------------------------------------------------------------- pin the live enclave
say "reading measurements from the devnet enclave"
curl -sS -m 30 "$(load enclave-url)/identity" -o "${STATE_DIR}/enclave-identity.json"
MEASUREMENTS="$(python3 - "${STATE_DIR}/enclave-identity.json" <<'PY'
import json, subprocess, sys
q = json.load(open(sys.argv[1]))["quote"]
b = bytes.fromhex(q[2:] if q.startswith("0x") else q)[48:]
# mr_td ++ mr_config_id, then rtmr0..2. rtmr3 is excluded: it carries app-id and
# instance-id, so including it would tie the ISM to one CVM.
pre = "0x" + (b[136:232] + b[328:472]).hex()
print(subprocess.run(["cast", "keccak", pre], capture_output=True, text=True).stdout.strip())
PY
)"
IDENTITY="$(load identity-digest)"
say "  measurements  ${MEASUREMENTS}"
say "  identity      ${IDENTITY}"

# ---------------------------------------------------------------- pin the local chain
say "reading a trusted checkpoint from ${CHAINID}"
GENESIS="$(cd "${REPO_DIR}/tee-hyperlane" && cargo run --quiet --release -p tee-coprocessor -- \
  bootstrap-celestia --rpc "${CELESTIA_RPC}" --identity-digest "${IDENTITY}" 2>/dev/null \
  | sed -n 's/^genesis state *//p' | tr -d ' ')"
[ -n "${GENESIS}" ] || die "could not bootstrap from ${CHAINID}"
HOOK="$(load merkle-hook-id)"
say "  origin hook   ${HOOK}"

# ---------------------------------------------------------------- deploy
# chain : chain-id : hyperlane mailbox
CHAINS="arbitrum:421614:0x598facE78a4302f11E3de0bee1894Da0b2Cb71F8
base:84532:0x6966b0E55883d49BFB24539356a2f8A673E02039
sepolia:11155111:0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

# A here-string, not a pipe: `cmd | while` runs the loop in a subshell, so `die` only exits
# the subshell and `save` writes state the parent never sees. That is how this silently
# stopped after the second chain.
while IFS=: read -r name chainid mailbox; do
  addr_file="${OUT_DIR}/pccs-${name}.json"
  [ -f "${addr_file}" ] || { warn "no PCCS on ${name}; skipping"; continue; }
  rpc="$(python3 -c "import json;print(json.load(open('${addr_file}'))['rpc'])")"
  entry="$(python3 -c "import json;print(json.load(open('${addr_file}'))['AttestationEntrypoint'])")"

  # Skip a chain that already has an ISM pinning this enclave and this checkpoint. Without
  # this, re-running the step silently abandons the previous deployment and pays for another.
  if has "ism-${name}"; then
    existing="$(load "ism-${name}")"
    cur="$(cast call "${existing}" "enclaveMeasurements()(bytes32)" --rpc-url "${rpc}" 2>/dev/null || true)"
    if [ "${cur}" = "${MEASUREMENTS}" ]; then
      say "${name} already has ${existing} pinning this enclave, skipping"
      continue
    fi
  fi

  say "deploying TeeDcapIsm on ${name}"
  out="$(cd "${CONTRACTS}" && forge create src/TeeDcapIsm.sol:TeeDcapIsm \
    --rpc-url "${rpc}" --private-key "${EVM_PRIVATE_KEY}" --broadcast \
    --constructor-args "${entry}" "${MEASUREMENTS}" "${IDENTITY}" "${HOOK}" \
                       "${mailbox}" "${GENESIS}" "${MAX_QUOTE_SKEW}" 2>&1)"
  ism="$(printf '%s' "${out}" | grep -oE "Deployed to: 0x[0-9a-fA-F]{40}" | grep -oE "0x[0-9a-fA-F]{40}")"
  if [ -z "${ism}" ]; then
    printf '%s\n' "${out}" | tail -5 >&2
    die "TeeDcapIsm deployment failed on ${name}"
  fi
  save "ism-${name}" "${ism}"
  # Prove it is live and pinned to the enclave we just measured, before moving on.
  #
  # Retried: a read issued immediately after deployment can return empty while the node
  # catches up, and an empty answer here previously aborted the whole run after the second
  # chain, leaving the third undeployed and looking like success.
  got=""
  for _ in $(seq 1 10); do
    got="$(cast call "${ism}" "enclaveMeasurements()(bytes32)" --rpc-url "${rpc}" 2>/dev/null || true)"
    [ -n "${got}" ] && break
    sleep 3
  done
  [ "${got}" = "${MEASUREMENTS}" ] || die "deployed ISM on ${name} pins '${got}', expected ${MEASUREMENTS}"
  say "  ${ism} verified"
done <<< "${CHAINS}"

say "EVM ISMs deployed"
