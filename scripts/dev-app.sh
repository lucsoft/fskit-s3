#!/usr/bin/env bash
# One-command dev install of the app + its FSKit extension, run attached.
#
# `cargo run` CANNOT register the extension: it builds a bare, unsigned binary with
# no embedded .appex, so macOS has nothing to register as a File System Extension.
# Registration only ever comes from the signed .app bundle, and reliably only from
# a stable location (/Applications) -- which is what this does:
#   build the host bundle (Debug) -> install to /Applications -> run it attached.
#
# The extension only needs reinstalling when ext/ changes; for UI-only work,
# `cargo run -p fskit-s3-app` is enough once the extension is installed + enabled.
#
# Requires a full Xcode + the paid-team signing set up (see xcode/README.md).
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="fskit-s3.app"
DEST="/Applications/$APP_NAME"
EXEC="$DEST/Contents/MacOS/${APP_NAME%.app}"
DERIVED="build/dev"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# Regenerate the Xcode project if xcodegen is available (harmless if up to date).
if command -v xcodegen >/dev/null 2>&1; then
  xcodegen generate >/dev/null
fi

echo "==> Building $APP_NAME (Debug)"
xcodebuild -scheme fskit-s3-host -configuration Debug \
  -destination "platform=macOS,arch=$(uname -m)" \
  -derivedDataPath "$DERIVED" -quiet build

BUILT="$DERIVED/Build/Products/Debug/$APP_NAME"
[ -d "$BUILT" ] || { echo "error: build produced no $BUILT" >&2; exit 1; }

echo "==> Quitting any running instance"
osascript -e 'quit app "fskit-s3"' 2>/dev/null || true
# Also stop a bare `cargo run` UI instance, so only the installed bundle runs.
pkill -f "target/debug/fskit-s3-app" 2>/dev/null || true
sleep 1

echo "==> Installing to $DEST"
rm -rf "$DEST"
cp -R "$BUILT" "$DEST"

# Nudge LaunchServices so the embedded extension is (re)discovered from the new copy.
[ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$DEST" || true

echo "==> Running (Ctrl-C to quit)"
exec "$EXEC"
