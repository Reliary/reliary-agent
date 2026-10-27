#!/usr/bin/env bash
# Verify that every crate tarball COMPILES, resolving sibling crates from the
# tarballs produced by this workspace rather than from crates.io.
#
# Why this exists
# ---------------
# `cargo package` builds the tarball and then compiles it with every dependency
# resolved from the registry. That catches the class of bug that shipped
# 0.8.0-0.8.4 (a file referenced by `include_str!` that was outside the crate
# directory and therefore absent from the tarball) and the class that shipped
# 0.9.1 (a one-line declaration invisible to the classifier: not caught here,
# but caught by the install smoke test).
#
# It also creates a chicken-and-egg on every release: as soon as a crate uses a
# new API from a sibling, the sibling's tarball cannot verify until that sibling
# is PUBLISHED. So CI goes red on the release commit and green on the next
# push, which trains everyone to ignore the gate.
#
# This script removes the ordering trap without weakening the check: it packages
# every crate without verifying, extracts each tarball, points `[patch.crates-io]`
# at the extracted siblings, and compiles each tarball. Missing files, bad
# `include_str!` paths and API mismatches inside the tarball all still fail.
#
# Usage: scripts/verify_crate_tarballs.sh [--keep]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES=(reliary-core reliary-memory reliary-compress reliary-sift
        reliary-risk reliary-fix reliary-dead reliary-edit
        reliary-search reliary-output reliary-pack reliary-agent)
KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

WORK="$(mktemp -d "${TMPDIR:-/tmp}/reliary-pkgcheck.XXXXXX")"
cleanup() { [ "$KEEP" = 1 ] || rm -rf "$WORK"; }
trap cleanup EXIT

cd "$ROOT"
echo "packaging ${#CRATES[@]} crates into $WORK" >&2
for c in "${CRATES[@]}"; do
  # --no-verify: verification happens below, against the extracted siblings.
  # Remove older tarballs first so the glob cannot pick a stale version.
  rm -f target/package/"$c"-[0-9]*.crate
  cargo package -p "$c" --allow-dirty --no-verify >/dev/null
  crate_file=$(ls target/package/"$c"-[0-9]*.crate | head -1)
  mkdir -p "$WORK/$c"
  tar -xzf "$crate_file" -C "$WORK/$c" --strip-components=1
  echo "  unpacked $c" >&2
done

# `[patch.crates-io]` for every sibling, so a tarball compiles against the code
# in THIS workspace instead of whatever is on the registry.
patch_block() {
  local self="$1" out=""
  for c in "${CRATES[@]}"; do
    [ "$c" = "$self" ] && continue
    out+="$c = { path = \"$WORK/$c\" }"$'\n'
  done
  printf '%s' "$out"
}

fail=0
for c in "${CRATES[@]}"; do
  printf '%-18s ' "$c"
  # A tarball that already contains a [patch] table is rejected by crates.io.
  if grep -q '^\[patch' "$WORK/$c/Cargo.toml"; then
    echo "FAIL (tarball declares [patch])"
    fail=1
    continue
  fi
  printf '\n[patch.crates-io]\n%s' "$(patch_block "$c")" >> "$WORK/$c/Cargo.toml"
  # A packaged binary crate ships a Cargo.lock pinned to registry sources; the
  # patch cannot override a locked source, so regenerate it here. (`cargo
  # install` without --locked does the same thing.)
  rm -f "$WORK/$c/Cargo.lock"
  if ( cd "$WORK/$c" && cargo check --quiet --all-targets ) 2>"$WORK/$c.err"; then
    echo "compiles"
  else
    echo "FAIL"
    sed -n '1,20p' "$WORK/$c.err" >&2
    fail=1
  fi
done

exit "$fail"
