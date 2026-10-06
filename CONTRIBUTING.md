# Contributing

## Checks

Run these before sending a change. CI (`.github/workflows/ci.yml`) runs the same ones.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo build -p junk-core -p junk-fake -p junk-trace -p junk-colmi -p junk-soundcore -p junk-sony --target thumbv7em-none-eabihf   # rustup target add thumbv7em-none-eabihf
uv run --with pytest pytest tools
```

Rust 1.88 or newer. On Linux the BLE crate needs `libdbus-1-dev` and `pkg-config`.

Tests that need a device are manual. The replay tests, which feed recorded traces through the
drivers, are what CI runs and what a change must keep green.

## House style

- Drivers are sans-I/O: a `Driver` is a pure state machine over `Input` and `Output`. No clock,
  no sleeping, no I/O, `no_std + alloc` for the protocol crates (`SPEC.md` section 3).
- Never panic on bytes from a device. Malformed input becomes an `Unparsed` event or an error answer.
- Every public item has a doc comment (`missing_docs` is on). `unsafe` is forbidden except in
  `junk-rfcomm`, which documents why.
- Layouts live in the wire layer as typed enums and `binrw` structs, not as magic numbers in the
  protocol layer.
- A fixture that was not captured from a device must say so in its header and in its file name.

## Fixtures

A new trace comes with provenance: which device and firmware, how it was produced (the command
or tool), and what was made up or redacted. Add it to `fixtures/README.md`. A fixture must never
contain real Bluetooth addresses, serial numbers, other people's health data or personal
identifiers; replace them with made-up values and say so in the header. Do not commit raw
`.pklg` captures.

## Licence

The project is licensed under MIT OR Apache-2.0. By contributing you agree that your contribution
is dual-licensed under the same terms, without additional conditions.
