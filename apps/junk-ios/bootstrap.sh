#!/usr/bin/env bash
# Everything between a clean checkout and a project Xcode will open: the Rust static
# libraries for the device and the simulator, the Swift uniffi generates from them, the
# xcframework that carries both, and the project itself. Idempotent — run it again after
# any change to the Rust, and the app picks the change up.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$here/../.." && pwd)"
generated="$here/Generated"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

usage() {
    cat <<'USAGE'
usage: bootstrap.sh [--release | --debug] [-h]

Builds junk-ffi for the iOS device and simulator targets, generates the Swift bindings,
assembles Generated/JunkFFI.xcframework and runs xcodegen. Safe to run repeatedly.

  --release   build the Rust with optimisations (the default)
  --debug     build the Rust unoptimised
  -h, --help  this
USAGE
}

profile=release
cargo_profile=(--release)
while [ $# -gt 0 ]; do
    case "$1" in
    --release)
        profile=release
        cargo_profile=(--release)
        ;;
    --debug)
        profile=debug
        cargo_profile=()
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        printf '\nbootstrap.sh: unknown argument: %s\n' "$1" >&2
        exit 2
        ;;
    esac
    shift
done

# One line per step, so a run says what it did.
say() { printf '==> %s\n' "$*"; }

device_lib="$target_dir/aarch64-apple-ios/$profile/libjunk_ffi.a"
sim_lib="$target_dir/aarch64-apple-ios-sim/$profile/libjunk_ffi.a"

for triple in aarch64-apple-ios aarch64-apple-ios-sim; do
    say "Building junk-ffi for $triple ($profile)"
    cargo build --quiet --manifest-path "$root/Cargo.toml" -p junk-ffi \
        --target "$triple" "${cargo_profile[@]}"
done

# The library carries its own metadata, so the bindings come from it rather than from a UDL
# — either static library will do, and the simulator one is the one this machine can run.
say "Generating the Swift bindings from $sim_lib"
rm -rf "$generated/bindings"
mkdir -p "$generated/bindings"
cargo run --quiet --manifest-path "$root/Cargo.toml" -p junk-ffi --bin uniffi-bindgen -- \
    generate --library "$sim_lib" --language swift --out-dir "$generated/bindings"

# An xcframework wants the header and the module map in one directory, and the module map
# under the name clang looks for.
say "Assembling Generated/Headers with module.modulemap"
rm -rf "$generated/Headers"
mkdir -p "$generated/Headers"
cp "$generated/bindings/junk_ffiFFI.h" "$generated/Headers/junk_ffiFFI.h"
cp "$generated/bindings/junk_ffiFFI.modulemap" "$generated/Headers/module.modulemap"

say "Creating Generated/JunkFFI.xcframework"
rm -rf "$generated/JunkFFI.xcframework"
xcodebuild -create-xcframework \
    -library "$device_lib" -headers "$generated/Headers" \
    -library "$sim_lib" -headers "$generated/Headers" \
    -output "$generated/JunkFFI.xcframework" >/dev/null

say "Copying junk_ffi.swift into Generated/"
cp "$generated/bindings/junk_ffi.swift" "$generated/junk_ffi.swift"

# A device build needs a signing team, which is nobody's business but this machine's: it
# lives in Local.xcconfig, which is gitignored and usually absent. project.yml pulls it in
# only when it is there, so a checkout without one still generates.
if [ -f "$here/Local.xcconfig" ]; then
    say "Generating JunkIOS.xcodeproj (with Local.xcconfig)"
    local_xcconfig=true
else
    say "Generating JunkIOS.xcodeproj (no Local.xcconfig; simulator builds need none)"
    local_xcconfig=false
fi
JUNK_IOS_LOCAL_XCCONFIG="$local_xcconfig" \
    xcodegen generate --quiet --spec "$here/project.yml" --project "$here"

cat <<DONE

Done. To build and run:

  open $here/JunkIOS.xcodeproj

and Run (Cmd-R) on a simulator or, with a team in Local.xcconfig, on a device. Headlessly,
for the simulator, which needs no signing:

  xcodebuild -project $here/JunkIOS.xcodeproj -scheme JunkIOS \\
      -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \\
      -derivedDataPath $here/build CODE_SIGNING_ALLOWED=NO build

The simulator has no Bluetooth, so a scan there fails and the app says so; only a real
device can find a ring.
DONE
