#!/usr/bin/env bash
# Create the Celestia side of the warp route.
#
# TIA is collateral here and synthetic on the EVM side, so one asset exercises both
# directions: sending locks TIA and mints on the remote, receiving burns there and unlocks
# here.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

wait_for_chain

if has celestia-token-id; then
  say "warp token already created: $(load celestia-token-id)"
else
  say "creating the TIA collateral token"
  res="$(tx relayer warp create-collateral-token "$(load mailbox-id)" utia)"
  token="$(ev "${res}" "hyperlane.warp.v1.EventCreateCollateralToken" "token_id")"
  [ -n "${token}" ] || die "could not read the token id"
  save celestia-token-id "${token}"

  # Point the token at the TEE ISM explicitly rather than relying on the mailbox default, so
  # that adding another ISM later cannot silently change what secures this route.
  if has teeism-ism-id; then
    say "pointing the token at the tee ism"
    tx relayer warp set-token "${token}" --ism-id "$(load teeism-ism-id)" >/dev/null 2>&1 \
      || warn "could not set the token ism; the mailbox default still applies"
  fi
fi

# Remote routers are enrolled by 90-evm-warp.sh, which owns both directions of the route.
# They cannot be enrolled here: the EVM routers do not exist yet at this point in `make init`.

say "celestia warp side ready"
