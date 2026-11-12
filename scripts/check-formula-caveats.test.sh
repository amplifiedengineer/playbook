#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-formula-caveats.test.sh: offline tests for check-formula-caveats.sh on
# fixture formulas.
#
# Usage: check-formula-caveats.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."
SCRIPT="$PWD/scripts/check-formula-caveats.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL $*" >&2; exit 1; }

# formula <name> <caveat line>: a fixture formula whose caveats hold the line.
formula() {
  local path="$TMP/$1.rb"
  {
    echo 'class Playbook < Formula'
    echo '  def caveats'
    echo '    <<~EOS'
    echo "      $2"
    echo '    EOS'
    echo '  end'
    echo 'end'
  } >"$path"
  echo "$path"
}

# expect <pass|fail> <label> <formula> [text]: runs the script and checks the result.
expect() {
  local rc=0 out
  out="$(bash "$SCRIPT" "$3" 2>&1)" || rc=$?
  if [ "$1" = pass ] && [ "$rc" -ne 0 ]; then fail "$2: expected a pass, got $rc: $out"; fi
  if [ "$1" = fail ] && [ "$rc" -ne 1 ]; then fail "$2: expected exit 1, got $rc: $out"; fi
  if [ -n "${4:-}" ] && ! grep -qF "$4" <<<"$out"; then fail "$2: output lacks $4: $out"; fi
}

expect fail "caveats without the sync line" \
  "$(formula without 'Usage sync is on by default.')" "playbook sync enable"
expect pass "caveats that name playbook sync enable" \
  "$(formula with 'Usage sync is off until you run playbook sync enable.')"
expect fail "caveats that still name pbsync enable" \
  "$(formula old 'Run pbsync enable to turn on sync.')" "playbook sync enable"

rc=0
out="$(bash "$SCRIPT" "$TMP/missing.rb" 2>&1)" || rc=$?
[ "$rc" -eq 1 ] || fail "a missing file: expected exit 1, got $rc"
grep -qF "$TMP/missing.rb" <<<"$out" || fail "a missing file: output lacks the path: $out"

rc=0
bash "$SCRIPT" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || fail "no argument: expected exit 2, got $rc"

echo "ok"
