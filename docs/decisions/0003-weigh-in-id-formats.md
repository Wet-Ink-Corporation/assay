# 0003 · Weigh-In item id formats (A-11 verified)

- **Status:** accepted
- **Date:** 2026-10-14
- **Issue:** #106
- **Spec:** `docs/architecture/07-decisions-assumptions.md` A-11, `04-pipeline.md` §9

## Context

A-11 assumed, from reviewer reports only, that one Weigh-In item carries the same id in three places: the journal's `item_id`, the decision atom's `weighin_item` and the commit trailer. The CP1 and experimental CP2 adapters (M4) join on that id, so a mismatch would link the wrong items. M4 step 0 is to read `crates/weighin` and record the formats.

## Finding

Read at `Wet-Ink-Corporation/weigh-in` commit `68a6e33f031eed4853bfed532cb2b009d7ad3eea` (workspace version 0.6.2). **A-11 holds:** all three are written from one `weighin_core::ItemId` value through its `Display`/`as_str`, so they cannot differ.

| Where | Format | Source |
|---|---|---|
| Id type | `wi-` then 4 to 16 lowercase hex characters. Minted as the sha256 prefix of `session + "\n" + normalised question`, 6 characters by default (`DEFAULT_HEX_LEN`), 8 (`EXTENDED_HEX_LEN`) when the 6-character id is already taken by a different question. `ItemId::parse` trims whitespace and rejects uppercase hex, a missing prefix and other characters. | `crates/weighin-core/src/id.rs:13-15, 46-83` |
| Journal | JSON lines under `.kb/_ledger/journal/<session>.jsonl`; every event payload has `item_id`, serialised `#[serde(transparent)]` as the bare id string. | `crates/weighin/src/journal.rs:17`, `crates/weighin-core/src/event.rs:249` and the other payloads |
| Decision atom | Markdown at `.kb/_intake/decisions/<id>-<slug>.md`; YAML front matter line `weighin_item: "<id>"`, the value JSON-quoted. | `crates/weighin-core/src/atom.rs:22, 256` |
| Commit trailer | `Weigh-In: <id> decided <option>`, printed by one `Trailer` type for both the decide receipt and `weighin trailer`. | `crates/weighin/src/ops.rs:1276-1289`, `crates/weighin/src/main.rs:912` |

## Consequences for the adapter

- Parse ids with the same rule as `ItemId::parse` (`wi-[0-9a-f]{4,16}`), not a fixed 6 characters: 8-character ids occur after a collision.
- Join the three sources on the exact id string. A trailer or atom whose id does not parse is refused, not matched loosely (wi-b5affa).
- Unchanged from `04 §9`: the journal hook is still not in the plugin (`plugin/hooks/hooks.json` has none at this commit), so local desktop sessions write no journal, and none of these records is signed. Matching ids proves only that the records agree, not who wrote them.
- Re-check this record when the adapter pins a newer weigh-in commit or `D-WI` (#44) lands.

## Note, 2026-10-19 (#109)

Checked against the Weigh-In 0.9.0 CLI (by running it, not by reading its source): after `weighin raise` and then `weighin defer`, `weighin journal` shows the deferral journaled as an `item_deferred` event, and `ts` is whole-second UTC (`2026-10-19T03:12:27Z`). The experimental `weigh-in` adapter (`assay-gates`, #109) relies on both. It refuses a journal with any other event `type`, or with a `ts` not in exactly that form.
