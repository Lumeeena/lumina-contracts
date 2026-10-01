# Contributing

Install Rust stable, GNU Make, and the wasm targets:

```bash
rustup target add wasm32v1-none wasm32-unknown-unknown
rustup component add rustfmt clippy
```

Before opening a pull request, run the same checks used in CI:

```bash
make check
```

`make test` builds the workspace release wasm — for both wasm targets, so the
host-compatibility test can inspect each — before running tests, which is also
required by the registry upgrade tests. See the README's Build & Test section
for the individual targets and interface snapshot update instructions.
