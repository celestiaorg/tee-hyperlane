#!/usr/bin/env bash
# Deploy the devnet enclave on Phala Cloud and wait for it to answer.
#
# The enclave is the one piece that cannot be local: a TDX quote has to come from real Intel
# hardware, and this machine is not it. Everything else in this devnet runs on the laptop.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

need phala
need curl

CVM_NAME="${CVM_NAME:-teeism-devnet}"
INSTANCE_TYPE="${INSTANCE_TYPE:-tdx.small}"
# Node 18 is prod9. Auto-selection sometimes lands on prod5, whose teepod reports
# tproxy_base_domain: None - the CVM runs, the gateway never registers it, and every request
# terminates TLS then returns nothing.
NODE_ID="${PHALA_NODE_ID:-18}"
OS_IMAGE="${PHALA_OS_IMAGE:-dstack-0.5.9}"

if has enclave-url; then
  url="$(load enclave-url)"
  if curl -sf -m 15 "${url}/health" >/dev/null 2>&1; then
    say "enclave already up at ${url}"
    exit 0
  fi
  warn "recorded enclave ${url} is not answering; deploying a new one"
fi

phala status >/dev/null 2>&1 || die "not authenticated to Phala Cloud. Run 'phala auth login <api-key>' with a key from cloud.phala.network > Settings > API Keys"

say "deploying ${CVM_NAME} (${INSTANCE_TYPE}, node ${NODE_ID}, ${OS_IMAGE})"
# --no-dev-os matters: if the CLI finds an SSH public key on this machine it otherwise
# provisions a dev image that permits shell access into the CVM, which would make the
# enclave's measurements meaningless.
out="$(phala deploy \
  --name "${CVM_NAME}" \
  --compose "${DEVNET_DIR}/enclave/docker-compose.yml" \
  --instance-type "${INSTANCE_TYPE}" \
  --node-id "${NODE_ID}" \
  --image "${OS_IMAGE}" \
  --no-dev-os \
  --wait --json 2>&1)" || { printf '%s\n' "${out}" >&2; die "phala deploy failed"; }

# The CLI prints progress before its JSON and wraps the payload differently per command, so
# the id is pulled from the first JSON object in the output rather than from a fixed path.
app_id="$(printf '%s' "${out}" | python3 -c "
import json, re, sys
raw = sys.stdin.read()
for m in re.finditer(r'[{\[]', raw):
    try:
        doc, _ = json.JSONDecoder().raw_decode(raw[m.start():])
    except ValueError:
        continue
    stack = [doc]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            for key in ('app_id', 'appId'):
                if isinstance(node.get(key), str) and node[key]:
                    print(node[key]); raise SystemExit
            stack.extend(node.values())
        elif isinstance(node, list):
            stack.extend(node)
" 2>/dev/null | head -1)"
if [ -z "${app_id}" ]; then
  # The deploy may still have created a CVM, so say where to look rather than leaving one
  # billing quietly.
  printf '%s\n' "${out}" >&2
  die "could not read the app id from the deploy response; check 'phala cvms ls' for a stray ${CVM_NAME}"
fi

url="https://${app_id}-8080.dstack-pha-prod9.phala.network"
save enclave-app-id "${app_id}"
save enclave-url    "${url}"

say "waiting for ${url}"
for i in $(seq 1 60); do
  if curl -sf -m 10 "${url}/health" >/dev/null 2>&1; then
    say "enclave is answering"
    exit 0
  fi
  sleep 10
done
die "enclave did not come up; check 'phala cvms get --cvm-id ${app_id}'"
