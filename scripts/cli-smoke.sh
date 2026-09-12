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

say "== known-bad inputs must fail fast (exit 2 = validation/parse error, never hang)"
printf 'lab "t"\nnode a\nnode a\n' > "$tmp/bad1.nll"
printf 'lab "t"\nnode a\nlink a:eth0 -- ghost:eth0\n' > "$tmp/bad2.nll"
printf 'lab "t"\nnode a {\n' > "$tmp/bad3.nll"
# these two used to hang the parser forever (#13)
printf 'lab "t"\nnode a\ndefaults impair { bogus }\n' > "$tmp/bad4.nll"
printf 'lab "t"\nnode a\nmesh m {\n' > "$tmp/bad5.nll"
# 2001:db8::1 must lex as IPv6 (#14) and multi-line props must merge (#16)
printf 'lab "t"\nnode a\nnode b\nlink a:eth0 -- b:eth0 {\n  2001:db8::1/64 -- 2001:db8::2/64\n  delay 10ms\n  loss 1%%\n}\n' > "$tmp/good1.nll"
timeout 10 "$bin" render "$tmp/good1.nll" | grep -q 'delay 10ms loss 1%' || { say "FAIL: multi-line impairment not merged / IPv6 not lexed"; fail=1; }
for f in "$tmp"/bad*.nll; do
  set +e; timeout 10 "$bin" validate "$f" >/dev/null 2>&1; rc=$?; set -e
  case $rc in
    2) ;;
    124) say "FAIL: $f hung"; fail=1 ;;
    *) say "FAIL: $f exit $rc (expected 2)"; fail=1 ;;
  esac
done

say "== validate --json / graph --mermaid / exit codes"
"$bin" --json validate examples/simple.nll | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["valid"] is True, d' || { say "FAIL validate --json"; fail=1; }
set +e; "$bin" --json validate "$tmp/bad1.nll" >/dev/null 2>&1; rc=$?; set -e
[ "$rc" -eq 2 ] || { say "FAIL: validate --json on a bad file exited $rc (expected 2)"; fail=1; }
"$bin" graph --mermaid examples/simple.nll | grep -q '^graph LR' || { say "FAIL graph --mermaid"; fail=1; }
"$bin" render --mermaid examples/cookbook/satellite-mesh.nll | grep -q 'net_' || { say "FAIL render --mermaid"; fail=1; }
"$bin" validate --list-rules | grep -q 'unreferenced-node *warning' || { say "FAIL validate --list-rules"; fail=1; }
printf 'lab "w"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\nnode lonely\n' > "$tmp/warn.nll"
set +e; "$bin" validate --strict "$tmp/warn.nll" >/dev/null 2>&1; rc=$?; set -e
[ "$rc" -eq 2 ] || { say "FAIL: validate --strict exited $rc (expected 2)"; fail=1; }
"$bin" validate --allow unreferenced-node "$tmp/warn.nll" 2>&1 | grep -q WARN && { say "FAIL: --allow did not silence the warning"; fail=1; }
"$bin" graph examples/cookbook/satellite-mesh.nll | grep -q 'net:' || { say "FAIL: graph ignores network blocks"; fail=1; }

say "== fmt"
printf 'lab "f"\nnode   a{forward ipv4}\n' | "$bin" fmt - | grep -q '^node a { forward ipv4 }$' || { say "FAIL fmt -"; fail=1; }
"$bin" fmt --check examples >/dev/null || { say "FAIL: examples are not fmt-clean"; fail=1; }

say "== lint"
"$bin" lint --list-rules | grep -q 'no-assertions' || { say "FAIL lint --list-rules"; fail=1; }
"$bin" lint examples/simple.nll >/dev/null || { say "FAIL lint"; fail=1; }
set +e; "$bin" lint --strict "$tmp/warn.nll" >/dev/null 2>&1; rc=$?; set -e
[ "$rc" -eq 2 ] || { say "FAIL: lint --strict exited $rc (expected 2)"; fail=1; }
"$bin" --json lint examples/simple.nll | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "findings" in d, d' || { say "FAIL lint --json"; fail=1; }

say "== doctor / verify (rootless)"
set +e; "$bin" --json doctor > "$tmp/doctor.json" 2>/dev/null; rc=$?; set -e
[ "$rc" -eq 0 ] || [ "$rc" -eq 1 ] || { say "FAIL: doctor exited $rc"; fail=1; }
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert "checks" in d and isinstance(d["ok"], bool), d' "$tmp/doctor.json" || { say "FAIL doctor --json"; fail=1; }
set +e; XDG_STATE_HOME="$tmp/state" "$bin" verify no-such-lab >/dev/null 2>&1; rc=$?; set -e
[ "$rc" -eq 1 ] || { say "FAIL: verify on a missing lab exited $rc (expected 1)"; fail=1; }

say "== completions"
for sh in bash zsh fish; do "$bin" completions "$sh" >/dev/null || { say "FAIL completions $sh"; fail=1; }; done

say "== status (no labs) --json is valid JSON"
XDG_STATE_HOME="$tmp/state" "$bin" status --json | python3 -c 'import json,sys; json.load(sys.stdin)' || { say "FAIL status --json"; fail=1; }

if [ "$fail" -ne 0 ]; then say "cli-smoke: FAILED"; exit 1; fi
say "cli-smoke: OK"
