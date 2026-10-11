#!/usr/bin/env bash
# Installs an OCI artifact only after verifying it against a pinned sha256 digest: the manifest's
# bytes must hash to the pin, and every layer's bytes must hash to (and be the size of) its
# descriptor in that manifest. Nothing reaches the output directory before every check passes
# (ADR-14, 02 §4, 05 T14). Tags are never accepted.
#
# Usage:
#   fetch-by-digest.sh digest-from-config <plugins.toml> <selector>
#       Prints the pinned digest (sha256:<hex>). Selectors: assay.digest, assay.warm_target,
#       plugin.<name> (the plugin's pin must be oci:sha256:<hex>; git: pins are not OCI).
#   fetch-by-digest.sh fetch <repository> <sha256:hex> <out-dir>
#       <repository> has no tag or digest, e.g. ghcr.io/wet-ink-corporation/assay. Each layer
#       is installed as <out-dir>/<its org.opencontainers.image.title>.
#
# Environment (never argv, so the token is not in the process list):
#   FETCH_BY_DIGEST_USERNAME, FETCH_BY_DIGEST_PASSWORD  registry credentials (packages: read);
#                                                       anonymous when the password is unset
#   FETCH_BY_DIGEST_ARTIFACT_TYPE                       if set, the manifest's artifactType
#   FETCH_BY_DIGEST_MODE                                file mode of installed layers (0755)
#   GITHUB_OUTPUT                                       if set, receives digest= and dir=
#
# Exit status: 0 done; 1 verification, fetch or configuration failure (nothing installed);
# 2 usage error, including a pin that is not sha256:<64 lowercase hex>.
set -euo pipefail

readonly MANIFEST_TYPE=application/vnd.oci.image.manifest.v1+json
readonly TITLE=org.opencontainers.image.title

die() {
  printf 'fetch-by-digest: %s\n' "$2" >&2
  exit "$1"
}

usage() {
  die 2 "usage: $0 digest-from-config <plugins.toml> <selector> | fetch <repository> <sha256:hex> <out-dir>"
}

is_digest() {
  [[ $1 =~ ^sha256:[0-9a-f]{64}$ ]]
}

require() {
  local tool
  for tool in "$@"; do
    command -v "$tool" > /dev/null || die 1 "required tool '$tool' is not on PATH"
  done
}

sha256_of() {
  local sum
  sum=$(sha256sum < "$1")
  printf 'sha256:%s' "${sum%% *}"
}

# oras_get manifest|blob <reference@digest> <destination>
oras_get() {
  if [[ -n ${FETCH_BY_DIGEST_PASSWORD:-} ]]; then
    printf '%s' "$FETCH_BY_DIGEST_PASSWORD" \
      | oras "$1" fetch --username "${FETCH_BY_DIGEST_USERNAME:-token}" --password-stdin --output "$3" "$2"
  else
    oras "$1" fetch --output "$3" "$2"
  fi
}

