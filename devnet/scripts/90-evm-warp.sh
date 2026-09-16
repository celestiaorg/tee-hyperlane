#!/usr/bin/env bash
# Deploy the EVM side of the warp route on every chain, and enroll both directions.
#
# This runs after 80-evm-isms.sh because a router's security module is set at initialize
# time and never changed afterwards: the ISM has to exist before the router that points at
# it. Both are redeployed on every `make init`, since the ISM pins a fresh enclave and a
# fresh chain genesis, and a router left pointing at the previous cycle's ISM would accept
# nothing.
#
# Enrollment lives here rather than in 50-warp-celestia.sh so that one step owns both
# directions of one route. Splitting it across two steps meant the Celestia half ran before
# the EVM half existed, so it silently enrolled nothing.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need cast
need forge

CONTRACTS="${REPO_DIR}/tee-hyperlane/contracts"
: "${EVM_PRIVATE_KEY:?set EVM_PRIVATE_KEY}"

# Destination gas for a warp delivery. Quoted by the origin hook, which is the noop hook on
# this devnet, so the value is recorded but never charged.
WARP_DEST_GAS="${WARP_DEST_GAS:-50000}"

wait_for_chain

TOKEN="$(load celestia-token-id)"
DOMAIN="$(load celestia-domain)"

# chain : chain-id : hyperlane mailbox
CHAINS="arbitrum:421614:0x598facE78a4302f11E3de0bee1894Da0b2Cb71F8
base:84532:0x6966b0E55883d49BFB24539356a2f8A673E02039
sepolia:11155111:0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

# A here-string, not a pipe: a piped `while` runs in a subshell, where `die` exits only the
# subshell and `save` writes state the parent never sees.
while IFS=: read -r name chainid mailbox; do
  addr_file="${OUT_DIR}/pccs-${name}.json"
  [ -f "${addr_file}" ] || { warn "no PCCS on ${name}; skipping"; continue; }
  rpc="$(python3 -c "import json;print(json.load(open('${addr_file}'))['rpc'])")"
  has "ism-${name}" || { warn "no ISM on ${name}; skipping"; continue; }
  ism="$(load "ism-${name}")"

  # Reuse a router only if it is real code pointing at the current ISM. Checking the code
  # size and not just the saved address matters: a previous run recorded an address that a
  # simulated deployment had predicted but never broadcast, and every later step trusted it.
  router=""
  if has "${name}-router"; then
    cand="$(load "${name}-router")"
    code="$(cast code "${cand}" --rpc-url "${rpc}" 2>/dev/null || true)"
    cur="$(cast call "${cand}" "interchainSecurityModule()(address)" --rpc-url "${rpc}" 2>/dev/null || true)"
    if [ "${code}" != "0x" ] && [ -n "${code}" ] \
       && [ "$(printf '%s' "${cur}" | tr 'A-Z' 'a-z')" = "$(printf '%s' "${ism}" | tr 'A-Z' 'a-z')" ]; then
      say "${name} already has router ${cand} on this ISM, skipping"
      router="${cand}"
    fi
  fi

  if [ -z "${router}" ]; then
    say "deploying the synthetic TIA router on ${name}"
    out="$(cd "${CONTRACTS}" && MAILBOX="${mailbox}" TEE_ISM="${ism}" \
      ORIGIN_DOMAIN="${DOMAIN}" ORIGIN_ROUTER="${TOKEN}" \
      TOKEN_NAME="Celestia TIA" TOKEN_SYMBOL="TIA" TOKEN_DECIMALS=6 \
      forge script script/DeployWarpSynthetic.s.sol:DeployWarpSynthetic \
        --rpc-url "${rpc}" --private-key "${EVM_PRIVATE_KEY}" --broadcast --slow 2>&1)"
    router="$(printf '%s' "${out}" | sed -n 's/.*HypERC20 *\(0x[0-9a-fA-F]\{40\}\).*/\1/p' | tail -1)"
    if [ -z "${router}" ]; then
      printf '%s\n' "${out}" | tail -20 >&2
      die "router deployment failed on ${name}"
    fi

    # Prove it landed. `forge script` prints the address it simulated, which exists whether
    # or not the broadcast succeeded, so the printed address alone is not evidence.
    code=""
    for _ in $(seq 1 15); do
      code="$(cast code "${router}" --rpc-url "${rpc}" 2>/dev/null || true)"
      [ -n "${code}" ] && [ "${code}" != "0x" ] && break
      sleep 3
    done
    [ -n "${code}" ] && [ "${code}" != "0x" ] || die "no code at ${router} on ${name}; the broadcast did not land"
    save "${name}-router" "${router}"
    save "warp-${name}" "${router}"
  fi

  # EVM -> Celestia. Idempotent on its own: enrolling the same domain twice overwrites.
  got="$(cast call "${router}" "routers(uint32)(bytes32)" "${DOMAIN}" --rpc-url "${rpc}" 2>/dev/null || true)"
  if [ "${got}" != "${TOKEN}" ]; then
    say "enrolling the celestia token on ${name}"
    cast send "${router}" "enrollRemoteRouter(uint32,bytes32)" "${DOMAIN}" "${TOKEN}" \
      --rpc-url "${rpc}" --private-key "${EVM_PRIVATE_KEY}" >/dev/null \
      || die "enrollRemoteRouter failed on ${name}"
    got="$(cast call "${router}" "routers(uint32)(bytes32)" "${DOMAIN}" --rpc-url "${rpc}" 2>/dev/null || true)"
  fi
  [ "${got}" = "${TOKEN}" ] || die "${name} router points at '${got}', expected ${TOKEN}"
  say "  ${router} ism ${ism}"
done <<< "${CHAINS}"

# ---------------------------------------------------------------- Celestia -> EVM
#
# Read what the chain actually holds rather than a saved marker. The markers went stale the
# first time a router address changed underneath them, and the chain rejects enrolling a
# domain that is already enrolled, so a repoint has to unroll first.
enroll_from_celestia() {
  local domain="$1" name="$2"
  has "${name}-router" || { warn "no ${name} router; skipping"; return 0; }
  local router want got
  router="$(load "${name}-router")"
  want="0x000000000000000000000000$(printf '%s' "${router#0x}" | tr 'A-Z' 'a-z')"
  got="$(appd query warp remote-routers "${TOKEN}" --node "${CELESTIA_RPC}" -o json 2>/dev/null \
    | jq -r --argjson d "${domain}" '.remote_routers[]? | select(.receiver_domain == $d) | .receiver_contract' \
    | tr 'A-Z' 'a-z')"

  if [ "${got}" = "${want}" ]; then
    say "${name} already enrolled on domain ${domain}"
    return 0
  fi
  if [ -n "${got}" ]; then
    say "repointing domain ${domain} from ${got}"
    tx relayer warp unroll-remote-router "${TOKEN}" "${domain}" >/dev/null
  fi
  # The 32-byte form, not the 20-byte address: Hyperlane addresses are 32 bytes everywhere,
  # and this CLI rejects a bare address rather than padding it.
  say "enrolling ${name} router ${router} on domain ${domain}"
  tx relayer warp enroll-remote-router "${TOKEN}" "${domain}" "${want}" "${WARP_DEST_GAS}" >/dev/null
  save "enrolled-${domain}" "${router}"
}

enroll_from_celestia "${SEPOLIA_DOMAIN}"          sepolia
enroll_from_celestia "${BASE_SEPOLIA_DOMAIN}"     base
enroll_from_celestia "${ARBITRUM_SEPOLIA_DOMAIN}" arbitrum

say "evm warp side ready"
