#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$repo_root/fixtures/measurements-small.txt"
expected="$repo_root/fixtures/expected-small.out"
actual="$(mktemp)"

cleanup() {
  rm -f "$actual"
}
trap cleanup EXIT

cargo build --release --manifest-path "$repo_root/Cargo.toml"
"$repo_root/target/release/brrr" "$fixture" > "$actual"

if diff -u "$expected" "$actual"; then
  echo "correctness check passed"
else
  echo "correctness check failed" >&2
  exit 1
fi
