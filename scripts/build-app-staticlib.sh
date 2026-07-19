#!/usr/bin/env bash
# Build the fskit-s3-app Rust staticlib for the arch(es) Xcode is building and
# place a (possibly universal) libfskit_s3_app.a in $BUILT_PRODUCTS_DIR for the
# host app target to link. Intended as an Xcode "Run Script" build phase that runs
# BEFORE "Compile Sources"; also works standalone (defaults to arm64/Release).
#
# The host target's Swift bootstrap (xcode/host/main.swift) then just calls the
# staticlib's `fskit_s3_app_run` C entry — the whole app (status-bar UI, mounts,
# extension health) is this one Rust library, so there's no separate host app to
# keep in sync. Mirror of build-ext-staticlib.sh.
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
