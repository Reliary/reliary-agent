#!/bin/bash
# Topological publish of workspace crates to crates.io via OIDC.
# Requires Rust 1.75+ (OIDC support built into cargo publish).
# Set up trusted publisher per crate:
#   https://doc.rust-lang.org/cargo/reference/publishing.html#trusted-publishers
set -e

# Order: leaves first, root last
CRATES=(
  "crates/reliary-core"
  "crates/reliary-search"
  "crates/reliary-compress"
  "crates/reliary-sift"
  "crates/reliary-risk"
  "crates/reliary-memory"
  "crates/reliary-fix"
  "crates/reliary-dead"
  "crates/reliary-output"
  "crates/reliary-agent"
)

for crate in "${CRATES[@]}"; do
  name=$(grep '^name =' "$crate/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
  echo "--- Publishing $name ---"
  # OIDC auth: cargo uses ACTIONS_ID_TOKEN_REQUEST_URL set by GH runner.
  # No CARGO_REGISTRY_TOKEN needed when trusted publisher is configured.
  cargo publish --manifest-path "$crate/Cargo.toml" --no-verify 2>&1
  # Crates.io index replication takes ~10s; sleep to avoid 429 on next crate
  sleep 15
  echo "  published $name"
done

echo "All crates published."
