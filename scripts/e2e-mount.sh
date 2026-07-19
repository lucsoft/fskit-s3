#!/usr/bin/env bash
#
# End-to-end test through the REAL macOS mount stack.
#
# backend/tests/live_s3.rs drives the StorageBackend trait directly. This goes a
# layer up and exercises the whole stack the way a user (or Finder) does:
#
#   /sbin/mount -F -t fskit-s3  ->  fskitd  ->  the Rust FSKit extension
#     ->  StorageBackend  ->  S3 (RustFS) or the in-memory demo
#
# It mounts a fresh volume at a unique, throwaway mount point, runs the file
# lifecycle the request describes — create -> update -> update -> check
# stats/modified -> delete, plus truncate, rename and a directory — using plain
# shell tools (printf/cat/stat/mv/rm/mkdir), checks each result, then unmounts
# with `diskutil unmount` (which clears the fskitd registry entry cleanly). It
# uses a unique connection name + mount point per run and NEVER touches any
# pre-existing mount.
#
# Prerequisites
#   * macOS 26+ with the fskit-s3 extension installed AND enabled
#     (System Settings > General > Login Items & Extensions > File System
#     Extensions). Check: `pluginkit -mv | grep fskit-s3` shows a leading '+'.
#   * s3 mode: an S3 endpoint. Defaults to the compose.yaml RustFS
#     (`docker compose up -d`); override with RUSTFS_ENDPOINT / FSKIT_S3_*.
#   * Tests the *currently installed* extension build, not this checkout — rebuild
#     and reinstall the host app first if you want to exercise newer ext code.
#
# Usage
#   scripts/e2e-mount.sh [s3|memory]      # default: s3
#
# Exit status: 0 = every check passed; 1 = a check failed; 2 = setup/precondition
# problem; 0 with a "skipping" note if the s3 endpoint is unreachable.

set -euo pipefail

MODE="${1:-s3}"

# S3 config — defaults match compose.yaml's RustFS.
ENDPOINT="${RUSTFS_ENDPOINT:-http://localhost:9000}"
BUCKET="${FSKIT_S3_BUCKET:-test-bucket}"
REGION="${FSKIT_S3_REGION:-us-east-1}"
ACCESS_KEY_ID="${FSKIT_S3_ACCESS_KEY_ID:-fskit}"
SECRET_ACCESS_KEY="${FSKIT_S3_SECRET_ACCESS_KEY:-fskit-secret}"

FS_TYPE="fskit-s3"
EXT_ID="dev.lucsoft.fskit-s3.ext"

# Unique per run so parallel/re-runs, the fskitd registry, and any leftover S3
# keys never collide — and so we never reuse someone else's mount point.
STAMP="$$-$(date +%s)"
NAME="e2e-${STAMP}"
# Under $HOME, a real path: /tmp and /var are symlinks into /private on macOS, so
# `mount` would report a canonicalized string that no longer matches what we
# passed — breaking is_mounted (and thus cleanup). Canonicalized again post-mkdir.
MOUNT_POINT="${HOME:-/tmp}/fskit-s3/e2e-${STAMP}"

pass=0
fail=0
say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
ok() {
	printf '  \033[32mok\033[0m   %s\n' "$*"
	pass=$((pass + 1))
}
bad() {
	printf '  \033[31mFAIL\033[0m %s\n' "$*"
	fail=$((fail + 1))
}
# check DESC ACTUAL EXPECTED
check() {
	if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1: got [$2], want [$3]"; fi
}
exists() { [ -e "$1" ] && echo yes || echo no; }
# True if OUR mount point is in the mount table. Captures `mount` first and then
# pattern-matches, rather than `mount | grep -q` — a short-circuiting `grep -q`
# closes the pipe early and SIGPIPE-kills the writer, which `pipefail` then
# reports as a failure (a timing-dependent false negative).
is_mounted() {
	local table
	table="$(mount)"
	[[ "${table}" == *" on ${MOUNT_POINT} ("* ]]
}

# Always unmount OUR mount (never anyone else's), even on failure.
cleanup() {
	local code=$?
	if is_mounted; then
		say "cleanup: unmounting ${MOUNT_POINT}"
		diskutil unmount "${MOUNT_POINT}" >/dev/null 2>&1 ||
			diskutil unmount force "${MOUNT_POINT}" >/dev/null 2>&1 ||
			umount -f "${MOUNT_POINT}" >/dev/null 2>&1 ||
			echo "  WARNING: could not unmount ${MOUNT_POINT}; try: diskutil unmount force ${MOUNT_POINT}"
	fi
	rmdir "${MOUNT_POINT}" 2>/dev/null || true
	exit "$code"
}
trap cleanup EXIT

say "e2e mount test (mode=${MODE})"
[ "$(uname)" = "Darwin" ] || {
	echo "macOS only"
	exit 2
}
# `pluginkit -m -i <id>` exits 0 iff the extension is registered — a targeted
# query, so no `cmd | grep -q` pipe to SIGPIPE-race under pipefail.
if ! pluginkit -m -i "${EXT_ID}" >/dev/null 2>&1; then
	echo "The ${EXT_ID} extension isn't registered. Build & run the host app and"
	echo "enable it in System Settings > Login Items & Extensions."
	exit 2
