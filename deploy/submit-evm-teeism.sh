#!/usr/bin/env bash
# Submit one enclave attestation to a TeeDcapIsm, then deliver its messages.
#
# Two steps, the same shape as the Celestia side:
#   submitAttestation  advances the trusted state and authorises the batch together
#   mailbox.process    hands each message to the mailbox, which asks the ISM
#
# There is no separate state update because there was never a second attestation. The quote
# covers both, and the ISM verifies it once.
#
# Only messages addressed to this chain are processed. A batch carries every leaf inserted in
# its range, which is what makes the merkle replay check work, so it will contain messages for
# other destinations. They are authorised here but delivered elsewhere.
set -euo pipefail

ATTESTED=${1:?usage: submit-evm-teeism.sh <attestation.json> | --preflight}
ISM=${TEE_ISM:?set TEE_ISM}
MAILBOX=${MAILBOX:?set MAILBOX}
RPC=${EVM_RPC:?set EVM_RPC}
LOCAL_DOMAIN=${LOCAL_DOMAIN:?set LOCAL_DOMAIN}
PK=${EVM_PRIVATE_KEY:?set EVM_PRIVATE_KEY}

# Can this relayer pay at all? Fails open: not being able to find out is not evidence of an
# empty account, and treating it as such would stall every route the moment the check broke.
if [ "$ATTESTED" = "--preflight" ]; then
  ADDR=$(cast wallet address --private-key "$PK" 2>/dev/null) || exit 0
  BAL=$(cast balance "$ADDR" --rpc-url "$RPC" 2>/dev/null) || exit 0
  # A verification is roughly 5M gas; refuse only when visibly unable to cover one.
  NEED=$(python3 -c "print(5_000_000 * 2_000_000_000)")
  if [ "$(python3 -c "print(1 if int('$BAL') < $NEED else 0)")" = "1" ]; then
    echo "relayer cannot pay on this chain: $ADDR holds $(cast from-wei "$BAL") ETH" >&2
    exit 1
  fi
  echo "  fee balance $(cast from-wei "$BAL") ETH"
  exit 0
fi

WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT

# The attested payload carries the message ids, so they are read from the bytes the enclave
# signed rather than recomputed from the messages beside them.
python3 - "$ATTESTED" "$WORK" <<'PY'
import json, sys
record = json.load(open(sys.argv[1])); work = sys.argv[2]
att = record["attestation"]
payload = bytes.fromhex(att["payload"].removeprefix("0x"))
STATE = 116
off = 2 * STATE + 32 + 8
count = int.from_bytes(payload[off:off + 8], "big"); off += 8
ids = [payload[off + i*32: off + (i+1)*32].hex() for i in range(count)]
open(f"{work}/quote", "w").write(att["quote"] if att["quote"].startswith("0x") else "0x"+att["quote"])
open(f"{work}/payload", "w").write(att["payload"] if att["payload"].startswith("0x") else "0x"+att["payload"])
open(f"{work}/new_state", "w").write(payload[STATE:2*STATE].hex())
open(f"{work}/ids", "w").write("\n".join(ids))
open(f"{work}/messages", "w").write("\n".join(record.get("messages", [])))
PY

WANT=$(cat "$WORK/new_state")
state_now() { cast call "$ISM" "state()(bytes)" --rpc-url "$RPC" 2>/dev/null | sed 's/^0x//'; }

# Both transactions go into the same block.
#
# They used to be sequential: send the attestation, wait for its receipt, then send the
# delivery and wait again. Nothing requires that. The mailbox call only needs the attestation
# to be *in the block before it*, which nonce ordering already guarantees, so waiting for a
# receipt in between bought nothing and cost a full block. On a twelve second chain that was
# most of the transfer.
#
# Delivery cannot be gas-estimated before the attestation lands, because until then the ISM
# has not authorised the message and the call reverts. So it carries an explicit limit,
# measured at 89k-127k across this deployment with headroom.
DELIVERY_GAS=${DELIVERY_GAS:-400000}
SENDER=$(cast wallet address --private-key "$PK")
NONCE=$(cast nonce "$SENDER" --rpc-url "$RPC")
PENDING=""

