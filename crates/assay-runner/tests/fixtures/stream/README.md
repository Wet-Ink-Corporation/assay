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
| `garbage_line.jsonl` | `Malformed`: a line that is not JSON |
| `no_structured_output.jsonl` | `Malformed`: a successful `result` without `structured_output` |
| `error_during_execution.jsonl` | `Malformed`: an execution error |
| `partial.jsonl` | the start of a call cut off by its timeout |
