#!/usr/bin/env bash
set -euo pipefail

profile="${1:-kernel}"

case "$profile" in
  kernel)
    build_cmd=(cargo build -p hrafn-kernel)
    tree_cmd=(cargo tree -p hrafn-kernel -e features)
    ;;
  full)
    build_cmd=(cargo build --features full --bin hrafn)
    tree_cmd=(cargo tree --features full --bin hrafn -e features)
    ;;
  *)
    echo "usage: $0 [kernel|full]" >&2
    exit 2
    ;;
esac

echo "== Hrafn build profile: $profile =="
echo

echo "+ ${build_cmd[*]}"
"${build_cmd[@]}"

echo
echo "== dependency feature tree =="
echo "+ ${tree_cmd[*]}"
mkdir -p target
"${tree_cmd[@]}" > "target/hrafn-${profile}-features.txt"
echo "wrote target/hrafn-${profile}-features.txt"

echo
if command -v cargo-bloat >/dev/null 2>&1; then
  if [[ "$profile" == "kernel" ]]; then
    cargo bloat --release -p hrafn-kernel --crates > "target/hrafn-${profile}-bloat.txt"
  else
    cargo bloat --release --features full --bin hrafn --crates > "target/hrafn-${profile}-bloat.txt"
  fi
  echo "wrote target/hrafn-${profile}-bloat.txt"
else
  echo "cargo-bloat not installed; skipping size attribution"
  echo "install with: cargo install cargo-bloat"
fi
