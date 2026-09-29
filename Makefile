CARGO ?= cargo
WASM_TARGET ?= wasm32v1-none

.PHONY: build test fmt clippy check

build:
	$(CARGO) build --workspace --target $(WASM_TARGET) --release

test: build
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check: fmt clippy test
