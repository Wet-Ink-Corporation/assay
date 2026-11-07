# stream-json fixtures

One `claude -p --output-format stream-json --verbose` transcript per file, one JSON message per
line. They follow the message shapes of Claude Code 2.1.296 (`system/init`, `assistant`, `user`,
`rate_limit_event`, `result` with `subtype` `success`, `error_max_turns`, `error_max_budget_usd`
or `error_during_execution`). They were written from that CLI's message schema, not captured from
a live call; replace them with captured transcripts when the doctor (#84) records some.

| File | Outcome |
|---|---|
| `ok.jsonl` | `Ok`, cost 0.4213, one routine `allowed` rate-limit event |
| `budget_exhausted.jsonl` | `BudgetExhausted`, cost 0.7512 (over the 0.74 cap by one response) |
| `turns_exhausted.jsonl` | `TurnsExhausted`, cost 0.3301 |
| `quota_exhausted.jsonl` | `QuotaExhausted`: `rejected` event, `rate_limit` assistant error, usage-limit result |
| `usage_limit_result_only.jsonl` | `QuotaExhausted` from the `result` message alone |
| `rate_limit_warning.jsonl` | `Ok` with one `allowed_warning` event kept and one `allowed` dropped |
| `no_result.jsonl` | `Malformed`: the stream ends without a `result` |
| `garbage_line.jsonl` | `Malformed`: a line that is not JSON before a successful `result` |
| `garbage_line_budget_exhausted.jsonl` | `BudgetExhausted` at 0.7512: a stray line does not override a final `result` |
| `no_structured_output.jsonl` | `Malformed`: a successful `result` without `structured_output` |
| `error_during_execution.jsonl` | `Malformed`: an execution error |
| `partial.jsonl` | the start of a call cut off by its timeout |
| `verifier_confirmed.jsonl` | `Ok`, cost 0.0123: a blind verifier's `confirmed` verdict (#91) |
| `verifier_refuted.jsonl` | `Ok`, cost 0.0234: a `refuted` verdict |
| `verifier_unclear.jsonl` | `Ok`, cost 0.0345: an `unclear` verdict |
| `verifier_bad_verdict.jsonl` | `Ok`, cost 0.0111: a verdict with a third key, which the verifier refuses |
| `coordinator_grouped.jsonl` | `Ok`, cost 0.0456: a coordinator answer putting findings 1 and 0 in one group (#96) |
| `coordinator_missing.jsonl` | `Ok`, cost 0.0321: a coordinator answer leaving finding 1 out of every group, which the coordinator refuses |

Session-init fixtures (#121) for `read_session_init` and the clean-init rule. `init_2_1_296.jsonl`
is captured from a real Claude Code 2.1.296 call under the 03 §8 profile, with its working
directory and ids sanitised; the others change one thing in it.

| File | Init |
|---|---|
| `init_2_1_296.jsonl` | clean against `../plugins-2.1.296.toml`; answers `ready` |
| `init_extra_builtin.jsonl` | an extra built-in plugin not on the allowlist |
| `init_non_builtin_plugin.jsonl` | a plugin that is not a built-in |
| `init_unlisted_skill.jsonl` | a skill not on the allowlist |
| `init_extra_tool.jsonl` | a tool beyond `Glob`, `Grep`, `Read` |
