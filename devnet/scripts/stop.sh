#!/usr/bin/env bash
# Tear the devnet down and leave nothing behind.
#
# Deliberately destructive: the chain is meant to start from a fresh genesis every time, so
# keeping state between runs would make a run depend on the one before it.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

KEEP_BIN="${KEEP_BIN:-1}"

say "stopping the coprocessor"
# The crate is tee-coprocessor but the binary it builds is tee-hyperlane, so matching on the
# crate name silently matched nothing and left the relayer running.
pkill -f "tee-hyperlane run --config" 2>/dev/null || true
pkill -f "vite.*3000" 2>/dev/null || true

if has enclave-app-id; then
  app_id="$(load enclave-app-id)"
  say "deleting the phala cvm ${app_id}"
  # A CVM bills by the hour, so leaving one running is the one mistake here that costs money.
  phala cvms delete --cvm-id "${app_id}" --force 2>&1 | tail -2 \
    || warn "could not delete ${app_id}; it is still billing, so check 'phala cvms ls'"
fi

say "stopping the chain"
STATE_DIR="${STATE_DIR}" docker compose -f "${DEVNET_DIR}/celestia/docker-compose.yml" down -v 2>&1 | tail -3 || true

say "pruning state"
# The PCCS address records are not devnet state: those contracts live on public testnets and
# outlive any number of make stop/init cycles. Losing them would mean redeploying seventeen
# contracts per chain to recover addresses that are still perfectly good.
# The host binaries are build output and the archive key is configuration. Neither is chain
# state, and pruning either one costs a rebuild or breaks the next init. KEEP_BIN=0 removes
# the binaries too; the key is always kept, since it is the user's to delete.
tmp="$(mktemp -d)"
[ "${KEEP_BIN}" = "1" ] && [ -d "${BIN_DIR}" ] && mv "${BIN_DIR}" "${tmp}/bin"
for keep in alchemy-key evm-key mnemonic; do
  [ -f "${STATE_DIR}/${keep}" ] && mv "${STATE_DIR}/${keep}" "${tmp}/${keep}"
done
mkdir -p "${tmp}/pccs"
for f in "${OUT_DIR}"/pccs-*.json; do [ -f "$f" ] && cp "$f" "${tmp}/pccs/"; done
rm -rf "${STATE_DIR}"
mkdir -p "${STATE_DIR}"
[ -d "${tmp}/bin" ] && mv "${tmp}/bin" "${BIN_DIR}"
for keep in alchemy-key evm-key mnemonic; do
  [ -f "${tmp}/${keep}" ] && mv "${tmp}/${keep}" "${STATE_DIR}/${keep}"
done
mkdir -p "${OUT_DIR}"
for f in "${tmp}"/pccs/pccs-*.json; do [ -f "$f" ] && mv "$f" "${OUT_DIR}/"; done
rm -rf "${tmp}"

say "devnet is down"
