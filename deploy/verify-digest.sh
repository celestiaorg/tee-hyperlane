#!/usr/bin/env bash
# Is the enclave running the code in this checkout?
#
# Each link is checked against the thing downstream of it, never against this repo's claims:
#
#   this checkout's compose file
#     -> the app_compose dstack measured
#       -> compose_hash
#         -> mr_config_id inside a quote Intel signed
#           -> what the ISM on chain will accept
#
# The last link is the one that matters. `info` in the enclave's reply is unsigned
# convenience data and an enclave could say anything there; the quote is what carries a
# signature, so a disagreement between the two means the info is wrong and the quote wins.
#
#   deploy/verify-digest.sh <app-id> [--ism <addr> --rpc <url>] [--rebuild]
#
# --rebuild also builds the image from source with Nix and compares digests, which takes
# about thirty five minutes. Without it the image leg is reported as unverified rather than
# passed, because the digest in the compose file is only the registry's word until rebuilt.
set -uo pipefail

APP_ID="${1:-}"
[ -n "${APP_ID}" ] || { echo "usage: $0 <app-id> [--ism <addr> --rpc <url>] [--rebuild]" >&2; exit 2; }
shift

ISM="" ; RPC="" ; REBUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --ism) ISM="$2"; shift 2 ;;
    --rpc) RPC="$2"; shift 2 ;;
    --rebuild) REBUILD=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The devnet's by default, because that is what a developer running this locally has just
# deployed. A testnet enclave measures deploy/docker-compose.yml instead, so checking one of
# those means naming it: COMPOSE=deploy/docker-compose.yml ./deploy/verify-digest.sh <app-id>.
# Without this the script could only ever pass for the devnet, and step 3 read as a real
# failure on every live enclave.
COMPOSE="${COMPOSE:-${ROOT}/devnet/enclave/docker-compose.yml}"
GATEWAY="${GATEWAY:-dstack-pha-prod9.phala.network}"
URL="${ENCLAVE_URL:-https://${APP_ID}-8080.${GATEWAY}}"
WORK="$(mktemp -d)"; trap 'rm -rf "${WORK}"' EXIT
fail=0
ok()  { printf '  \033[32mok\033[0m   %s\n' "$1"; }
bad() { printf '  \033[31mBAD\033[0m  %s\n' "$1"; fail=1; }
note(){ printf '  --   %s\n' "$1"; }

echo "enclave ${URL}"
curl -sS -m 30 "${URL}/identity" -o "${WORK}/id.json" || { bad "the enclave did not answer"; exit 1; }

echo
echo "1. compose_hash is the hash of what dstack measured"
python3 - "${WORK}" <<'PY' || fail=1
import hashlib, json, sys
work = sys.argv[1]
d = json.load(open(f"{work}/id.json"))
tcb = d["info"]["tcb_info"]
tcb = json.loads(tcb) if isinstance(tcb, str) else tcb
ac = tcb["app_compose"]
open(f"{work}/app_compose.json", "w").write(ac)
got = hashlib.sha256(ac.encode()).hexdigest()
claimed = d["info"]["compose_hash"].removeprefix("0x")
open(f"{work}/compose_hash", "w").write(got)
open(f"{work}/compose_in_quote", "w").write(
    json.loads(ac)["docker_compose_file"])
print(f"       sha256(app_compose) {got}")
if got != claimed:
    print(f"       the enclave claims   {claimed}")
    raise SystemExit(1)
PY
[ $fail -eq 0 ] && ok "reproduced from the preimage the enclave supplied" || bad "compose_hash is not the hash of the app_compose it reported"

echo
echo "2. that hash is inside the signed quote, not only the unsigned info"
python3 - "${WORK}" <<'PY' && ok "mr_config_id in the quote carries it" || bad "the quote carries a different compose hash than info claims"
import json, sys
work = sys.argv[1]
d = json.load(open(f"{work}/id.json"))
q = d["quote"]
raw = bytes.fromhex(q[2:] if q.startswith("0x") else q)
# The TD report body starts after the 48 byte quote header. mr_config_id sits at offset 184
# of the body and dstack writes it as 0x01 then the compose hash then zero padding.
body = raw[48:]
cfg = body[184:232]
if cfg[0] != 1:
    print(f"       mr_config_id does not start with 0x01: {cfg[:1].hex()}")
    raise SystemExit(1)
in_quote = cfg[1:33].hex()
expect = open(f"{work}/compose_hash").read().strip()
print(f"       mr_config_id        {in_quote}")
raise SystemExit(0 if in_quote == expect else 1)
PY

echo
echo "3. the measured compose is this checkout's compose"
if diff -q "${WORK}/compose_in_quote" "${COMPOSE}" >/dev/null 2>&1; then
  ok "byte for byte identical to ${COMPOSE#${ROOT}/}"
else
  bad "the enclave measured a different compose file"
  diff "${COMPOSE}" "${WORK}/compose_in_quote" | head -20 | sed 's/^/       /'
fi

IMAGE="$(grep -oE 'image:[[:space:]]*\S+' "${WORK}/compose_in_quote" | awk '{print $2}' | head -1)"
DIGEST="${IMAGE##*@}"
echo
echo "4. the image that compose pins"
note "${IMAGE}"

echo
echo "5. what the chain will accept"
if [ -n "${ISM}" ] && [ -n "${RPC}" ]; then
  MEASURED="$(python3 - "${WORK}" <<'PY'
import json, subprocess, sys
work = sys.argv[1]
d = json.load(open(f"{work}/id.json"))
q = d["quote"]; raw = bytes.fromhex(q[2:] if q.startswith("0x") else q)[48:]
# mr_td ++ mr_config_id, then rtmr0..2. rtmr3 is excluded on purpose: it carries app-id and
# instance-id, so including it would tie an ISM to one CVM rather than to the code it runs.
pre = "0x" + (raw[136:232] + raw[328:472]).hex()
print(subprocess.run(["cast", "keccak", pre], capture_output=True, text=True).stdout.strip())
PY
)"
  PINNED="$(cast call "${ISM}" 'enclaveMeasurements()(bytes32)' --rpc-url "${RPC}" 2>/dev/null)"
  note "enclave measures ${MEASURED}"
  note "ism pins        ${PINNED}"
  [ "${MEASURED}" = "${PINNED}" ] && ok "this enclave satisfies that ISM" || bad "this enclave would be rejected by that ISM"
else
  note "skipped; pass --ism <addr> --rpc <url> to check an EVM ISM"
fi

echo
echo "6. the image is what this source builds"
if [ "${REBUILD}" -eq 1 ]; then
  command -v nix >/dev/null 2>&1 || { bad "nix is not installed"; }
  if command -v nix >/dev/null 2>&1; then
    ( cd "${ROOT}" && nix build .#image ) || bad "nix build failed"
    CONFIG="$(tar -xOf "${ROOT}/result" manifest.json 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)[0]["Config"])' 2>/dev/null)"
    note "rebuilt config digest ${CONFIG}"
    note "compose pins manifest digest ${DIGEST}"
    note "these are different objects and are not expected to be equal; compare the config"
    note "digest against deploy/MAINTAIN.md, which records the expected value"
  fi
else
  note "not rebuilt. ${DIGEST} is the registry's word until you run --rebuild,"
  note "which is the only link here that is not independently checked."
fi

echo
[ ${fail} -eq 0 ] && echo "every checked link holds." || echo "at least one link failed."
exit ${fail}
