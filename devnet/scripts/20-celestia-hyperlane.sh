#!/usr/bin/env bash
# Deploy Hyperlane core on the local Celestia chain.
#
# Every step here is permissionless, which is the point: this is the same
# sequence anyone would run against a real chain, not a genesis fixture.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

wait_for_chain

if has mailbox-id; then
  say "hyperlane core already deployed, skipping"
  exit 0
fi

# A noop ISM so the mailbox has a default before the TEE ISM exists. The TEE ISM
# cannot be created first: it pins a light client checkpoint, and reading one
# means the chain is already running.
say "creating noop ism"
out="$(tx relayer hyperlane ism create-noop)"
noop_ism="$(ev "${out}" "hyperlane.core.interchain_security.v1.EventCreateNoopIsm" "ism_id")"
[ -n "${noop_ism}" ] || die "could not read the noop ism id"
save noop-ism-id "${noop_ism}"

say "creating mailbox on domain ${CELESTIA_DOMAIN}"
out="$(tx relayer hyperlane mailbox create "${noop_ism}" "${CELESTIA_DOMAIN}")"
mailbox="$(ev "${out}" "hyperlane.core.v1.EventCreateMailbox" "mailbox_id")"
[ -n "${mailbox}" ] || die "could not read the mailbox id"
save mailbox-id "${mailbox}"

say "creating merkle tree hook"
out="$(tx relayer hyperlane hooks merkle create "${mailbox}")"
merkle="$(ev "${out}" "hyperlane.core.post_dispatch.v1.EventCreateMerkleTreeHook" "merkle_tree_hook_id")"
[ -n "${merkle}" ] || die "could not read the merkle tree hook id"
save merkle-hook-id "${merkle}"

# A noop required-hook keeps dispatch free. The interchain gas paymaster would
# otherwise quote a fee in utia for a destination this devnet has no oracle for.
say "creating noop hook"
out="$(tx relayer hyperlane hooks noop create)"
noop_hook="$(ev "${out}" "hyperlane.core.post_dispatch.v1.EventCreateNoopHook" "noop_hook_id")"
[ -n "${noop_hook}" ] || die "could not read the noop hook id"
save noop-hook-id "${noop_hook}"

# The merkle tree hook has to be the default, because the tree it maintains is what the
# enclave proves against. The required hook has to be set to something: warp refuses to
# dispatch without one, and a noop keeps the dispatch free.
say "pointing the mailbox at the merkle tree hook"
tx relayer hyperlane mailbox set "${mailbox}" \
  --default-hook "${merkle}" --required-hook "${noop_hook}" >/dev/null

say "hyperlane core is up"
