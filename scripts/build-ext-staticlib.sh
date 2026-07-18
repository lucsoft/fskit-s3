#!/usr/bin/env bash
# Build the fskit-s3-ext Rust staticlib for the arch(es) Xcode is building and
# place a (possibly universal) libfskit_s3_ext.a in $BUILT_PRODUCTS_DIR for the
# extension target to link. Intended as an Xcode "Run Script" build phase that
# runs BEFORE "Compile Sources"; also works standalone (defaults to arm64/Release).
set -euo pipefail
cd "$(dirname "$0")/.."

# Xcode "Run Script" phases run with a minimal PATH that omits rustup's
# ~/.cargo/bin and Homebrew, so `cargo`/`rustup` aren't found. Add them.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Compile the git SHA into the staticlib (ext/build.rs reads this), so the SHA
# the extension logs at activate matches the one stamped into its Info.plist.
export FSKIT_S3_GIT_SHA="$("$(dirname "$0")/git-sha.sh")"

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
