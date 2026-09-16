#!/bin/sh
# Bring up a single-validator celestia-app whose genesis is created on first run.
#
# The home directory is a bind mount, so `make stop` deletes it and the next
# `make init` starts from a genuinely fresh chain rather than from whatever the
# last run left behind.
set -eu

CHAINID="${CHAINID:-teeism-local}"
HOME_DIR="${CELESTIA_APP_HOME:-/home/celestia/.celestia-app}"
VALIDATOR_COINS="${VALIDATOR_COINS:-1000000000000000utia}"
VALIDATOR_STAKE="${VALIDATOR_STAKE:-5000000000utia}"
# Funded at genesis so the relayer and the UI have gas without a faucet round trip.
RELAYER_COINS="${RELAYER_COINS:-1000000000000utia}"
USER_COINS="${USER_COINS:-1000000000000utia}"
BLOCK_TIME="${BLOCK_TIME:-1s}"

appd() { celestia-appd "$@" --home "${HOME_DIR}"; }
keyadd() { appd keys add "$1" --keyring-backend test --output json; }

if [ ! -f "${HOME_DIR}/config/genesis.json" ]; then
  echo "==> initialising ${CHAINID}"
  appd init "${CHAINID}" --chain-id "${CHAINID}"

  # Written to the mounted home so the host can read them back without shelling
  # into the container.
  mkdir -p "${HOME_DIR}/devnet"
  keyadd validator > "${HOME_DIR}/devnet/validator.json"
  keyadd relayer   > "${HOME_DIR}/devnet/relayer.json"
  keyadd user      > "${HOME_DIR}/devnet/user.json"

  addr() { appd keys show "$1" -a --keyring-backend test; }
  appd genesis add-genesis-account "$(addr validator)" "${VALIDATOR_COINS}"
  appd genesis add-genesis-account "$(addr relayer)"   "${RELAYER_COINS}"
  appd genesis add-genesis-account "$(addr user)"      "${USER_COINS}"

  # The gentx is executed during InitGenesis and is metered like any other
  # transaction, so a fee-less one aborts the chain before it produces a block.
  appd genesis gentx validator "${VALIDATOR_STAKE}" \
    --keyring-backend test --chain-id "${CHAINID}" --fees 2000utia
  appd genesis collect-gentxs

  CFG="${HOME_DIR}/config/config.toml"
  APP="${HOME_DIR}/config/app.toml"

  # Listen outside the container.
  sed -i 's#"tcp://127.0.0.1:26657"#"tcp://0.0.0.0:26657"#g' "${CFG}"
  # The transaction indexer is off by default. The coprocessor finds dispatches
  # through an indexed tx search, so without this it sees nothing at all.
  sed -i 's#indexer = "null"#indexer = "kv"#g' "${CFG}"
  sed -i "s#^timeout_commit = .*#timeout_commit = \"${BLOCK_TIME}\"#" "${CFG}"
  # A devnet has one node, so there is nobody to gossip with and no reason to
  # refuse a peerless start.
  sed -i 's#^cors_allowed_origins = .*#cors_allowed_origins = ["*"]#' "${CFG}"

  sed -i 's#^enable = false#enable = true#' "${APP}"
  sed -i 's#^swagger = false#swagger = true#' "${APP}"
  sed -i 's#^address = "tcp://localhost:1317"#address = "tcp://0.0.0.0:1317"#' "${APP}"
  sed -i 's#^address = "localhost:9090"#address = "0.0.0.0:9090"#' "${APP}"
  sed -i 's#^enabled-unsafe-cors = false#enabled-unsafe-cors = true#' "${APP}"
  sed -i 's#^minimum-gas-prices = .*#minimum-gas-prices = "0.002utia"#' "${APP}"

  echo "==> genesis ready"
fi

echo "==> starting celestia-appd"
exec celestia-appd start --home "${HOME_DIR}" --api.enable --grpc.enable --force-no-bbr
