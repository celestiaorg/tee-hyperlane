#!/usr/bin/env bash
# The Celestia side of every route: one ISM per origin, and the routing ISM that fans them
# out.
#
# An ISM pins the identity of the enclave that attests *its* origin, and each origin family
# runs its own enclave image. So Sepolia, Arbitrum and Base pin the Ethereum enclave while
# Eden pins the evolve one, and changing either leaves the other alone. That is the whole
# point of the split: before it, one identity served every origin and a change anywhere
# re-deployed everything.
#
# Adding an origin is a row in ORIGINS plus its bootstrap in `genesis_for`.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need curl
wait_for_chain

# Endpoints and anchors the bootstraps need. Defaults match the live deployment; override in
# devnet/.env for another one.
SEPOLIA_BEACON="${SEPOLIA_BEACON:-https://ethereum-sepolia-beacon-api.publicnode.com}"
SEPOLIA_RPC="${SEPOLIA_RPC:-https://rpc.sepolia.ethpandaops.io}"
ARBITRUM_ARCHIVE="${ARBITRUM_ARCHIVE:-https://api.zan.top/arb-sepolia}"
ARBITRUM_ANCHOR="${ARBITRUM_ANCHOR:-0x042B2E6C5E99d4c521bd49beeD5E99651D9B0Cf4}"
BASE_ANCHOR="${BASE_ANCHOR:-0x2fF5cC82dBf333Ea30D8ee462178ab1707315355}"
MOCHA_RPC="${MOCHA_RPC:-https://rpc.celestia-mocha.com}"
# Base's confirmed head trails by the five day dispute window, which no free endpoint serves
# eth_getProof that far back for.
if [ -z "${BASE_ARCHIVE:-}" ] && [ -f "${STATE_DIR}/alchemy-base-key" ]; then
  BASE_ARCHIVE="https://base-sepolia.g.alchemy.com/v2/$(cat "${STATE_DIR}/alchemy-base-key")"
fi
BASE_ARCHIVE="${BASE_ARCHIVE:-}"

A="${BIN_DIR}/celestia-appd"
B="${REPO_DIR}/tee-hyperlane/target/release/tee-hyperlane"
TX="--from relayer --keyring-backend test --home ${CELHOME} --chain-id ${CHAINID}
    --node ${CELESTIA_RPC} --fees 200000utia --gas 900000 --broadcast-mode sync -y -o json"

# origin : domain : enclave family : merkle tree address on that origin
ORIGINS="sepolia:11155111:ethereum:0x0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d
arbitrum:421614:ethereum:0x000000000000000000000000ad34a66bf6db18e858f6b686557075568c6e031c
base:84532:ethereum:0x00000000000000000000000086fb9f1c124fb20ff130c41a79a432f770f67afd
eden:3735928814:evolve:0x000000000000000000000000cfbe7016d123d52a7db4fc7d087ccb5421dbf8db"

# Wait for a transaction and report its code, since `--broadcast-mode sync` only means the
# node accepted it.
settle() {
  local hash="$1" tries=0
  while [ "${tries}" -lt 12 ]; do
    sleep 3
    out="$("${A}" query tx "${hash}" --node "${CELESTIA_RPC}" -o json 2>/dev/null)" || { tries=$((tries+1)); continue; }
    printf '%s' "${out}"
    return 0
  done
  return 1
}

# The genesis state for one origin, anchored at its current head.
#
# ISM_GENESIS_<ORIGIN> overrides it, which is how a checkpoint from an earlier deployment is
# re-used to recover messages it had already seen. `tee-hyperlane rotate-state` turns a live
# ISM's state into one of these.
genesis_for() { # <origin> <family>
  local override
  override="$(eval "printf '%s' \"\${ISM_GENESIS_$(printf '%s' "$1" | tr 'a-z-' 'A-Z_'):-}\"")"
  if [ -n "${override}" ]; then
    printf '%s' "${override}"
    return
  fi
  local digest
  digest="$(load "identity-digest-$2")"
  case "$1" in
    sepolia)
      "${B}" bootstrap-ethereum --beacon "${SEPOLIA_BEACON}" --execution "${SEPOLIA_RPC}" \
        --identity-digest "${digest}" 2>/dev/null | sed -n 's/^genesis state *//p' | tr -d ' ' ;;
    arbitrum)
      "${B}" bootstrap-l2 --rollup arbitrum --l2-archive "${ARBITRUM_ARCHIVE}" \
        --anchor "${ARBITRUM_ANCHOR}" --identity-digest "${digest}" 2>/dev/null \
        | sed -n 's/^genesis state *//p' | tr -d ' ' ;;
    base)
      "${B}" bootstrap-l2 --rollup base --l2-archive "${BASE_ARCHIVE}" \
        --anchor "${BASE_ANCHOR}" --identity-digest "${digest}" 2>/dev/null \
        | sed -n 's/^genesis state *//p' | tr -d ' ' ;;
    eden)
      # --out is not optional here. Eden's node serves eth_getProof for `latest` only, so the
      # anchor's tree proof has to be captured at bootstrap and filed under its height; the
      # route reads it back as the snapshot every attestation needs. Without it the ISM is
      # created at a height nothing can ever prove, and the route asks to be re-bootstrapped
      # for the rest of its life.
      "${B}" bootstrap-eden --rpc "${MOCHA_RPC}" --identity-digest "${digest}" \
        --out "${STATE_DIR}/proofs/eden-to-celestia/staging/bootstrap.json" 2>/dev/null \
        | sed -n 's/^genesis state *//p' | tr -d ' ' ;;
    *) return 1 ;;
  esac
}