echo "== submit attestation =="
if [ "$(state_now || echo none)" = "$WANT" ]; then
  echo "  state already advanced, skipping"
else
  # A revert carries Automata's four letter reason; describeQuoteError expands it for free.
  if ! OUT=$(cast send "$ISM" "submitAttestation(bytes,bytes)" "$(cat "$WORK/quote")" "$(cat "$WORK/payload")" \
       --rpc-url "$RPC" --private-key "$PK" --nonce "$NONCE" --async --json 2>&1); then
    CODE=$(printf '%s' "$OUT" | grep -oE "QuoteRejected\(0x[0-9a-f]+\)" | grep -oE "0x[0-9a-f]+" | head -1)
    if [ -n "$CODE" ]; then
      echo "  verifier refused the quote: $(cast to-utf8 "$CODE" 2>/dev/null)" >&2
      cast call "$ISM" "describeQuoteError(bytes)(string)" "$CODE" --rpc-url "$RPC" 2>/dev/null | sed 's/^/  /' >&2
    else
      printf '%s\n' "$OUT" | tail -3 >&2
    fi
    exit 1
  fi
  # --async gives the hash, not a receipt: the point is not to wait here.
  ATTEST_TX=$(printf '%s' "$OUT" | tr -d '"' | tail -1)
  echo "  tx $ATTEST_TX (not waiting)"
  PENDING="$ATTEST_TX"
  NONCE=$((NONCE + 1))
fi

echo "== deliver messages addressed to domain $LOCAL_DOMAIN =="
i=0
while IFS= read -r MSG; do
  [ -z "$MSG" ] && continue
  DEST=$(python3 -c "print(int('$MSG'[82:90],16))")
  ID=$(sed -n "$((i+1))p" "$WORK/ids")
  if [ "$DEST" != "$LOCAL_DOMAIN" ]; then
    echo "  [$i] destination $DEST, not ours - skipping"; i=$((i+1)); continue
  fi
  if [ "$(cast call "$MAILBOX" "delivered(bytes32)(bool)" "0x$ID" --rpc-url "$RPC" 2>/dev/null)" = "true" ]; then
    echo "  [$i] 0x$ID already delivered"; i=$((i+1)); continue
  fi
  echo "  [$i] processing 0x$ID"
  TX=$(cast send "$MAILBOX" "process(bytes,bytes)" "0x" "0x$MSG" --rpc-url "$RPC" --private-key "$PK" \
       --nonce "$NONCE" --gas-limit "$DELIVERY_GAS" --async 2>/dev/null | tr -d '"' | tail -1)
  if [ -n "$TX" ]; then echo "    $TX (not waiting)"; PENDING="$PENDING $TX"; NONCE=$((NONCE + 1));
  else echo "    could not submit"; fi
  i=$((i+1))
done < <(cat "$WORK/messages"; echo)

# Now wait, once, for everything that was sent. They were queued by nonce so the chain
# already ordered them; this only establishes that they landed and reports how they went.
if [ -n "$PENDING" ]; then
  echo "== receipts =="
  rc=0
  for tx in $PENDING; do
    R=$(cast receipt "$tx" --rpc-url "$RPC" --confirmations 1 --json 2>/dev/null) || { echo "  $tx no receipt"; rc=1; continue; }
    printf '%s' "$R" | python3 -c "
import sys, json
d = json.load(sys.stdin)
ok = int(d.get('status', '0x0'), 16) == 1
print('  %s block %d gas %d %s' % (d['transactionHash'][:18] + '...', int(d['blockNumber'], 16),
                                   int(d.get('gasUsed', '0x0'), 16), '' if ok else 'REVERTED'))
raise SystemExit(0 if ok else 1)" || rc=1
  done
  exit $rc
fi
