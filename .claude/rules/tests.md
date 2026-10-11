---
paths:
  - "**/tests/**/*.rs"
  - "**/benches/**/*.rs"
---

# Test rules (rust-craft)

- Red first: every behavior gets a failing test before its implementation, and every bug gets a regression test that fails first (TST-01).
- Integration tests use the public API only and compile as one binary: `tests/it/main.rs` plus modules, with helpers in `tests/it/support.rs`, declared `#[cfg(test)] mod support;` so the test lint relaxations apply (TST-02, TST-14).
- Names state behavior. One behavior per test, laid out Arrange, Act, Assert (TST-03). Tests return `Result` and use `?` (TST-04).
- Assert exact values (`assert_eq!` on whole values) and exact error variants (`matches!`, `assert_matches!`). Never assert `is_ok()` alone. Never restate a constant or mirror the implementation's branches as the expected value: assert a property (TST-05).
- Helper functions outside `#[test]` fns and `#[cfg(test)]` modules give every assertion a message (`missing_assert_message`, H-03).
- Tests are deterministic: no sleeps, no wall clock, no real network, and injected clocks and seeds (TST-10). Use fakes of port traits, not mocks of internals (TST-09).
- Never delete, `#[ignore]` or loosen a test to get green (H-12). Changing an expected value is a human decision.
- A test Miri cannot run (processes, network, FFI) carries `#[cfg_attr(miri, ignore = "<what Miri cannot do>")]`; never hide tests with `cfg(not(miri))` (TST-17).
