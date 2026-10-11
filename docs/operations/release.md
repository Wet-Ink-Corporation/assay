# Releasing assay

The assay binary is delivered by digest, never by tag or release asset (ADR-14,
`docs/architecture/02-architecture.md` §4, `05-security.md` T14). `.github/workflows/release.yml`
builds it from a `v*` tag and publishes it to GHCR as an OCI artifact; consumers pin the digest in
base `.assay/plugins.toml` `[assay] digest` and fetch it with
`.github/actions/fetch-by-digest`, which aborts on any digest mismatch.

## One-time setup

1. **Protect release tags.** Repository ruleset (Settings, Rules, Rulesets, New tag ruleset):
   target `refs/tags/v*`; enforcement *Active*; rules *Restrict creations*, *Restrict updates*,
   *Restrict deletions* and *Block force pushes*; bypass list: the owner only. Without it, anyone
   with write access could push a `v*` tag and run the release workflow. The workflow also refuses
   a tag whose commit is not on `main` or whose name is not `v<workspace version>`.
2. **Protect the release workflow.** `.github/workflows/release.yml` and `.github/actions/**` are
   trust paths (02 §7) and fall under the ruleset that restricts `.github/workflows/**` edits to
   the owner (`05 §7`).
3. **Package settings** (after the first release creates `ghcr.io/wet-ink-corporation/assay`):
   visibility *Private*; under *Manage Actions access* add every consumer repository with the
   *Read* role. The `org.opencontainers.image.source` annotation links the package to this
   repository, which gives this repository's own workflows access.
4. **Action access.** Settings, Actions, General, Access: *Accessible from repositories in the
   Wet-Ink-Corporation organization*, so consumer workflows can `uses:` the composite action.

## Cutting a release

1. On `main`, set `version` in `[workspace.package]` of the root `Cargo.toml` and merge.
2. Tag the merge commit `v<version>` and push the tag (the owner, per the ruleset).
3. The workflow runs `build` (locked, release profile, no cache), `publish` (the only job with
   `packages: write`), `verify` (pulls the release back by digest with `packages: read` through
   `fetch-by-digest` and checks the bytes against the build) and `release-notes` (the only job with
   `contents: write`; it runs no repository code).
4. Copy the digest from the job summary or the GitHub release notes into each consumer's base
   `.assay/plugins.toml` `[assay] digest`. That edit is a trust-path change and is reviewed as one
   (`05` T3).

temper and weigh-in builds use the same mechanism: their release jobs publish an OCI artifact,
and the consumer pins `pin = "oci:sha256:…"` and fetches with `selector: plugin.<name>`. A
`git:<commit sha>` pin is checked out by SHA instead.

## A-20: GITHUB_TOKEN pulls across private repositories

A-20 assumes that a workflow in private consumer repository A can pull the org's GHCR package
published from this repository using A's own `GITHUB_TOKEN` with `packages: read`, once the
package grants A read access. It cannot be verified before the first real release, because the
package does not exist until then. **Status: unverified.**

Verification, after the first release:

1. In the package settings, grant one private consumer repository (or a scratch private
   repository in the org) *Read* under *Manage Actions access*.
2. In that repository, run a `workflow_dispatch` workflow with only `contents: read` and
   `packages: read` that calls `fetch-by-digest` with the released `repository` and `digest`.
   Expected: the step succeeds and the binary's sha256 matches the release notes.
3. Negative control: remove the repository from *Manage Actions access* and re-run. Expected: the
   manifest fetch fails (`denied` or `not found`) and the job aborts with nothing installed.
4. Record the result, with links to both runs, in the A-20 row of
   `docs/architecture/07-decisions-assumptions.md` and on Story #68's A-20 checkbox.

If A-20 fails, the fallbacks are those in 07: vendor the pinned binary into base `.assay/`, or a
read-only deploy credential held in Z1 only.
