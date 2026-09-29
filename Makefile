CARGO ?= cargo
WASM_TARGET ?= wasm32v1-none
WASM_PATH ?= target/$(WASM_TARGET)/release/lumina_registry.wasm
WASM_SIZE_BUDGET_FILE ?= wasm-size-budget.txt

PHONY: build test fmt clippy check wasm-size

build:
	$(CARGO) build --workspace --target $(WASM_TARGET) --release

test: build
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets - -D warnings

check: fmt clippy test wasm-size

wasm-size: build
	# Measure the compiled contract wasm size and enforce the committed budget.
	@actual=$$($wc -c < "$(WASM_PATH)" | tr -d ' [:space:]'); \
	  echo "$actual"); \
	budget=$$(grep -v '^[^0-9]*' "$(WASM_SIZE_BUDGET_FILE)" | head -n 1 | tr -d ' \r\n'); \
	echo "WASM size: $actual bytes (budget: $budget bytes)"; \
	if [ "$actual" -gt "$budget" ]; then \
	  echo "ERROR: WASM size $actual bytes exceeds budget $budget bytes" >&2; \
	  exit 1; \
	fi
