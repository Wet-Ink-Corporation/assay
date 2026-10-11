---
paths:
  - "**/Cargo.toml"
  - "Cargo.lock"
  - "**/build.rs"
---

# Manifest rules (rust-craft)

- Declare every dependency once, in the root `[workspace.dependencies]`, as a caret floor at the oldest version whose API you use (`"1.4.2"`), never `=`, `~`, `*` or an upper bound; the one exception is a proc-macro companion crate you re-export, pinned with `=`. Put `default-features = false` and the baseline features there; members write `name = { workspace = true }` and only add `features` (ARC-12). Sibling crates are workspace dependencies too (ARC-01).
- A new dependency, a major-version bump, a `[patch]` or `[replace]` entry and a `rust-version` raise are human decisions (H-16). Pre-approved crates are listed in `.claude/temper.json`. Justify a dependency with maintenance status, size, license and `cargo deny check`; read its features from `cargo info <crate>`, not memory.
- Every member has `[lints]\nworkspace = true`; the hook refuses a member without it. Lint levels live only in the root `[workspace.lints]` (MOD-09). Changing them is H-05 and needs human approval.
- Members inherit `edition`, `rust-version`, `license`, `version` and `publish` from `[workspace.package]` (ARC-01). Never pass `--ignore-rust-version` (MOD-01). Turning `publish` on is H-16, and `cargo_common_metadata` then requires the crates.io metadata.
- `[profile.*]` tables live only in the root manifest; cargo ignores them in members (PERF-09).
- Each crate enables every dependency feature its own code uses, never relying on a sibling or a dev-dependency to switch it on (ARC-16). Check with `cargo check -p <crate>`.
- Features are additive, named for what they enable (`serde`, never `use-serde` or `no-std`), and never change a type's meaning or gate a `pub` field or trait method. Optional dependencies are `dep:name` features, forwarded with `name?/feature`; a feature that needs another lists it (API-16).
- Never change `test`, `doctest`, `harness`, `autotests` or similar target switches to get green (H-12).
- Never edit `Cargo.lock` by hand. Change it with cargo (`cargo update -p <crate> --precise <version>`), and resolve a lockfile conflict by taking the base version and re-running cargo (PROC-13).
- Respect dependency direction: domain crates never depend on adapters, runtimes or frameworks (ARC-02). Check with `cargo tree --workspace --depth workspace -e normal --target all`.
- `build.rs` is production code (ARC-17). It writes only under `OUT_DIR`, emits `cargo::rerun-if-changed` for every input and `cargo::rustc-check-cfg` for every custom cfg, reads the target from `CARGO_CFG_*` (not `cfg!`), prints directives with `writeln!(io::stdout().lock(), ..)?`, and returns errors from `main`. No network, no timestamps.