# verify_blob <file> <what> <expected digest> [<expected size>]
verify_blob() {
  local actual size
  actual=$(sha256_of "$1")
  [[ $actual == "$3" ]] || die 1 "digest mismatch for $2: pinned $3, got $actual"
  if [[ $# -eq 4 ]]; then
    size=$(wc -c < "$1")
    [[ $((size)) -eq $4 ]] || die 1 "size mismatch for $2: descriptor says $4 bytes, got $((size))"
  fi
}

digest_from_config() {
  [[ $# -eq 2 ]] || usage
  require python3
  python3 -I - "$1" "$2" << 'PY'
import re
import sys

path, selector = sys.argv[1], sys.argv[2]


def fail(message, status=1):
    print(f"fetch-by-digest: {message}", file=sys.stderr)
    sys.exit(status)


if sys.version_info < (3, 11):
    fail("python3 >= 3.11 is required (tomllib)")
import tomllib

try:
    with open(path, "rb") as handle:
        config = tomllib.load(handle)
except (OSError, tomllib.TOMLDecodeError) as error:
    fail(f"cannot read {path}: {error}")

if selector in ("assay.digest", "assay.warm_target"):
    assay = config.get("assay")
    value = assay.get(selector.split(".", 1)[1]) if isinstance(assay, dict) else None
elif selector.startswith("plugin.") and len(selector) > len("plugin."):
    name = selector[len("plugin."):]
    plugins = config.get("plugin", [])
    matches = [p for p in plugins if isinstance(p, dict) and p.get("name") == name] if isinstance(plugins, list) else []
    if len(matches) != 1:
        fail(f"expected exactly one [[plugin]] named {name!r} in {path}, found {len(matches)}")
    pin = matches[0].get("pin")
    if not isinstance(pin, str) or not pin.startswith("oci:"):
        fail(f"plugin {name!r} pin {pin!r} is not oci:sha256:<hex>; a git: pin is fetched by commit SHA, not from a registry")
    value = pin[len("oci:"):]
else:
    fail(f"unknown selector {selector!r}: use assay.digest, assay.warm_target or plugin.<name>", 2)

if not isinstance(value, str) or re.fullmatch(r"sha256:[0-9a-f]{64}", value) is None:
    fail(f"{selector} in {path} is {value!r}, not sha256:<64 lowercase hex> (tags are never accepted)")
print(value)
PY
}

fetch() {
  [[ $# -eq 3 ]] || usage
  local repository=$1 digest=$2 out=$3
  is_digest "$digest" || die 2 "pin '$digest' is not sha256:<64 lowercase hex>; tags are never accepted"
  [[ $repository != *@* && ${repository##*/} != *:* ]] \
    || die 2 "repository '$repository' must not carry a tag or digest"
  require oras jq sha256sum

  # Global, so the EXIT trap still sees it after this function returns or dies.
  stage=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/fetch-by-digest.XXXXXX")
  trap 'rm -rf -- "$stage"' EXIT
  mkdir "$stage/layers"

  oras_get manifest "$repository@$digest" "$stage/manifest.json" \
    || die 1 "could not fetch the manifest $repository@$digest"
  verify_blob "$stage/manifest.json" "the manifest" "$digest"

  local manifest=$stage/manifest.json
  jq -e --arg type "$MANIFEST_TYPE" '.mediaType == $type' "$manifest" > /dev/null \
    || die 1 "$repository@$digest is not an OCI image manifest"
  if [[ -n ${FETCH_BY_DIGEST_ARTIFACT_TYPE:-} ]]; then
    jq -e --arg type "$FETCH_BY_DIGEST_ARTIFACT_TYPE" '.artifactType == $type' "$manifest" > /dev/null \
      || die 1 "$repository@$digest is not of artifact type $FETCH_BY_DIGEST_ARTIFACT_TYPE"
  fi

  local layers
  layers=$(jq -r --arg title "$TITLE" '
    if (.layers | type) == "array" and (.layers | length) > 0 then
      .layers[] | [(.digest | tostring), (.size | tostring), (.annotations[$title] // "" | tostring)] | @tsv
    else error("no layers") end' "$manifest") \
    || die 1 "$repository@$digest has no layers"

  local layer_digest size title titles=()
  while IFS=$'\t' read -r layer_digest size title; do
    is_digest "$layer_digest" || die 1 "layer digest '$layer_digest' is not sha256:<64 lowercase hex>"
    [[ $size =~ ^[0-9]+$ ]] || die 1 "layer $layer_digest has no valid size"
    [[ $title =~ ^[A-Za-z0-9._-]+$ && $title != . && $title != .. ]] \
      || die 1 "layer $layer_digest title '$title' is not a plain file name"
    [[ ! -e $stage/layers/$title ]] || die 1 "two layers are titled '$title'"
    oras_get blob "$repository@$layer_digest" "$stage/layers/$title" \
      || die 1 "could not fetch layer $layer_digest"
    verify_blob "$stage/layers/$title" "layer '$title'" "$layer_digest" "$size"
    titles+=("$title")
  done <<< "$layers"

  # Every check passed: only now does anything reach the output directory.
  mkdir -p -- "$out"
  for title in "${titles[@]}"; do
    install -m "${FETCH_BY_DIGEST_MODE:-0755}" -- "$stage/layers/$title" "$out/$title"
    printf '%s\n' "$out/$title"
  done
  if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'digest=%s\ndir=%s\n' "$digest" "$out" >> "$GITHUB_OUTPUT"
  fi
}

[[ $# -ge 1 ]] || usage
command=$1
shift
case $command in
  digest-from-config) digest_from_config "$@" ;;
  fetch) fetch "$@" ;;
  *) usage ;;
esac
