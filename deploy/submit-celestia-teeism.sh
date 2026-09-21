#!/usr/bin/env bash
# Submit one enclave attestation to Celestia's x/teeism, then deliver its messages.
#
# Two steps where a proof-carrying ISM needs three:
#   submit-attestation  advances the trusted state and authorises the batch together
#   mailbox process     hands each message to the mailbox, which asks the ISM
#
# There is no separate "update" transaction because there was never a second attestation.
# A proof-carrying ISM splits the batch from the state because two Groth16 proofs have to
# project the same quote through two different public-value shapes, and each needs its own
# transaction. The quote itself carries both, so verifying it once settles both.
#
# Only messages addressed to this chain are processed. The batch necessarily contains every
# leaf inserted in the range - that is what makes the merkle replay check work - so on a
# shared mailbox it will include other people's messages. They are authorised but never
# delivered here, because their destination domain is not ours.
#
# Every step is skipped if the chain shows it already happened, so re-running this script
# with the same attestation finishes an interrupted batch instead of failing.
set -euo pipefail

ATTESTED=${1:?usage: submit-celestia-teeism.sh <attestation.json> | --preflight}
ISM=${CELESTIA_ISM:?set CELESTIA_ISM}
MAILBOX=${CELESTIA_MAILBOX:?set CELESTIA_MAILBOX}
NODE=${CELESTIA_RPC:-http://localhost:26657}
HOME_DIR=${CELHOME:?set CELHOME}
CHAIN_ID=${CELESTIA_CHAIN_ID:-teeism-local}
LOCAL_DOMAIN=${CELESTIA_DOMAIN:?set CELESTIA_DOMAIN}
KEY=${CELESTIA_KEY:-relayer}
APPD=${APPD:-celestia-appd}
COLLATERAL_BIN=${COLLATERAL_BIN:-teeism-collateral}

TX="--home $HOME_DIR --keyring-backend test --chain-id $CHAIN_ID --node $NODE --from $KEY -y -o json"
Q="--node $NODE -o json"

# Can this relayer pay to submit at all?
#
# Fail open, never closed. This check exists to stop wasted work, so it may only block a
# route when it positively knows the balance is too low. Not being able to find out - no
# binary, no keyring, an unreachable node - is not evidence of an empty account, and treating
# it as such would take down every route the moment the check itself broke.
if [ "$ATTESTED" = "--preflight" ]; then
  ADDR=$("$APPD" keys show "$KEY" -a --home "$HOME_DIR" --keyring-backend test 2>/dev/null) || ADDR=""
  [ -z "$ADDR" ] && exit 0
  HAVE=$("$APPD" query bank balances "$ADDR" $Q 2>/dev/null \
    | python3 -c "
import sys, json
bs = json.load(sys.stdin).get('balances', [])
print(next((int(b['amount']) for b in bs if b['denom'] == 'utia'), 0))" 2>/dev/null) || HAVE=""
  [ -z "$HAVE" ] && exit 0
  NEED=20000
  if [ "$HAVE" -lt "$NEED" ]; then
    echo "the relayer cannot pay Celestia fees: $ADDR holds ${HAVE}utia and one batch needs at least ${NEED}utia; fund that address" >&2
    exit 1
  fi
  echo "  fee balance ${HAVE}utia, enough for at least one batch"
  exit 0
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# The attested payload carries the message ids, so they are read from the bytes the enclave
# signed rather than recomputed from the messages alongside them. If the two ever disagreed,
# the chain would reject the batch; taking them from the payload means they cannot.
python3 - "$ATTESTED" "$WORK" <<'PY'
import json, sys

record = json.load(open(sys.argv[1]))
work = sys.argv[2]
att = record["attestation"]

payload = bytes.fromhex(att["payload"].removeprefix("0x"))
STATE = 116
new_state = payload[STATE:2 * STATE]
off = 2 * STATE + 32 + 8
count = int.from_bytes(payload[off:off + 8], "big")
off += 8
ids = [payload[off + i * 32: off + (i + 1) * 32].hex() for i in range(count)]

open(f"{work}/quote.hex", "w").write(att["quote"])
open(f"{work}/new_state", "w").write(new_state.hex())
open(f"{work}/ids", "w").write("\n".join(ids))
open(f"{work}/messages", "w").write("\n".join(record.get("messages", [])))
json.dump(att, open(f"{work}/att.json", "w"))
PY

WANT=$(cat "$WORK/new_state")

state_now() {
  "$APPD" query teeism ism "$ISM" $Q 2>/dev/null \
    | python3 -c "import sys,json,base64;print(base64.b64decode(json.load(sys.stdin)['ism']['state']).hex())" 2>/dev/null
}

send() {
  local out hash
  out=$("$APPD" tx $@ $TX --fees 200000utia --gas 2000000) || {
    echo "  broadcast failed" >&2; return 1
  }
  hash=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
code = d.get('code', 0)
if code:
    print('  rejected at broadcast: code', code, d.get('raw_log', '')[:300], file=sys.stderr)
    sys.exit(1)
print(d['txhash'])" "$out") || return 1
  echo "  tx $hash"
  for _ in $(seq 1 30); do
    sleep 2
    if out=$("$APPD" query tx "$hash" $Q 2>/dev/null); then
      python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print('  code', d['code'], 'height', d['height'], d.get('raw_log','')[:200])
sys.exit(0 if d['code'] == 0 else 1)" "$out"
      return
    fi
  done
  echo "  $hash accepted at broadcast but not included within 60s" >&2
  return 1
}

echo "== submit attestation =="
if [ "$(state_now || echo none)" = "$WANT" ]; then
  echo "  state already advanced, skipping"
else
  # Intel's collateral is fetched here rather than on chain, because consensus cannot make
  # network calls. Every artifact is signed by Intel and those signatures are checked during
  # verification, so carrying them costs nothing in security.
  echo "  fetching intel collateral"
  "$COLLATERAL_BIN" -quote "$WORK/quote.hex" -out "$WORK/collateral.json" >/dev/null

  python3 - "$WORK" <<'PY'
import json, sys
work = sys.argv[1]
att = json.load(open(f"{work}/att.json"))
bundle = {
    "quote": att["quote"],
    # The event log is JSON text. The module takes it as bytes and parses it itself, so
    # it travels hex-encoded like everything else in the bundle.
    "event_log": "0x" + att["event_log"].encode().hex(),
    "payload": att["payload"],
    "collateral": json.load(open(f"{work}/collateral.json")),
}
json.dump(bundle, open(f"{work}/submit.json", "w"), indent=2)
PY

  send teeism submit-attestation "$ISM" "$WORK/submit.json"
fi

echo "== deliver messages addressed to domain $LOCAL_DOMAIN =="
# The list has no trailing newline, and a bare `while read` drops a final unterminated line -
# which is every batch's last message. Feeding it through `cat; echo` terminates it.
i=0
while IFS= read -r MSG; do
  [ -z "$MSG" ] && continue
  DEST=$(python3 -c "print(int('$MSG'[82:90],16))")
  ID=$(sed -n "$((i+1))p" "$WORK/ids")
  if [ "$DEST" != "$LOCAL_DOMAIN" ]; then
    echo "  [$i] destination $DEST, not ours - skipping"
    i=$((i+1)); continue
  fi
  if "$APPD" query hyperlane delivered "$MAILBOX" "0x$ID" $Q 2>/dev/null | grep -q '"delivered": *true'; then
    echo "  [$i] 0x$ID already delivered"; i=$((i+1)); continue
  fi
  echo "  [$i] processing 0x$ID"
  send hyperlane mailbox process "$MAILBOX" "0x" "0x$MSG"
  i=$((i+1))
done < <(cat "$WORK/messages"; echo)