fi

case "${MODE}" in
memory)
	SOURCE="/memory"
	;;
s3)
	code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${ENDPOINT}" || true)"
	if [ -z "${code}" ] || [ "${code}" = "000" ]; then
		echo "S3 endpoint ${ENDPOINT} unreachable — start it (docker compose up -d)"
		echo "or set RUSTFS_ENDPOINT. Skipping (not a failure)."
		exit 0
	fi
	SOURCE="/s3/${NAME}?bucket=${BUCKET}&access_key_id=${ACCESS_KEY_ID}&region=${REGION}&endpoint=${ENDPOINT}"
	;;
*)
	echo "usage: $0 [s3|memory]"
	exit 2
	;;
esac

# --- mount ------------------------------------------------------------------
mkdir -p "${MOUNT_POINT}"
# Canonicalize (resolve symlinks, collapse //) so MOUNT_POINT is byte-identical
# to what `mount` prints — is_mounted and cleanup depend on the exact string.
MOUNT_POINT="$(cd "${MOUNT_POINT}" && pwd -P)"
say "mount"
echo "  source: ${SOURCE}"
echo "  point:  ${MOUNT_POINT}"
# The secret rides -o (insecure, self-contained for the test) since a fresh
# per-run connection name has no Keychain item; config rides the source path.
if [ "${MODE}" = "s3" ]; then
	/sbin/mount -F -t "${FS_TYPE}" -o "secret=${SECRET_ACCESS_KEY}" "${SOURCE}" "${MOUNT_POINT}"
else
	/sbin/mount -F -t "${FS_TYPE}" "${SOURCE}" "${MOUNT_POINT}"
fi
# mount returns before the volume is usable; wait for it to appear.
for _ in $(seq 1 20); do
	is_mounted && break
	sleep 0.5
done
if is_mounted; then ok "mounted"; else
	bad "mount did not appear in the mount table"
	exit 1
fi

F="${MOUNT_POINT}/e2e-${STAMP}.txt"

# --- create -----------------------------------------------------------------
say "create empty file"
: >"${F}"
check "exists" "$(exists "${F}")" "yes"
check "size after create" "$(stat -f '%z' "${F}")" "0"

# --- first update -----------------------------------------------------------
say "first update: write 'hello'"
printf 'hello' >"${F}"
check "content" "$(cat "${F}")" "hello"
check "size" "$(stat -f '%z' "${F}")" "5"
m1="$(stat -f '%m' "${F}")"

# S3 Last-Modified is 1s-granular; space the updates so mtime can move.
sleep 1.1

# --- second update ----------------------------------------------------------
say "second update: append ' world'"
printf ' world' >>"${F}"
check "content" "$(cat "${F}")" "hello world"
check "size" "$(stat -f '%z' "${F}")" "11"
m2="$(stat -f '%m' "${F}")"

# --- modified state ---------------------------------------------------------
say "modified state + full stats"
stat -f '  mode=%Sp uid=%u size=%z mtime="%Sm" name=%N' "${F}"
if [ "${MODE}" = "s3" ]; then
	if [ "${m2}" -gt "${m1}" ]; then ok "mtime advanced after modification (${m1} -> ${m2})"; else
		bad "mtime did not advance (${m1} -> ${m2})"
	fi
else
	# The in-memory demo reports a process-stable instant, so mtime is constant;
	# just record it rather than assert a direction.
	echo "  (memory) mtime ${m1} -> ${m2}"
fi

# --- truncate (shrink via a shorter overwrite) ------------------------------
say "truncate: overwrite with shorter 'hi'"
printf 'hi' >"${F}"
check "content" "$(cat "${F}")" "hi"
check "size" "$(stat -f '%z' "${F}")" "2"

# --- rename -----------------------------------------------------------------
say "rename (mv)"
G="${MOUNT_POINT}/e2e-${STAMP}-renamed.txt"
mv "${F}" "${G}"
check "old path gone" "$(exists "${F}")" "no"
check "new path content" "$(cat "${G}")" "hi"

# --- directory listing ------------------------------------------------------
say "directory listing shows the renamed file"
ls -la "${MOUNT_POINT}" | grep -F "e2e-${STAMP}" || true
check "listed" "$(ls -1 "${MOUNT_POINT}" | grep -Fxc "e2e-${STAMP}-renamed.txt" || true)" "1"

# --- delete -----------------------------------------------------------------
say "delete"
rm "${G}"
check "file removed" "$(exists "${G}")" "no"
check "stat fails after delete" "$(stat -f '%z' "${G}" 2>/dev/null || echo GONE)" "GONE"

# --- directory create/remove ------------------------------------------------
say "directory create/remove"
D="${MOUNT_POINT}/e2e-${STAMP}-dir"
mkdir "${D}"
check "dir created" "$([ -d "${D}" ] && echo yes || echo no)" "yes"
rmdir "${D}"
check "dir removed" "$([ -d "${D}" ] && echo yes || echo no)" "no"

# --- summary ----------------------------------------------------------------
say "summary"
printf '  passed: %d   failed: %d\n' "${pass}" "${fail}"
[ "${fail}" -eq 0 ]
