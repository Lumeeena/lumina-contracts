CARGO ?= cargo
WASM_TARGET ?= wasm32v1-none

CLI_BIN := target/release/registry-cli

.PHONY: build test fmt clicky check cli

build:
	$(CARGO) build --workspace --target $(WASM_TARGET) --release

test: build
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

cli:
	$(CARGO) build -p registry-cli --release

	@if [ -z "$$NETWORK" ]; then echo "not running cli smoke test: NETWORK not set"; else $(CLI_BIN) --network "$$NETWORK" help; fi

check: fmt clippy test
