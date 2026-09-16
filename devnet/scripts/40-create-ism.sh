#!/usr/bin/env bash
# Create the TEE ISM on the local chain and point the mailbox at it.
#
# This is the step the ordering constraint forces last: the ISM pins an enclave's
# measurements, and those only exist once an enclave is running; and it pins an origin light
# client checkpoint, which is read live from the origin rather than hardcoded.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

wait_for_chain

ENCLAVE_URL="${ENCLAVE_URL:-$(load enclave-url)}"
SEPOLIA_BEACON="${SEPOLIA_BEACON:-https://ethereum-sepolia-beacon-api.publicnode.com}"
SEPOLIA_RPC="${SEPOLIA_RPC:-https://ethereum-sepolia-rpc.publicnode.com}"
# Hyperlane's canonical Sepolia merkle tree hook, left-padded to 32 bytes the way a Hyperlane
# message addresses it.
SEPOLIA_MERKLE_HOOK="${SEPOLIA_MERKLE_HOOK:-0x4917a9746A7B6E0A57159cCb7F5a6744247f2d0d}"

if has teeism-ism-id; then
  say "tee ism already created: $(load teeism-ism-id)"
  exit 0
fi

mkdir -p "${OUT_DIR}"

say "reading enclave measurements from ${ENCLAVE_URL}"
(cd "${REPO_DIR}/tee-circuit" && cargo run --quiet -p circuit-tool -- identity \
  --url "${ENCLAVE_URL}" --json "${OUT_DIR}/identity.json") >/dev/null
[ -s "${OUT_DIR}/identity.json" ] || die "circuit-tool wrote no identity"

digest="$("${BIN_DIR}/teeism-identity" -identity "${OUT_DIR}/identity.json")"
save identity-digest "${digest}"

# The checkpoint is read from the origin now, so the ISM starts from a head anyone can check
# rather than from a value baked into this script.
say "reading a trusted Sepolia checkpoint"
state="$(cd "${REPO_DIR}/tee-hyperlane" && cargo run --quiet -p tee-coprocessor -- bootstrap-ethereum \
  --beacon "${SEPOLIA_BEACON}" --execution "${SEPOLIA_RPC}" \
  --identity-digest "${digest}" 2>/dev/null \
  | sed -n 's/^genesis state *//p' | tr -d ' ')"
[ -n "${state}" ] || die "could not read a genesis state from bootstrap-ethereum"
save genesis-state "${state}"

# Left-pad the 20-byte EVM hook address to the 32 bytes a Hyperlane message carries.
hook32="0x000000000000000000000000$(printf '%s' "${SEPOLIA_MERKLE_HOOK}" | sed 's/^0x//' | tr 'A-F' 'a-f')"
save sepolia-merkle-hook "${hook32}"

python3 - "${OUT_DIR}" "${state}" "${hook32}" <<'PY'
import json, sys
out, state, hook = sys.argv[1], sys.argv[2], sys.argv[3]
json.dump({
    "state": state,
    "merkle_tree_address": hook,
    "identity": json.load(open(f"{out}/identity.json")),
}, open(f"{out}/ism.json", "w"), indent=2)
PY

say "creating the tee ism"
res="$(tx relayer teeism create "${OUT_DIR}/ism.json")"
ism="$(ev "${res}" "celestia.teeism.v1.EventCreateInterchainSecurityModule" "id")"
[ -n "${ism}" ] || die "could not read the ism id"
save teeism-ism-id "${ism}"

say "pointing the mailbox default ism at ${ism}"
tx relayer hyperlane mailbox set "$(load mailbox-id)" --default-ism "${ism}" >/dev/null

say "tee ism is live"
