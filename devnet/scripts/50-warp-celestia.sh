#!/usr/bin/env bash
# Create the Celestia side of the warp routes.
#
# Two assets, deliberately pointing opposite ways, because lock/unlock and mint/burn are
# different code paths and one asset only exercises half of each:
#
#   TIA   Celestia native. Collateral here, synthetic on every EVM chain.
#         Sending locks here and mints there; receiving burns there and unlocks here.
#   USDC  EVM native. Synthetic here, collateral on Sepolia.
#         Sending burns here and unlocks there; receiving locks there and mints here.
#
# Carrying both means a change that breaks one direction cannot pass unnoticed.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

wait_for_chain

# Point a token at the TEE ISM explicitly rather than relying on the mailbox default, so that
# adding another ISM later cannot silently change what secures the route.
point_at_ism() {
  local token="$1"
  has ism-celestia-sepolia || return 0
  say "pointing ${token} at the tee ism"
  tx relayer warp set-token "${token}" --ism-id "$(load ism-celestia-sepolia)" >/dev/null 2>&1 \
    || warn "could not set the token ism; the mailbox default still applies"
}

if has celestia-token-id; then
  say "TIA token already created: $(load celestia-token-id)"
else
  say "creating the TIA collateral token"
  res="$(tx relayer warp create-collateral-token "$(load mailbox-id)" utia)"
  token="$(ev "${res}" "hyperlane.warp.v1.EventCreateCollateralToken" "token_id")"
  [ -n "${token}" ] || die "could not read the TIA token id"
  save celestia-token-id "${token}"
  point_at_ism "${token}"
fi

if has celestia-usdc-token-id; then
  say "USDC token already created: $(load celestia-usdc-token-id)"
else
  say "creating the USDC synthetic token"
  res="$(tx relayer warp create-synthetic-token "$(load mailbox-id)")"
  token="$(ev "${res}" "hyperlane.warp.v1.EventCreateSyntheticToken" "token_id")"
  [ -n "${token}" ] || die "could not read the USDC token id"
  save celestia-usdc-token-id "${token}"
  point_at_ism "${token}"
  # A synthetic's denom only exists once the token does, so record it rather than expecting
  # anyone downstream to reconstruct it. This is the string the UI's CELESTIA_DENOM needs.
  save celestia-usdc-denom "hyperlane/${token}"
fi

# Remote routers are enrolled by 90-evm-warp.sh, which owns both directions of each route.
# They cannot be enrolled here: the EVM routers do not exist yet at this point in `make init`.

say "celestia warp side ready"
