#!/usr/bin/env bash
# Refuse to let a credential reach a tracked file.
#
# This repository is public and an Alchemy key was committed to it once already, in
# deploy/server/coprocessor.toml, where it sat in main's tree. A key in a public repository
# is scraped within hours, which is a plausible reason the free tier was exhausted early.
#
# Live values belong in devnet/.state/, which devnet/.gitignore excludes. Anything tracked
# should carry a placeholder instead.
#
#   deploy/check-secrets.sh            # scan tracked files
#   deploy/check-secrets.sh --staged   # scan what is about to be committed
#
# Install as a hook:
#   ln -s ../../deploy/check-secrets.sh .git/hooks/pre-commit
set -uo pipefail

MODE="${1:-}"
if [ "${MODE}" = "--staged" ]; then
  FILES=$(git diff --cached --name-only --diff-filter=ACM)
else
  FILES=$(git ls-files)
fi
[ -n "${FILES}" ] || exit 0

fail=0
report() { printf '  \033[31m%s\033[0m  %s\n' "$1" "$2"; fail=1; }

while IFS= read -r f; do
  [ -f "${f}" ] || continue
  case "${f}" in
    # Our own tooling, the archived record of the previous deployment, and vendored upstream
    # code. Automata's scripts default to Anvil's published test key, which is not a secret
    # and would otherwise make this shout on every run until someone stopped reading it.
    deploy/check-secrets.sh|deploy/archive/*|devnet/automata/*) continue ;;
  esac

  # A provider key in a URL. The placeholder spellings are what belongs there instead.
  # A provider host followed anywhere on the line by a long opaque path segment. Matching
  # the segment rather than a per-provider URL shape is what makes this catch the next
  # provider too, and an earlier version that tried to spell out /v2/ missed a real key.
  hits=$(grep -nE '(alchemy|infura|quicknode|blastapi|ankr|zan\.top|drpc|tenderly)\.[a-z.]+/[^" ]*[A-Za-z0-9_-]{24,}' "${f}" 2>/dev/null \
         | grep -vE 'ALCHEMY_KEY|INFURA_KEY|<[A-Za-z_]+>|YOUR_|KEY_HERE|\$\{' || true)
  [ -n "${hits}" ] && report "provider key" "${f}: $(printf '%s' "${hits}" | head -1 | cut -c1-90)"

  # A bare secp256k1 private key.
  hits=$(grep -nE '\b(0x)?[0-9a-fA-F]{64}\b' "${f}" 2>/dev/null \
         | grep -iE 'priv|secret|mnemonic|PRIVATE_KEY' \
         | grep -viE 'ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80|59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d|0{64}' || true)
  [ -n "${hits}" ] && report "private key" "${f}: $(printf '%s' "${hits}" | head -1 | cut -c1-90)"

  # A BIP39 phrase. Twelve or more lowercase words in a row, on a line that says so.
  hits=$(grep -niE 'mnemonic|seed phrase' "${f}" 2>/dev/null \
         | grep -E '([a-z]+ ){11,}[a-z]+' || true)
  [ -n "${hits}" ] && report "mnemonic" "${f}: $(printf '%s' "${hits}" | head -1 | cut -c1-70)"
done <<< "${FILES}"

if [ ${fail} -eq 0 ]; then
  echo "  no credentials in tracked files"
else
  echo
  echo "  Put the live value in devnet/.state/ instead, which is gitignored, and leave a"
  echo "  placeholder in the tracked file. If one has already been pushed, rotate it:"
  echo "  deleting it from the tip does not remove it from history or from whoever scraped it."
fi
exit ${fail}
