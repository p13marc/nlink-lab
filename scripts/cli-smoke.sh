#!/usr/bin/env bash
# Rootless smoke test of the nlink-lab binary. Exercises clap wiring, every
# example topology through validate/render, the JSON envelopes, and a few
# known-bad inputs that must fail fast (never hang). Used by the `cli-smoke`
# CI job and by `just ci`.
set -euo pipefail
bin=${1:-target/debug/nlink-lab}
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
fail=0
say() { printf '%s\n' "$*" >&2; }

say "== --help for every subcommand"
"$bin" --help >/dev/null
for cmd in $("$bin" --help | sed -n '/^Commands:/,/^$/p' | awk 'NR>1 && $1 !~ /^(help|Options:)$/ {print $1}'); do
  "$bin" "$cmd" --help >/dev/null || { say "FAIL: $cmd --help"; fail=1; }
done

say "== validate + render every example"
# examples/imports/ holds parametric modules that are only meaningful when
# imported (the lib test test_all_nll_examples_parse skips them the same way)
for f in $(find examples -name '*.nll' -not -path 'examples/imports/*' | sort); do
  timeout 20 "$bin" validate "$f" >/dev/null 2>"$tmp/err" || { say "FAIL validate $f: $(head -3 "$tmp/err")"; fail=1; continue; }
  timeout 20 "$bin" render "$f" >"$tmp/rt.nll" 2>"$tmp/err" || { say "FAIL render $f: $(head -3 "$tmp/err")"; fail=1; continue; }
  timeout 20 "$bin" render --json "$f" | python3 -c 'import json,sys; json.load(sys.stdin)' || { say "FAIL render --json $f"; fail=1; }
done

say "== known-bad inputs must fail fast (exit 1, never hang)"
printf 'lab "t"\nnode a\nnode a\n' > "$tmp/bad1.nll"
printf 'lab "t"\nnode a\nlink a:eth0 -- ghost:eth0\n' > "$tmp/bad2.nll"
printf 'lab "t"\nnode a {\n' > "$tmp/bad3.nll"
for f in "$tmp"/bad*.nll; do
  set +e; timeout 10 "$bin" validate "$f" >/dev/null 2>&1; rc=$?; set -e
  case $rc in
    1) ;;
    124) say "FAIL: $f hung"; fail=1 ;;
    *) say "FAIL: $f exit $rc (expected 1)"; fail=1 ;;
  esac
done

say "== completions"
for sh in bash zsh fish; do "$bin" completions "$sh" >/dev/null || { say "FAIL completions $sh"; fail=1; }; done

say "== status (no labs) --json is valid JSON"
XDG_STATE_HOME="$tmp/state" "$bin" status --json | python3 -c 'import json,sys; json.load(sys.stdin)' || { say "FAIL status --json"; fail=1; }

if [ "$fail" -ne 0 ]; then say "cli-smoke: FAILED"; exit 1; fi
say "cli-smoke: OK"
