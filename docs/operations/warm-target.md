# The warm target/

Z2b builds start from a warm `target/` without `actions/cache`, which a run's runtime token can
pre-seed under the base scope so that a poisoned entry outlives the merge (ADR-21,
`docs/architecture/06-operations.md` §3, `05-security.md` T6).

## How it works

1. **Build (trusted).** `.github/workflows/warm-target.yml` runs on every push to `main` (and its
   `build` job refuses any other ref). It copies the checkout to the fixed path `/work/ws` (A-29:
   cargo fingerprints hold absolute paths), runs what Z2b compiles (`cargo clippy`, `cargo test
   --no-run`, `cargo doc`, all `--locked --workspace --all-features`), runs
   `cargo clean -p <workspace crates>` so only dependency artifacts remain, and packs `target/`
   with `$CARGO_HOME/registry/index` and `registry/cache` into `warm-target.tar.zst`
   (`.github/actions/warm-target/warm-target.sh pack`). Nothing is restored; the build is cold.
2. **Publish.** `publish`, the only job with `packages: write`, checks the archive against the
   sha256 in `build`'s outputs and pushes it to `ghcr.io/<owner>/assay-warm-target`, tagged with
   the commit SHA for humans. The job summary shows the digest and the TOML line to pin it.
3. **Verify.** `verify` pulls it back by digest with `fetch-by-digest`, compares the bytes with
   the build's sha256, and unpacks it through the restore allowlist.
4. **Pin.** The owner copies the digest into base `.assay/plugins.toml`:

   ```toml
   [assay]
   warm_target = "sha256:…"
   ```

   That edit is a trust-path change and is reviewed as one (`05` T3). assay refuses a
   `warm_target` that is not `sha256:<64 lowercase hex>` (`ConfigError::Digest`, and exit 1 from
   `fetch-by-digest.sh digest-from-config`). Leaving the pin on an older digest is safe: a stale
   warm `target/` only costs build time.
5. **Restore (Z2b).** After checking the PR out at `/work/ws` and installing the toolchain, and
   before anything builds, a Z2b job runs `.github/actions/warm-target` with `packages: read`. It
   reads the pin from the base checkout, fetches and verifies it, unpacks it (any entry that is
   not a regular file or directory under `target/`, `registry/index/` or `registry/cache/`, or
   that has an empty, `.` or `..` component, refuses the whole archive before anything is
   written; at most 2,000,000 entries and 16 GiB), then runs `cargo clean --locked -p` for every
   workspace package `cargo metadata --no-deps` lists, each name checked against
   `[A-Za-z0-9][A-Za-z0-9_-]*`.

## The Actions cache rule

`crates/assay-cli/tests/it/workflow_cache.rs` enforces the rule over every workflow and composite
action, with two allowlists: a job is decision-bearing unless it is exempted by name (today only
`rust.yml`'s `gate`, `aarch64` and `miri`, which are developer feedback that no assay decision
reads), and decision-bearing jobs and composite actions may only use `actions/checkout`,
`actions/upload-artifact`, `actions/download-artifact`, `dtolnay/rust-toolchain`,
`oras-project/setup-oras` and this repository's own composite actions. Adding an action to either
list is a review of whether it touches the cache.

## One-time setup

The package `ghcr.io/wet-ink-corporation/assay-warm-target` is created by the first push. Give it
the same settings as the assay binary's package (`docs/operations/release.md`): visibility
*Private*, and *Read* under *Manage Actions access* for every repository whose Z2b restores it.
Each push to `main` adds a package version; prune old versions in the package settings or with a
retention policy, keeping the one base config pins.

## A-29: path sensitivity

Both the build and Z2b use `/work/ws` and the runner image's `CARGO_HOME`, so fingerprints of
registry dependencies match. Whether a Z2b build actually reuses them (A-29) is measured in M1:
compare a Z2b `cargo clippy` with and without the restore, with `CARGO_LOG=cargo::core::compiler::fingerprint=info`
showing which units are dirty and why.
