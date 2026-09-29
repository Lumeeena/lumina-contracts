# Contributing

Install Rust stable, GNU Make, and the Soroban wasm target:

```bash
rustup target add wasm32v1-none
rustup component add rustfmt clippy
```

Before opening a pull request, run the same checks used in CI:

```bash
make check
```

`make test` builds the workspace release wasm before running tests, which is
required by the registry upgrade tests. See the README's Build & Test section
for the individual targets and interface snapshot update instructions.
