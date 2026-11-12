// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A fake `gh` script for the deliver tests. It logs every call to
//! `$FAKE_GH_DIR/log` and keeps the PR it was given in files beside it.

pub(crate) const FAKE_GH: &str = r#"#!/bin/bash
d="$FAKE_GH_DIR"
echo "$*" >> "$d/log"
val() { shift_to="$1"; shift; while [ $# -gt 0 ]; do [ "$1" = "$shift_to" ] && { echo "$2"; return; }; shift; done; }
json() {
  printf '{"number":7,"url":"https://github.com/o/r/pull/7","isDraft":%s,"state":"%s","title":"%s","baseRefName":"%s"}' \
    "$(cat $d/draft)" "$(cat $d/state)" "$(cat $d/title)" "$(cat $d/base)"
}
case "$1 $2" in
  "pr list")
    case "$*" in
      *headRefName*) [ -f "$d/heads" ] && cat "$d/heads" || echo "[]";;
      *) [ -f "$d/title" ] && echo "[$(json)]" || echo "[]";;
    esac;;
  "pr create")
    [ -f "$d/fail_create" ] && { echo "boom" >&2; exit 1; }
    val --title "$@" > "$d/title"; val --base "$@" > "$d/base"
    case "$*" in *--draft*) echo true > "$d/draft";; *) echo false > "$d/draft";; esac
    [ -f "$d/state" ] || echo OPEN > "$d/state"; cp "$(val --body-file "$@")" "$d/body"
    echo https://github.com/o/r/pull/7;;
  "pr edit")
    t="$(val --title "$@")"; [ -n "$t" ] && echo "$t" > "$d/title"
    b="$(val --base "$@")"; [ -n "$b" ] && echo "$b" > "$d/base"
    f="$(val --body-file "$@")"; [ -n "$f" ] && cp "$f" "$d/body"; true;;
  "pr ready") case "$*" in *--undo*) echo true > "$d/draft";; *) echo false > "$d/draft";; esac;;
  "pr merge") [ -f "$d/noqueue" ] || touch "$d/queued";;
  "pr view")
    q=null; [ -f "$d/queued" ] && q='{"enabledAt":"x"}'
    printf '{"state":"%s","autoMergeRequest":%s}' "$(cat $d/state)" "$q";;
  "pr checks") [ -f "$d/checks" ] && cat "$d/checks" || echo "[]"; exit 8;;
  *) echo "unexpected gh $*" >&2; exit 1;;
esac
"#;
