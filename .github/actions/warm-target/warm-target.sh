#!/usr/bin/env bash
# The warm target/ for Z2b (ADR-21, 06 §3, 05 T6): packed by the trusted main-branch job in
# .github/workflows/warm-target.yml, restored by .github/actions/warm-target after
# fetch-by-digest has verified it. The archive is untrusted input on restore: only regular files
# and directories under target/, registry/index/ and registry/cache/ are accepted, and one
# entry outside that allowlist refuses the whole archive before anything is written.
#
# Usage:
#   warm-target.sh pack <workspace> <cargo-home> <archive>
#       Writes <workspace>/target and <cargo-home>/registry/{index,cache} as a zstd tar.
#       registry/src is left out; cargo re-extracts it from the cache.
#   warm-target.sh unpack <archive> <workspace> <cargo-home>
#       Checks every entry, then extracts target/ into <workspace> and registry/ into
#       <cargo-home>, with tarfile's `data` filter (no escaping path, no special bits).
#   warm-target.sh clean <workspace>
#       In <workspace>: `cargo clean --locked -p <name>...` for every workspace package that
#       `cargo metadata --no-deps` lists, so no workspace crate's artifact is reused.
#
# A-29: cargo fingerprints hold absolute paths; the workspace and CARGO_HOME must be at the same
# paths in the packing and the restoring job (the workflow uses /work/ws and the image's
# CARGO_HOME).
#
# Exit status: 0 done; 1 a refused archive, a refused package name or a failed tool; 2 usage.
set -euo pipefail

die() {
  printf 'warm-target: %s\n' "$2" >&2
  exit "$1"
}

usage() {
  die 2 "usage: $0 pack <workspace> <cargo-home> <archive> | unpack <archive> <workspace> <cargo-home> | clean <workspace>"
}

require() {
  local tool
  for tool in "$@"; do
    command -v "$tool" > /dev/null || die 1 "required tool '$tool' is not on PATH"
  done
}

# Reads a tar stream on stdin. `check` validates every entry; `extract` validates again and
# extracts. Entries: regular files and directories only, names relative with no empty, `.` or
# `..` component and no control character, under an allowlisted prefix; at most MAX_ENTRIES
# entries and MAX_BYTES of content (H-13).
read -r -d '' TAR_PY << 'PY' || true
import sys
import tarfile

mode, workspace, cargo_home = sys.argv[1], sys.argv[2], sys.argv[3]
MAX_ENTRIES = 2_000_000
MAX_BYTES = 16 * 1024**3
ALLOWED = (("target",), ("registry", "index"), ("registry", "cache"))
ROOTS = {"target": workspace, "registry": cargo_home}


def fail(message):
    print(f"warm-target: {message}", file=sys.stderr)
    sys.exit(1)


if not hasattr(tarfile, "data_filter"):
    fail("python3 with tarfile extraction filters (3.12, or 3.11.4 and later) is required")


def components(name):
    if not name or name.startswith("/") or any(ord(c) < 0x20 or ord(c) == 0x7F for c in name):
        fail(f"entry {name!r} is not a plain relative name")
    parts = name.split("/")
    if parts[-1] == "":
        parts.pop()
    if not parts or any(part in ("", ".", "..") for part in parts):
        fail(f"entry {name!r} has an empty, '.' or '..' component")
    return tuple(parts)


entries = 0
size = 0
try:
    # errorlevel 2: a filter refusal raises instead of skipping the entry silently.
    with tarfile.open(fileobj=sys.stdin.buffer, mode="r|", errorlevel=2) as archive:
        for member in archive:
            entries += 1
            if entries > MAX_ENTRIES:
                fail(f"more than {MAX_ENTRIES} entries")
            parts = components(member.name)
            if not any(parts[: len(prefix)] == prefix for prefix in ALLOWED):
                fail(f"entry {member.name!r} is outside target/, registry/index/ and registry/cache/")
            if not (member.isreg() or member.isdir()):
                fail(f"entry {member.name!r} is not a regular file or directory")
            size += member.size
            if size > MAX_BYTES:
                fail(f"more than {MAX_BYTES} bytes of content")
            if mode == "extract":
                archive.extract(member, ROOTS[parts[0]], filter="data")
except (tarfile.TarError, OSError) as error:
    fail(f"cannot read the archive: {error}")
if entries == 0:
    fail("the archive is empty")
PY

pack() {
  [[ $# -eq 3 ]] || usage
  local workspace=$1 cargo_home=$2 archive=$3 dir
  require tar zstd
  for dir in "$workspace/target" "$cargo_home/registry/index" "$cargo_home/registry/cache"; do
    [[ -d $dir && ! -L $dir ]] || die 1 "$dir is not a directory"
  done
  tar --create --file=- --directory="$workspace" target \
    --directory="$cargo_home" registry/index registry/cache \
    | zstd -T0 -3 -q -c > "$archive"
}

# tar_stream <archive> check|extract <workspace> <cargo-home>
tar_stream() {
  zstd -d -q -c -- "$1" | python3 -I -c "$TAR_PY" "$2" "$3" "$4"
}

unpack() {
  [[ $# -eq 3 ]] || usage
  local archive=$1 workspace=$2 cargo_home=$3
  require zstd python3
  [[ -f $archive ]] || die 1 "$archive is not a file"
  [[ -d $workspace ]] || die 1 "workspace $workspace is not a directory"
  mkdir -p -- "$cargo_home"
  # Two passes: nothing is written unless every entry passed the check.
  tar_stream "$archive" check "$workspace" "$cargo_home"
  tar_stream "$archive" extract "$workspace" "$cargo_home"
}

clean() {
  [[ $# -eq 1 ]] || usage
  local workspace=$1 names name packages=()
  require cargo jq
  cd -- "$workspace"
  # Checked in jq, where each name is still one string (command substitution would drop a
  # trailing empty one), and again below.
  names=$(cargo metadata --locked --no-deps --format-version 1 | jq -r '
    .packages[].name
    | if type == "string" and test("\\A[A-Za-z0-9][A-Za-z0-9_-]*\\z") then .
      else error("workspace package name \(tojson) is not a cargo package name") end') \
    || die 1 "cannot list the workspace packages"
  while IFS= read -r name; do
    [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]] \
      || die 1 "workspace package name '$name' is not a cargo package name"
    packages+=(-p "$name")
  done <<< "$names"
  ((${#packages[@]} > 0)) || die 1 "cargo metadata lists no workspace package"
  cargo clean --locked "${packages[@]}"
}

[[ $# -ge 1 ]] || usage
command=$1
shift
case $command in
  pack) pack "$@" ;;
  unpack) unpack "$@" ;;
  clean) clean "$@" ;;
  *) usage ;;
esac
