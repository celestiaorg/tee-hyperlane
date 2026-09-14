#!/usr/bin/env bash
# Submit one proved attestation to Celestia's x/zkism, then deliver its messages.
#
# Same three steps as the EVM side, because the ISM is the same protocol:
#   update           advances the trusted origin state
#   submit-messages  authorises the batch under the new root
#   mailbox process  hands each message to the mailbox, which asks the ISM
#
# Only messages addressed to this chain are processed. The batch necessarily contains every
# leaf inserted in the range - that is what makes the merkle replay check work - so on a
# shared mailbox it will include other people's messages. They are authorised but never
# delivered here, because their destination domain is not ours.
#
# Every step is skipped if the chain shows it already happened, so re-running this script
# with the same proved.json finishes an interrupted batch instead of failing.
set -euo pipefail

PROVED=${1:?usage: submit-celestia.sh <proved.json> | --preflight}
ISM=${CELESTIA_ISM:?set CELESTIA_ISM}
MAILBOX=${CELESTIA_MAILBOX:-0x68797065726c616e650000000000000000000000000000000000000000000000}
NODE=${CELESTIA_RPC:-https://rpc-mocha.pops.one}
HOME_DIR=${CELHOME:-/tmp/celhome}
LOCAL_DOMAIN=${CELESTIA_DOMAIN:-1297040200}
APPD=${APPD:-celestia-appd}

TX="--home $HOME_DIR --keyring-backend test --chain-id mocha-5 --node $NODE --from bridge -y -o json"
Q="--node $NODE -o json"

# Can this relayer pay to submit at all?
#
# Asked before proving as well as during it, because proving is hours of CPU and submission is
# what comes after: an empty fee account otherwise costs those hours and then fails, every
# tick, forever. Two transactions is the floor for a batch - update and submit-messages - so
# that is what is checked; the exact number of deliveries on top is not knowable here.
if [ "$PROVED" = "--preflight" ]; then
  # Fail open, never closed. This check exists to stop wasted proving, so it may only block a
  # route when it positively knows the balance is too low. Not being able to find out - no
  # binary, no keyring, an unreachable node - is not evidence of an empty account, and
  # treating it as such would take down every route the moment the check itself broke.
  ADDR=$("$APPD" keys show bridge -a --home "$HOME_DIR" --keyring-backend test 2>/dev/null) || ADDR=""
  [ -z "$ADDR" ] && exit 0
  HAVE=$("$APPD" query bank balances "$ADDR" $Q 2>/dev/null \
    | python3 -c "
import sys, json
bs = json.load(sys.stdin).get('balances', [])
print(next((int(b['amount']) for b in bs if b['denom'] == 'utia'), 0))" 2>/dev/null) || HAVE=""
  # An unreachable node is not an empty account; let the route proceed and fail later if so.
  [ -z "$HAVE" ] && exit 0
  NEED=20000
  if [ "$HAVE" -lt "$NEED" ]; then
    echo "the relayer cannot pay Celestia fees: $ADDR holds ${HAVE}utia and one batch needs at least ${NEED}utia; fund that address" >&2
    exit 1
  fi
  echo "  fee balance ${HAVE}utia, enough for at least one batch"
  exit 0
fi

jqv() { python3 -c "import sys,json;print(json.load(open('$PROVED'))$1)"; }

# The reason a tx failed, for the one case that is not a failure. Set by `send`.
LAST_LOG=""

# A tx that reports code 0 in CheckTx can still fail in DeliverTx, so wait for the result.
#
# CheckTx's own verdict has to be read first. A rejected tx still comes back with a txhash,
# and polling for it then fails sixty seconds later as "timed out" - which names the symptom
# and hides the cause. So a non-zero code here is reported as itself, immediately.
send() {
  local out hash
  out=$("$APPD" tx $@ $TX --fees 10000utia --gas 800000) || {
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
  for _ in $(seq 1 20); do
    sleep 3
    if out=$("$APPD" query tx "$hash" $Q 2>/dev/null); then
      LAST_LOG=$(python3 -c "
import sys, json
print(json.loads(sys.argv[1]).get('raw_log', ''))" "$out")
      python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print('  code', d['code'], 'height', d['height'], d.get('raw_log','')[:160])
sys.exit(0 if d['code'] == 0 else 1)" "$out"
      return
    fi
  done
  # Accepted into the mempool but never included: almost always the fee, the sequence, or a
  # node that dropped it. Say so, rather than leaving a bare hash to chase.
  echo "  $hash accepted at broadcast but not included within 60s;" >&2
  echo "  check the bridge account's balance and sequence against $NODE" >&2
  return 1
}

WANT=$(python3 - "$PROVED" <<'PY'
import json, sys
pv = bytes.fromhex(json.load(open(sys.argv[1]))["proofs"]["state_transition"]["public_values"])
first = int.from_bytes(pv[0:8], "little")
print("0x" + pv[8 + first + 8:8 + first + 8 + 32].hex())
PY
)

state_root() {
  "$APPD" query zkism ism "$ISM" $Q 2>/dev/null \
    | python3 -c "import sys,json,base64;print('0x'+base64.b64decode(json.load(sys.stdin)['ism']['state'])[:32].hex())"
}

echo "== update state =="
if [ "$(state_root || echo none)" = "$WANT" ]; then
  echo "  state already at $WANT, skipping"
else
  send zkism update "$ISM" "$(jqv "['proofs']['state_transition']['proof']")" \
                           "$(jqv "['proofs']['state_transition']['public_values']")"
fi

echo "== submit messages =="
# There is no queryable "already submitted for this root" flag here, and the obvious test -
# "is some id still authorised" - is wrong, because verifying a message consumes its id. So
# module's own rejection is the check: resubmitting the same batch is a no-op, not a failure.
if ! send zkism submit-messages "$ISM" "$(jqv "['proofs']['state_membership']['proof']")" \
                                       "$(jqv "['proofs']['state_membership']['public_values']")"
then
  case "$LAST_LOG" in
    *"already submitted"*|*"already been submitted"*)
      echo "  already submitted for this root, continuing"
      ;;
    *)
      exit 1
      ;;
  esac
fi

echo "== deliver messages addressed to domain $LOCAL_DOMAIN =="
COUNT=$(python3 -c "import json;print(len(json.load(open('$PROVED'))['messages']))")
for i in $(seq 0 $((COUNT-1))); do
  MSG=$(jqv "['messages'][$i]")
  [ -z "$MSG" ] && continue
  DEST=$(python3 -c "print(int('$MSG'[82:90],16))")
  if [ "$DEST" != "$LOCAL_DOMAIN" ]; then
    echo "  [$i] destination $DEST, not ours - skipping"
    continue
  fi
  ID=$(jqv "['batch'][$i]")
  if "$APPD" query hyperlane delivered "$MAILBOX" "$ID" $Q 2>/dev/null | grep -q '"delivered": *true'; then
    echo "  [$i] $ID already delivered"; continue
  fi
  echo "  [$i] processing $ID"
  send hyperlane mailbox process "$MAILBOX" "0x" "0x$MSG"
done
