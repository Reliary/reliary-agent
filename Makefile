# Local integration-test entry points.
#
# `make e2e`         — offline Docker gate (Layer 1). No network, no key.
# `make e2e-opencode`— real OpenCode agent with a keyless free model (Layer 2).
#
# Both build the release binary first. Layer 2 needs `opencode` on PATH and
# network access to OpenCode's free tier; no API key is used.

BIN := target/release/reliary

.PHONY: build e2e e2e-opencode

build:
	cargo build --release

# Layer 1: the shipped binary in a clean container, driven as an agent would.
e2e: build
	docker build -f Dockerfile.e2e -t reliary-e2e .
	docker run --rm --network none reliary-e2e

# Layer 2: a real OpenCode agent (keyless free model) calling reliary's tools.
e2e-opencode: build
	RELIARY_BIN=$(CURDIR)/$(BIN) bash scripts/e2e_opencode.sh
