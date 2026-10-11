@AGENTS.md

# Claude Code specifics

- The temper plugin enforces the rust-craft standard in this repository. It refuses Rust edits until the `temper:rust-craft` skill is loaded, refuses escape hatches before they are written, routes configuration changes, new dependencies, lint exceptions and removed tests to you for approval, and will not let a turn that changed Rust end until the gate is green and `temper:rust-reviewer` has approved the current tree. A turn can also end honestly with `Status: WIP` or `Status: BLOCKED`.
- Entry points: `/temper:implement <task>` (plan, then test-first implementation, then independent review), `/temper:review`, `/temper:gate`, `/temper:audit`, `/temper:setup`.
- Subagents: `temper:rust-architect` plans, `temper:rust-implementer` implements test-first, and `temper:rust-reviewer` gives an adversarial fresh-context review with a validated verdict. Each starts with the standard preloaded.
- Each crate's `CLAUDE.md` imports its `AGENTS.md` charter, so the charter loads when you read files in that crate.
