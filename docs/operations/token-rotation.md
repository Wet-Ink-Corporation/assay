# Rotating the Claude OAuth token

assay's LLM stage authenticates with one OAuth token, created with `claude setup-token` and held
as the `CLAUDE_CODE_OAUTH_TOKEN` secret of the `assay-review` environment in each consumer
repository (`docs/architecture/05-security.md` §5, `06-operations.md` §1). Only the `review` job
and the monthly canary receive it. This runbook replaces that token. The preflight's auth
failures and `assay doctor`'s expiry warning link here (06 F4).

## When to rotate

- **`assay doctor` warns** `the OAuth token expires on <date>, in <n> days`. It warns from 30
  days before the recorded expiry (05 §5). Rotate before the date, not on it.
- **`assay doctor` fails** with `FAIL token-expiry: the OAuth token expired on <date>`. The
  token is past its recorded expiry; the LLM stage is unavailable until you rotate.
- **The preflight fails** with `the CLI is not logged in` or `the CLI authenticates with "<x>",
  not oauth_token`, followed by `rotate the OAuth token:` and a link to this page. Every PR's LLM
  review is deferred as `infra-llm-unavailable` until it passes (rubric rule 8a). The monthly
  canary runs the same preflight, so it fails loudly here first.
- **An egress-scan hit** (05 T2 and T8: the token or an encoding of it in a model's output or an
  uploaded artifact) fails the job and flags rotation. Treat the token as disclosed: rotate at
  once, before reading further into the cause.

An expired or revoked token can also pass `claude auth status` and fail only on the first model
call. Whether the pinned CLI checks the token in `auth status` is not recorded, so do not rely on it.
The recorded expiry and the monthly canary are the early warnings.

## Before you start

- You need the Claude subscription account the token belongs to, and admin access to every
  consumer repository (environment secrets and variables).
- Keep the token out of every agent-reachable place: no issue, PR, comment, commit, chat or
  shell history, and never in an agent session (`05` T2, P10). Do this from your own terminal.

## Rotate

1. **Create the new token.** On your own machine, signed in to the subscription account, run
   `claude setup-token` and follow its prompts. It prints the new token once. Note the date it
   expires; this is the date step 3 records.
2. **Replace the secret** in each consumer repository's `assay-review` environment. Paste the
   value at the prompt rather than on the command line, so it stays out of your shell history:

   ```sh
   gh secret set CLAUDE_CODE_OAUTH_TOKEN --env assay-review --repo <owner>/<repo>
   ```

   Or in the web UI: Settings, Environments, `assay-review`, Environment secrets,
   `CLAUDE_CODE_OAUTH_TOKEN`, Update. Do the same for the environment the monthly canary
   workflow uses, if it is a different one.
3. **Record the new expiry** as the `ASSAY_TOKEN_EXPIRES_ON` variable of the same environment, as
   `YYYY-MM-DD` (a UTC date). It is a variable, not a secret, so doctor can read it:

   ```sh
   gh variable set ASSAY_TOKEN_EXPIRES_ON --env assay-review --repo <owner>/<repo> --body 2027-10-13
   ```

   doctor treats the token as expired from that date on. Anything other than exactly
   `YYYY-MM-DD` is refused, and the value is never echoed back.
4. **Verify.** Run the monthly canary workflow by hand (`workflow_dispatch` on the default
   branch). Its preflight must pass, and `assay doctor` must print no `token-expiry` line. A PR
   deferred as `infra-llm-unavailable` is reviewed on its next run (a requeue or a new push,
   06 §1).
5. **Retire the old token** wherever the account lists its tokens, after step 4 passes. If it was
   disclosed (an egress-scan hit), retire it first and accept the outage until step 4.

Never set `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` or a provider variable to get the LLM stage
back sooner. An API key outranks the OAuth token and bills the API, and the preflight refuses
any of them (06 §1).

## The expiry date

The expiry is recorded when the token is created (05 §5) as `ASSAY_TOKEN_EXPIRES_ON` in the
`assay-review` environment, next to the secret it describes, so one rotation updates both.
`assay doctor` reads it from its environment: the job running doctor passes
`ASSAY_TOKEN_EXPIRES_ON: ${{ vars.ASSAY_TOKEN_EXPIRES_ON }}`. Unset or empty, doctor prints no
expiry line. With 31 or more days left it prints nothing, from 30 days to 1 day it prints
`warn token-expiry: …` and still exits 0, and from the date on it prints `FAIL token-expiry: …`
and exits 1.
