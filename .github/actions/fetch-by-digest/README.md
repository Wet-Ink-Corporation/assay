# fetch-by-digest

Pulls an OCI artifact from GHCR by a pinned sha256 digest and verifies it before anything can run
it (ADR-14, `docs/architecture/02-architecture.md` §4, `05-security.md` T14). It is the one
delivery mechanism for every pinned tool: the assay binary (`[assay] digest`), the warm `target/`
(`[assay] warm_target`) and plugin builds pinned `oci:sha256:…` (temper, weigh-in).

## What it guarantees

- These are refused before the registry is contacted (exit 2):
  - a pin that is not `sha256:<64 lowercase hex>`, such as a tag or a short or upper-case digest;
  - a `repository` that is not lowercase `registry[:port]/path`, including one with a tag, a
    digest or a leading `-`. GHCR names are lowercase, so callers must lowercase the owner (for
    example `ghcr.io/${OWNER,,}/assay` in bash);
  - an empty `out-dir`, or one containing a newline.
- The manifest's bytes must hash to the pin, it must be an OCI image manifest (and of
  `artifact-type`, when given), and every layer must have a non-negative integer size, bytes that
  hash to its descriptor digest and that size. Any mismatch fails the step (exit 1).
- Layers are staged in a private temporary directory and copied to `out-dir` only after every
  check has passed, so a failed fetch leaves nothing to execute. Each layer is installed under its
  `org.opencontainers.image.title`, which must be a plain file name (no `/`, not `.` or `..`) and
  unique within the manifest.
- The token reaches `oras` on stdin, never on the command line, and is not written to a docker
  config file.

## Use

The calling job grants `packages: read` (the pull) and, to read the pin from base config,
`contents: read`. Read the pin only from a checkout of the base SHA, never from the PR head (P2,
`05` T3).

```yaml
permissions:
  contents: read
  packages: read
steps:
  - uses: actions/checkout@<sha> # vX.Y.Z
    with:
      ref: ${{ github.event.pull_request.base.sha }}
      path: base
      persist-credentials: false
  - id: assay
    uses: Wet-Ink-Corporation/assay/.github/actions/fetch-by-digest@<40-hex commit sha> # vX.Y.Z
    with:
      repository: ghcr.io/wet-ink-corporation/assay
      config: base/.assay/plugins.toml
      selector: assay.digest                # or assay.warm_target, plugin.temper, plugin.weigh-in
      artifact-type: application/vnd.wet-ink.assay.binary.v1
      out-dir: ${{ runner.temp }}/assay
  - run: '"$DIR/assay"'
    env:
      DIR: ${{ steps.assay.outputs.dir }}
```

Inputs `digest` and `config` are exclusive. A `git:<sha>` plugin pin is not an OCI artifact and is
refused; such a tool is checked out by commit SHA instead. The composite action pins `setup-oras`
by commit SHA, and `setup-oras` checks the oras 1.3.4 download against its shipped sha256;
Dependabot proposes bumps to the action, but the `version:` input is bumped by hand.

For use from another repository, the assay repository's Actions settings must allow access from
repositories in the organisation (Settings, Actions, General, Access), and the GHCR package must
grant that repository read access (`docs/operations/release.md`, A-20).

## The script

`fetch-by-digest.sh` holds all the logic and runs without GitHub: `digest-from-config` reads a pin
from `plugins.toml` (python3 >= 3.11 `tomllib`), and `fetch` pulls and verifies (`oras`, `jq`,
`sha256sum`). Its usage, environment and exit codes are in its header.
`crates/assay-cli/tests/it/fetch_by_digest.rs` runs it against a fake `oras`, including a pinned
digest the registry does not honour, a tampered layer, tag pins and a path-escaping title.
