# Preflight status fixtures

Outputs of `claude --version` and `claude auth status` for `read_cli_status` and the
`ClaudeHeadless` probe (#83). The `_2_1_296` files were captured from Claude Code 2.1.296 under a
cleared environment (only `HOME`, `PATH` and the credential under test), with their home
directory paths sanitised to `/home/runner`. The others are constructed: each changes one thing a
fail-closed reader must refuse, or is a shape the preflight must read and then reject.

| File | Shape | Read as |
|---|---|---|
| `version_2_1_296.txt` | `2.1.296 (Claude Code)` | version `2.1.296` |
| `version_no_suffix.txt` | the version alone | `StatusReadError::Version` |
| `version_extra_line.txt` | a second line after the version | `StatusReadError::Version` |
| `auth_oauth_2_1_296.json` | `CLAUDE_CODE_OAUTH_TOKEN` set | `oauth_token`, `firstParty`: passes |
| `auth_api_key_2_1_296.json` | `ANTHROPIC_API_KEY` set | `api_key`: the preflight refuses |
| `auth_bedrock_2_1_296.json` | `CLAUDE_CODE_USE_BEDROCK=1` | `third_party`, `bedrock`: refused |
| `auth_logged_out.json` | `loggedIn: false` | read; the preflight refuses |
| `auth_not_json.txt` | prose | `NotJson` |
| `auth_trailing_text.json` | two objects | `NotJson` |
| `auth_not_object.json` | an array | `NotJson` |
| `auth_repeated_key.json` | `authMethod` twice | `RepeatedKey` |
| `auth_missing_method.json` | no `authMethod` | `Field { field: "authMethod" }` |
| `auth_mistyped_logged_in.json` | `loggedIn: "true"` | `Field { field: "loggedIn" }` |
| `auth_control_character.json` | a BEL in `authMethod` | `Name { field: "authMethod" }` |