for row in ${ORIGINS}; do
  IFS=: read -r name domain family tree <<< "${row}"
  say "== ${name} (domain ${domain}, ${family} enclave)"

  # Already created on this chain, so leave it alone. An origin whose bootstrap failed the
  # first time is the normal reason to run this again - Eden's needs a synced DA node, which
  # can take half an hour - and without this the re-run mints a second ISM for every origin
  # that already worked, and a second routing ISM over them. `make stop` clears .state, so a
  # fresh chain starts from nothing and this never hides a stale id.
  if has "ism-celestia-${name}"; then
    say "  already created: $(load "ism-celestia-${name}")"
    continue
  fi

  # `|| true` is load-bearing. lib.sh sets `-euo pipefail`, so a bootstrap that exits non-zero
  # - base with no archive key, Eden with a DA node that has not caught up - kills the whole
  # script at this assignment, before the guard on the next line can skip that origin. The
  # guard read as if it handled the case and never once ran: base took the run down with it
  # and Eden, the origin after it, was never attempted.
  genesis="$(genesis_for "${name}" "${family}" || true)"
  [ -n "${genesis}" ] || { warn "  could not anchor ${name}; skipping"; continue; }

  OUT_DIR="${OUT_DIR}" python3 - "${genesis}" "${tree}" "${name}" "${family}" <<'PY'
import json, sys, os
state, tree, name, family = sys.argv[1:5]
out = os.environ["OUT_DIR"]
json.dump({"state": state, "merkle_tree_address": tree,
           "identity": json.load(open(f"{out}/identity-{family}.json"))},
          open(f"{out}/ism-{name}-origin.json", "w"), indent=2)
PY

  hash="$("${A}" tx teeism create "${OUT_DIR}/ism-${name}-origin.json" ${TX} 2>&1 \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["txhash"])' 2>/dev/null)"
  [ -n "${hash}" ] || { warn "  ${name}: broadcast failed"; continue; }
  id="$(settle "${hash}" | python3 -c '
import sys, json
d = json.load(sys.stdin)
if d.get("code"):
    print("", end="")
    raise SystemExit
for ev in d["events"]:
    if "teeism" in ev["type"]:
        for a in ev["attributes"]:
            if a["key"] == "id":
                print(a["value"].strip(chr(34)))
' 2>/dev/null)"
  [ -n "${id}" ] || { warn "  ${name}: ISM not created"; continue; }
  save "ism-celestia-${name}" "${id}"
  say "  ${id}"
done

# ---------------------------------------------------------------- routing
#
# Four origins deliver into one Celestia token and each ISM pins one origin domain, so a
# single ISM would reject three of four.
say "== routing ism"
# Reused, never re-created. This is the ISM the mailbox and both warp tokens point at, so a
# second one does not replace the first, it orphans it: the domains registered on the old one
# stay there and nothing points at it any more.
if has routing-ism-id; then
  routing="$(load routing-ism-id)"
  say "  already created: ${routing}"
else
  hash="$("${A}" tx hyperlane ism create-routing ${TX} 2>&1 \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["txhash"])')"
  routing="$(settle "${hash}" | python3 -c '
import sys, json
for ev in json.load(sys.stdin)["events"]:
    if "RoutingIsm" in ev["type"] or "routing" in ev["type"].lower():
        for a in ev["attributes"]:
            if a["key"] in ("ism_id", "id"):
                print(a["value"].strip(chr(34)))
                raise SystemExit
')"
  [ -n "${routing}" ] || die "routing ISM not created"
  save routing-ism-id "${routing}"
  say "  ${routing}"
fi

for row in ${ORIGINS}; do
  IFS=: read -r name domain _ _ <<< "${row}"
  has "ism-celestia-${name}" || continue
  say "  domain ${domain} -> $(load "ism-celestia-${name}")"
  settle "$("${A}" tx hyperlane ism set-routing-ism-domain "${routing}" "${domain}" \
    "$(load "ism-celestia-${name}")" ${TX} 2>&1 \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["txhash"])')" >/dev/null
done

say "== pointing the tokens and the mailbox at it"
for key in celestia-token-id celestia-usdc-token-id; do
  has "${key}" || continue
  settle "$("${A}" tx warp set-token "$(load "${key}")" --ism-id "${routing}" ${TX} 2>&1 \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["txhash"])')" >/dev/null
  say "  $(load "${key}")"
done
settle "$("${A}" tx hyperlane mailbox set "$(load mailbox-id)" --default-ism "${routing}" ${TX} 2>&1 \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["txhash"])')" >/dev/null
say "  mailbox default"

say "celestia ISMs ready"
