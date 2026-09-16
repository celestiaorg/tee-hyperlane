# Shared helpers for the devnet scripts. Sourced, never executed.
# shellcheck shell=bash

set -euo pipefail

DEVNET_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(cd "${DEVNET_DIR}/.." && pwd)"
STATE_DIR="${STATE_DIR:-${DEVNET_DIR}/.state}"
OUT_DIR="${STATE_DIR}/out"

CELESTIA_APP_DIR="${CELESTIA_APP_DIR:-${REPO_DIR}/../celestia-app-local}"
CELESTIA_IMAGE="${CELESTIA_IMAGE:-celestia-app-teeism:local}"
CELESTIA_CONTAINER="${CELESTIA_CONTAINER:-teeism-celestia}"
CHAINID="${CHAINID:-teeism-local}"
CELESTIA_RPC="${CELESTIA_RPC:-http://localhost:26657}"
CELESTIA_API="${CELESTIA_API:-http://localhost:1317}"

# The devnet chain's Hyperlane domain. Deliberately not Mocha's: the EVM side
# enrolls routers by domain, and reusing a live domain would let a devnet router
# and a testnet router be confused for one another.
CELESTIA_DOMAIN="${CELESTIA_DOMAIN:-1297040299}"

SEPOLIA_DOMAIN=11155111
BASE_SEPOLIA_DOMAIN=84532
ARBITRUM_SEPOLIA_DOMAIN=421614

TX_FEES="${TX_FEES:-200000utia}"
TX_GAS="${TX_GAS:-900000}"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror\033[0m %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }

BIN_DIR="${BIN_DIR:-${STATE_DIR}/bin}"
APPD="${APPD:-${BIN_DIR}/celestia-appd}"
COLLATERAL_BIN="${COLLATERAL_BIN:-${BIN_DIR}/teeism-collateral}"
CELHOME="${CELHOME:-${STATE_DIR}/celestia}"

# The key that pays for EVM deployments. Taken from the environment, or from a file dropped in
# .state, so nothing sensitive lives in the repository. .state survives `make stop`.
if [ -z "${EVM_PRIVATE_KEY:-}" ] && [ -f "${STATE_DIR}/evm-key" ]; then
  EVM_PRIVATE_KEY="$(tr -d ' \n\r' < "${STATE_DIR}/evm-key")"
  case "${EVM_PRIVATE_KEY}" in 0x*) ;; *) EVM_PRIVATE_KEY="0x${EVM_PRIVATE_KEY}" ;; esac
  export EVM_PRIVATE_KEY
fi

# The chain's home directory is a bind mount, so a host-side binary shares the container's
# keyring and can sign without shelling into it. That keeps file paths host paths, which
# matters because the submit step hands the node a file it just wrote.
appd() { "${APPD}" "$@" --home "${CELHOME}"; }

# Build the host-side binaries if they are missing, so pruning the build tree is recoverable.
ensure_binaries() {
  mkdir -p "${BIN_DIR}"
  [ -d "${CELESTIA_APP_DIR}" ] || die "no celestia-app checkout at ${CELESTIA_APP_DIR}; set CELESTIA_APP_DIR"
  if [ ! -x "${APPD}" ]; then
    say "building celestia-appd for the host"
    (cd "${CELESTIA_APP_DIR}" && go build -o "${APPD}" ./cmd/celestia-appd)
  fi
  if [ ! -x "${COLLATERAL_BIN}" ]; then
    say "building teeism-collateral"
    (cd "${CELESTIA_APP_DIR}" && go build -o "${COLLATERAL_BIN}" ./x/teeism/cmd/teeism-collateral)
  fi
}

# addr <key> - the bech32 address of a devnet key.
addr() { appd keys show "$1" -a --keyring-backend test; }

# Queries need a node, not a home, but passing both is harmless and keeps one helper.
q() { appd query "$@" --node "${CELESTIA_RPC}" -o json; }

# tx <key> <args...> - broadcast, wait for inclusion, and print the tx result as
# JSON. Fails loudly on a non-zero code rather than leaving a later step to
# discover the transaction never landed.
tx() {
  local key="$1"; shift
  local raw hash
  raw="$(appd tx "$@" \
    --from "${key}" --keyring-backend test --chain-id "${CHAINID}" \
    --node "${CELESTIA_RPC}" \
    --fees "${TX_FEES}" --gas "${TX_GAS}" \
    --broadcast-mode sync --yes --output json 2>&1)" || {
      printf '%s\n' "${raw}" >&2
      die "broadcast failed"
    }

  # The node echoes CheckTx first. A non-zero code here means it never entered a
  # block, so reporting the hash would be misleading.
  local code
  code="$(printf '%s' "${raw}" | jq -r 'select(.code != null) | .code' 2>/dev/null | head -1)"
  if [ -n "${code}" ] && [ "${code}" != "0" ]; then
    printf '%s\n' "${raw}" >&2
    die "transaction rejected at CheckTx with code ${code}"
  fi

  hash="$(printf '%s' "${raw}" | jq -r 'select(.txhash != null) | .txhash' 2>/dev/null | head -1)"
  [ -n "${hash}" ] || { printf '%s\n' "${raw}" >&2; die "no tx hash in response"; }

  wait_for_tx "${hash}"
}

# wait_for_tx <hash> - poll until the transaction is in a block, then print it.
wait_for_tx() {
  local hash="$1" i result code
  for i in $(seq 1 60); do
    if result="$(appd query tx "${hash}" --node "${CELESTIA_RPC}" --output json 2>/dev/null)"; then
      code="$(printf '%s' "${result}" | jq -r '.code // 0')"
      if [ "${code}" != "0" ]; then
        printf '%s\n' "$(printf '%s' "${result}" | jq -r '.raw_log // .rawLog // "no log"')" >&2
        die "transaction ${hash} failed on chain with code ${code}"
      fi
      printf '%s' "${result}"
      return 0
    fi
    sleep 1
  done
  die "timed out waiting for ${hash}"
}

# ev <tx-json> <event-type> <attribute> - read one attribute out of a tx result.
ev() {
  printf '%s' "$1" | jq -r --arg t "$2" --arg k "$3" '
    [ .events[]? | select(.type == $t) | .attributes[]? | select(.key == $k) | .value ]
    | first // empty
  ' | tr -d '"'
}

# save <name> <value> - record a deployed address so later steps and the relayer
# can read it back without re-parsing transaction logs.
save() {
  mkdir -p "${OUT_DIR}"
  printf '%s' "$2" > "${OUT_DIR}/$1"
  printf '  %-28s %s\n' "$1" "$2"
}

# load <name> - read back a saved value, failing if the step that writes it never ran.
load() {
  local f="${OUT_DIR}/$1"
  [ -f "${f}" ] || die "missing ${1}; run 'make init' first"
  cat "${f}"
}

has() { [ -f "${OUT_DIR}/$1" ]; }

# wait_for_chain - block until the node is answering and producing blocks.
wait_for_chain() {
  local i height
  for i in $(seq 1 90); do
    height="$(curl -s -m 2 "${CELESTIA_RPC}/status" 2>/dev/null \
      | jq -r '.result.sync_info.latest_block_height // empty' 2>/dev/null || true)"
    if [ -n "${height}" ] && [ "${height}" -gt 0 ] 2>/dev/null; then
      say "celestia is at height ${height}"
      return 0
    fi
    sleep 1
  done
  die "celestia did not start; check 'docker logs ${CELESTIA_CONTAINER}'"
}
