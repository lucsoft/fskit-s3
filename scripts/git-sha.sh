#!/usr/bin/env bash
# Print a build identity string for the current checkout: the short commit SHA,
# plus `-dirty` when the working tree has uncommitted changes. Used to stamp both
# the host app and the extension so the host can tell whether the extension it
# will launch was built from the same commit — see scripts/stamp-git-sha.sh and
# ext/build.rs. Prints `unknown` if git can't answer (never fails the build).
set -euo pipefail

# Xcode sets $PROJECT_DIR; standalone, resolve relative to this script.
cd "${PROJECT_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"

git describe --always --dirty --abbrev=12 2>/dev/null || echo unknown
