#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-formula-caveats.sh: fails when a Homebrew formula does not tell users
# to run `playbook sync enable`, the one command that turns usage sync on.
#
# Usage: check-formula-caveats.sh FORMULA
set -euo pipefail

SYNC_COMMAND='playbook sync enable'

if [ "$#" -ne 1 ]; then
  echo "usage: check-formula-caveats.sh FORMULA" >&2
  exit 2
fi
FORMULA="$1"

if [ ! -f "$FORMULA" ]; then
  echo "::error::$FORMULA does not exist." >&2
  exit 1
fi

if ! grep -qF "$SYNC_COMMAND" "$FORMULA"; then
  echo "::error::The caveats in $FORMULA do not name \`$SYNC_COMMAND\`. Merge the homebrew-tap caveats change first." >&2
  exit 1
fi
