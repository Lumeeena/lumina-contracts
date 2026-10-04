CARGO ?= cargo
# The target we ship: the one `stellar contract build` uses and the upgrade
# tests deploy from.
WASM_TARGET ?= wasm32v1-none
# The target a plain `cargo build` picks. It is built as well so that the
# host-compatibility test in `registry/tests/wasm_targets.rs` has its artifacts
# to inspect — on current Rust its output is not loadable, and CI should say so
# rather than leave it to a deployment (README, "Build & test").
CANARY_WASM_TARGET ?= wasm32-unknown-unknown
WASM_PATH ?= target/$(WASM_TARGET)/release/lumina_registry.wasm
WASM_SIZE_BUDGET_FILE ?= wasm-size-budget.txt

CLI_BIN := target/release/registry-cli

.PHONY: build build-canary wasm-both test fmt clippy check wasm-size cli


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

cli:
	$(CARGO) build -p registry-cli --release

	@if [ -z "$$NETWORK" ]; then echo "not running cli smoke test: NETWORK not set"; else $(CLI_BIN) --network "$$NETWORK" help; fi

wasm-size: build
	# Measure the compiled contract wasm size and enforce the committed budget.
	@actual=$$(wc -c < "$(WASM_PATH)" | tr -d ' [:space:]'); \
	budget=$$(grep -v '^#' "$(WASM_SIZE_BUDGET_FILE)" | tr -d ' \r\n'); \
	echo "WASM size: $$actual bytes (budget: $$budget bytes)"; \
	if [ "$$actual" -gt "$$budget" ]; then \
	  echo "ERROR: WASM size $$actual bytes exceeds budget $$budget bytes" >&2; \
	  exit 1; \
	fi

check: fmt clippy test wasm-size