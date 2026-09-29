CARGO ?= cargo
# The target we ship: the one `stellar contract build` uses and the upgrade
# tests deploy from.
WASM_TARGET ?= wasm32v1-none
# The target a plain `cargo build` picks. It is built as well so that the
# host-compatibility test in `registry/tests/wasm_targets.rs` has its artifacts
# to inspect — on current Rust its output is not loadable, and CI should say so
# rather than leave it to a deployment (README, "Build & test").
CANARY_WASM_TARGET ?= wasm32-unknown-unknown

.PHONY: build build-canary wasm-both test fmt clippy check

build:
	$(CARGO) build --workspace --target $(WASM_TARGET) --release

build-canary:
	$(CARGO) build --workspace --target $(CANARY_WASM_TARGET) --release

# Both wasm targets, so a host-incompatible artifact is caught by the test
# suite instead of at deploy time.
wasm-both: build build-canary

test: wasm-both
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check: fmt clippy test
