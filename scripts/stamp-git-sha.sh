#!/usr/bin/env bash
# Xcode build phase: stamp the current git SHA into the built bundle's Info.plist
# under `FSKitS3GitSHA`, so the host can compare its own SHA against the SHA of
# the extension FSKit will actually launch (see FskitS3HostApp.swift).
#
# Runs as a target Run Script phase, i.e. BEFORE the target's code-signing step —
# so the signature covers the stamped value. Both the host and the extension use
# this same script.
set -euo pipefail

SHA="$("$PROJECT_DIR/scripts/git-sha.sh")"

# The built Info.plist inside the product being signed.
PLIST="${CODESIGNING_FOLDER_PATH}/Contents/Info.plist"
if [ ! -f "$PLIST" ]; then
  echo "stamp-git-sha: no Info.plist at $PLIST — skipping" >&2
  exit 0
fi

# `-replace` inserts the key if absent, overwrites it if present.
plutil -replace FSKitS3GitSHA -string "$SHA" "$PLIST"
echo "stamp-git-sha: ${PRODUCT_NAME:-?}=$SHA -> $PLIST"
