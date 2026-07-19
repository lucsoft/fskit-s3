#!/usr/bin/env bash
# Build the fskit-s3-app Rust staticlib for the arch(es) Xcode is building and
# place a (possibly universal) libfskit_s3_app.a in $BUILT_PRODUCTS_DIR for the
# host app target to link. Intended as an Xcode "Run Script" build phase that runs
# BEFORE "Compile Sources"; also works standalone (defaults to arm64/Release).
#
# The host target is a SwiftUI app that reaches this staticlib through a UniFFI
# contract (app/src/ffi.rs); this script also refreshes the generated Swift bindings
# in xcode/host/Generated/ so they can't drift from the contract. Mirror of
# build-ext-staticlib.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

# Xcode "Run Script" phases run with a minimal PATH that omits rustup's
# ~/.cargo/bin and Homebrew, so `cargo`/`rustup` aren't found. Add them.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

CONFIG="${CONFIGURATION:-Release}"
if [ "$CONFIG" = "Debug" ]; then PROFILE_DIR="debug"; PROFILE_FLAG=""; else PROFILE_DIR="release"; PROFILE_FLAG="--release"; fi

# Map Xcode ARCHS -> Rust targets.
targets=()
for a in ${ARCHS:-arm64}; do
  case "$a" in
    arm64) targets+=("aarch64-apple-darwin") ;;
    x86_64) targets+=("x86_64-apple-darwin") ;;
    *) echo "warning: unknown arch $a" >&2 ;;
  esac
done

# Build each arch. The crate emits `staticlib` (the .a we link) AND `cdylib` (the
# .dylib uniffi-bindgen reads) in one pass, so no separate build for the bindings.
libs=()
for t in "${targets[@]}"; do
  rustup target add "$t" >/dev/null 2>&1 || true
  cargo build -p fskit-s3-app $PROFILE_FLAG --target "$t"
  libs+=("target/$t/$PROFILE_DIR/libfskit_s3_app.a")
done

OUT="${BUILT_PRODUCTS_DIR:-target}/libfskit_s3_app.a"
mkdir -p "$(dirname "$OUT")"
if [ "${#libs[@]}" -gt 1 ]; then
  lipo -create "${libs[@]}" -output "$OUT"
else
  cp "${libs[0]}" "$OUT"
fi
echo "fskit-s3: built $OUT"

# During a SwiftUI **preview** build the committed bindings are already current, so
# skip regeneration entirely: previews shouldn't pay for bindgen, and — crucially —
# a Run Script that rewrites a *compiled source* (Generated/*.swift) mid-build
# thrashes the preview's incremental build and can stop the canvas from rendering.
if [ "${ENABLE_PREVIEWS:-NO}" = "YES" ]; then
  echo "fskit-s3: preview build — skipping Swift binding regeneration"
  exit 0
fi

# Regenerate the Swift bindings from the freshly built library so the contract
# (app/src/ffi.rs) and the Swift the host compiles can never drift. UniFFI's library
# mode reads the metadata out of the cdylib built above.
GEN_TARGET="${targets[0]}"
DYLIB="target/$GEN_TARGET/$PROFILE_DIR/libfskit_s3_app.dylib"
TMP="$(mktemp -d)"
cargo run -q -p uniffi-bindgen -- generate \
  --library "$DYLIB" \
  --language swift \
  --out-dir "$TMP"

# Copy over only the two files we use (the C header is consumed via the bridging
# header, not a Clang module, so the generated modulemap is dropped), and only when
# the content actually changed — leaving mtimes untouched otherwise keeps Xcode's
# incremental and preview builds from seeing a phantom source change every build.
mkdir -p xcode/host/Generated
for f in fskit_s3_app.swift fskit_s3_appFFI.h; do
  if ! cmp -s "$TMP/$f" "xcode/host/Generated/$f" 2>/dev/null; then
    cp "$TMP/$f" "xcode/host/Generated/$f"
    echo "fskit-s3: updated xcode/host/Generated/$f"
  fi
done
rm -rf "$TMP"
