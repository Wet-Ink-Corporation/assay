---
id: "kb-decision-wi-7573a0"
title: "Keep the narrow set-aside"
kind: decision
status: accepted
authority_tier: decision
summary: "In the context of \"May the redaction test set aside the rejected contract's own leading name, now that review.event contains the producer string review?\", facing after the rename, every contract's message starts with review.<x>, which contains the job name review, so the no-producer-text test would flag the reader's own label, we decided for Keep the narrow set-aside and neglected Rename the fixture job name, on the premise that producer text cannot hide in the leading label, accepting that if wrong: a leak exactly at the label goes unseen."
depends_on: []
related: []
source_paths: []
last_reviewed: "2026-11-07"
reversibility: high
phase: null
supersedes: []
superseded_by: null
weighin_item: "wi-7573a0"
question: "May the redaction test set aside the rejected contract's own leading name, now that review.event contains the producer string review?"
door: two-way
blast_radius: file
decider: "human:claude"
provenance: user-approved
decided_at: "2026-11-07T19:02:46Z"
tree_hash: "cd46e8ef27acde636bb4560d37e68a8cf2086641"
recommended: "A"
flip_condition: "a message that legitimately quotes a contract name mid-text"
---

# Keep the narrow set-aside

## Context and problem statement

May the redaction test set aside the rejected contract's own leading name, now that review.event contains the producer string review?

After the rename, every contract's message starts with review.<x>, which contains the job name review, so the no-producer-text test would flag the reader's own label.

Raised by an agent (sweep) as a call and captured by Weigh-In as `wi-7573a0`. Anchor: `crates/review-app/tests/it/redaction.rs:132`.

## Decision drivers

- Files touched

## Considered options

### A · Keep the narrow set-aside (chosen)

Strip only the rejected contract's own leading '<name> document' label; check the rest whole.

- Holds if producer text cannot hide in the leading label.
- If wrong: a leak exactly at the label goes unseen.

### B · Rename the fixture job name

Leave the test as on main and change the fixtures' job name away from review.

- Holds if no fixture must say review.
- If wrong: fixture churn across goldens and digests.

## Evidence

- `crates/review-app/tests/it/redaction.rs:140`: strip_prefix(contract.name()).and_then(|rest| rest.strip_prefix(" document")).unwrap_or(&shown)

## Decision outcome

Chosen option: **Keep the narrow set-aside**, the recommended option.

Decider's note: chosen on the decision card 2026-11-07

### Consequences

- Good, because it holds if producer text cannot hide in the leading label.
- Bad, because if wrong: a leak exactly at the label goes unseen.

### Confirmation

Revisit when: a message that legitimately quotes a contract name mid-text

## Why this might be wrong

the reader could someday put producer text before ' document'

## Provenance

- Decided 2026-11-07T19:02:46Z by human:claude (user-approved), via chat.
- Raised in session `b0ba05d6-017b-597e-94b4-24b45ec76c68`.
- Verbatim: "May the redaction test set aside the rejected contract's own leading name, now that review.event contains the producer string review?"
