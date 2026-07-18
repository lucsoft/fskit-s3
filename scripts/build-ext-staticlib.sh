#!/usr/bin/env bash
# Build the fskit-s3-ext Rust staticlib for the arch(es) Xcode is building and
# place a (possibly universal) libfskit_s3_ext.a in $BUILT_PRODUCTS_DIR for the
# extension target to link. Intended as an Xcode "Run Script" build phase that
# runs BEFORE "Compile Sources"; also works standalone (defaults to arm64/Release).
set -euo pipefail
cd "$(dirname "$0")/.."

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

libs=()
for t in "${targets[@]}"; do
  rustup target add "$t" >/dev/null 2>&1 || true
  cargo build -p fskit-s3-ext $PROFILE_FLAG --target "$t"
  libs+=("target/$t/$PROFILE_DIR/libfskit_s3_ext.a")
done

OUT="${BUILT_PRODUCTS_DIR:-target}/libfskit_s3_ext.a"
mkdir -p "$(dirname "$OUT")"
if [ "${#libs[@]}" -gt 1 ]; then
  lipo -create "${libs[@]}" -output "$OUT"
else
  cp "${libs[0]}" "$OUT"
fi
echo "fskit-s3: built $OUT"
