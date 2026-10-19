//! The Weigh-In adapter in experimental mode (04 §9, #109): the head's journals and decision
//! atoms, read in their documented formats, reported as warnings that never fail the gate.

use std::error::Error;
use std::fs;
use std::path::Path;

use assay_app::{BuildJob, GateJob, GateJobOutcome};
use assay_domain::{GateConclusion, Severity};
use assay_gates::{EntryKind, Revision, WeighInAdapter, WeighInError, WeighInSettingsError};
use serde_json::json;

use crate::temper_support::FakeRevisions;
use crate::weigh_in_support::{DECISIONS, Head, JOURNALS, atom, event, lines, raised, triaged};

const NEUTRAL: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Neutral);
const PASS: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);
const JOURNAL: &str = ".kb/_ledger/journal/s1.jsonl";
const T0: &str = "2026-10-11T06:00:00Z";
const T1: &str = "2026-10-11T06:00:01Z";
const T2: &str = "2026-10-11T06:00:02Z";
const BLOCKING: &str = "weigh-in-open-blocking";
const PENDING: &str = "weigh-in-pending-triage";
const UNREADABLE: &str = "weigh-in-unreadable";

/// `(rule, JOURNAL, line, severity)`.
fn at(rule: &str, line: u64, severity: Severity) -> (String, String, u64, Severity) {
    (rule.to_owned(), JOURNAL.to_owned(), line, severity)
}

/// A head whose one journal `s1` holds `events`.
fn head_with(events: &[serde_json::Value]) -> Head {
    let mut head = Head::default();
    head.journal("s1", events);
    head
}

#[test]
fn a_head_without_a_ledger_passes_with_no_findings() -> Result<(), Box<dyn Error>> {
    let head = Head::default();

    let seen = head.seen()?;

    assert_eq!(seen, (PASS, Vec::new()));
    Ok(())
}

#[test]
fn a_journal_whose_items_are_all_settled_passes() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        triaged("wi-aa4c3b", T1, Some("two-way"), Some("judgment")),
        event("item_defaulted", "wi-aa4c3b", T2),
        raised("wi-951c9e", T0, None),
        event("item_dismissed", "wi-951c9e", T1),
        raised("wi-0a1b", T0, Some("merge")),
        event("item_decided", "wi-0a1b", T1),
        event("record_written", "wi-0a1b", T2),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (PASS, Vec::new()));
    Ok(())
}

#[test]
fn an_untriaged_open_item_is_a_pending_triage_warning_at_its_raise() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-0a1b", T0, None),
        event("item_decided", "wi-0a1b", T1),
        raised("wi-aa4c3b", T1, None),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(PENDING, 3, Severity::Minor)]));
    Ok(())
}

#[test]
fn a_triaged_one_way_item_is_open_and_blocking() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        triaged("wi-aa4c3b", T1, Some("one-way"), Some("judgment")),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(BLOCKING, 1, Severity::Major)]));
    Ok(())
}

#[test]
fn a_triaged_item_blocks_on_blocks_authority_or_any_other_value() -> Result<(), Box<dyn Error>> {
    let cases = [
        (Some("merge"), "two-way", "judgment"),
        (None, "two-way", "authority"),
        (None, "unsure", "judgment"),
        (None, "two-way", "Judgment"),
    ];
    for (blocks, door, authority) in cases {
        let head = head_with(&[
            raised("wi-aa4c3b", T0, blocks),
            triaged("wi-aa4c3b", T1, Some(door), Some(authority)),
        ]);

        let seen = head.seen()?;

        assert_eq!(
            seen,
            (NEUTRAL, vec![at(BLOCKING, 1, Severity::Major)]),
            "{blocks:?} {door} {authority}"
        );
    }
    Ok(())
}

#[test]
fn a_triaged_item_with_no_raise_blocks_on_unknown_blocks() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[triaged("wi-aa4c3b", T1, Some("two-way"), Some("judgment"))]);

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(BLOCKING, 1, Severity::Major)]));
    Ok(())
}

#[test]
fn a_raise_without_a_blocks_field_blocks_once_triaged() -> Result<(), Box<dyn Error>> {
    let mut raise = raised("wi-aa4c3b", T0, None);
    if let Some(object) = raise.as_object_mut() {
        object.remove("blocks");
    }
    let head = head_with(&[
        raise,
        triaged("wi-aa4c3b", T1, Some("two-way"), Some("judgment")),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(BLOCKING, 1, Severity::Major)]));
    Ok(())
}

#[test]
fn a_two_way_judgment_item_blocking_nothing_is_not_reported() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        triaged("wi-aa4c3b", T1, Some("two-way"), Some("judgment")),
        event("item_presented", "wi-aa4c3b", T2),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (PASS, Vec::new()));
    Ok(())
}

#[test]
fn a_null_or_absent_triaged_door_or_authority_is_pending_triage() -> Result<(), Box<dyn Error>> {
    let mut absent = triaged("wi-aa4c3b", T1, Some("one-way"), None);
    if let Some(object) = absent.as_object_mut() {
        object.remove("authority");
    }
    for triage in [
        triaged("wi-aa4c3b", T1, None, Some("authority")),
        triaged("wi-aa4c3b", T1, Some("one-way"), None),
        absent,
    ] {
        let head = head_with(&[raised("wi-aa4c3b", T0, None), triage]);

        let seen = head.seen()?;

        assert_eq!(seen, (NEUTRAL, vec![at(PENDING, 1, Severity::Minor)]));
    }
    Ok(())
}

#[test]
fn grounded_presented_and_deferred_items_stay_open() -> Result<(), Box<dyn Error>> {
    for kind in ["item_grounded", "item_presented", "item_deferred"] {
        let head = head_with(&[
            raised("wi-aa4c3b", T0, None),
            triaged("wi-aa4c3b", T1, Some("one-way"), Some("authority")),
            event(kind, "wi-aa4c3b", T2),
        ]);

        let seen = head.seen()?;

        assert_eq!(
            seen,
            (NEUTRAL, vec![at(BLOCKING, 1, Severity::Major)]),
            "{kind}"
        );
    }
    Ok(())
}

#[test]
fn a_deferral_after_a_decision_reopens_the_item() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        event("item_decided", "wi-aa4c3b", T1),
        event("item_deferred", "wi-aa4c3b", T2),
    ]);

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(PENDING, 1, Severity::Minor)]));
    Ok(())
}

#[test]
fn events_are_ordered_by_time_across_journals() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    head.journal("a-later-session", &[event("item_decided", "wi-aa4c3b", T2)]);
    head.journal("s1", &[raised("wi-aa4c3b", T0, None)]);
    head.journal("z-earlier", &[raised("wi-0a1b", T1, None)]);
    head.journal("y-later", &[event("item_dismissed", "wi-0a1b", T0)]);

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![(
                PENDING.to_owned(),
                ".kb/_ledger/journal/z-earlier.jsonl".to_owned(),
                1,
                Severity::Minor
            )]
        ),
        "wi-aa4c3b is decided later elsewhere; wi-0a1b was dismissed before it was raised again"
    );
    Ok(())
}

#[test]
fn an_unknown_event_type_refuses_the_journal_but_not_the_others() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    head.journal(
        "s1",
        &[
            raised("wi-aa4c3b", T0, None),
            event("item_reopened", "wi-aa4c3b", T1),
        ],
    );
    head.journal("s2", &[raised("wi-0a1b", T0, None)]);

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![
                at(UNREADABLE, 2, Severity::Minor),
                (
                    PENDING.to_owned(),
                    ".kb/_ledger/journal/s2.jsonl".to_owned(),
                    1,
                    Severity::Minor
                ),
            ]
        )
    );
    Ok(())
}

#[test]
fn a_journal_not_exactly_in_the_documented_form_is_refused_at_its_line()
-> Result<(), Box<dyn Error>> {
    let good = lines(&[raised("wi-aa4c3b", T0, None)]);
    let mut bad_ts = event("item_decided", "wi-aa4c3b", T1);
    bad_ts["ts"] = json!("2026-10-11T06:00:01.5Z");
    let mut bad_blocks = raised("wi-0a1b", T1, None);
    bad_blocks["blocks"] = json!(true);
    let mut bad_door = triaged("wi-0a1b", T1, None, None);
    bad_door["door"] = json!(["one-way"]);
    let mut no_type = event("item_decided", "wi-aa4c3b", T1);
    if let Some(object) = no_type.as_object_mut() {
        object.remove("type");
    }
    let cases = [
        (format!("{good}\n"), 2),
        (format!("{good}not json\n"), 2),
        (format!("{good}[1]\n"), 2),
        (
            format!("{good}{{\"type\":\"record_written\",\"type\":\"x\"}}\n"),
            2,
        ),
        (lines(&[event("item_decided", "wi-AA4C3B", T1)]), 1),
        (lines(&[event("item_decided", "wi-aa4c3b ", T1)]), 1),
        (format!("{good}{bad_ts}\n"), 2),
        (format!("{good}{bad_blocks}\n"), 2),
        (format!("{good}{bad_door}\n"), 2),
        (format!("{good}{no_type}\n"), 2),
    ];
    for (text, line) in cases {
        let mut head = Head::default();
        head.file(JOURNAL, &text);

        let seen = head.seen()?;

        assert_eq!(
            seen,
            (NEUTRAL, vec![at(UNREADABLE, line, Severity::Minor)]),
            "{text:?}"
        );
    }
    Ok(())
}

#[test]
fn a_journal_ending_without_its_newline_is_refused_whole() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    let text = lines(&[raised("wi-aa4c3b", T0, None)]);
    head.file(JOURNAL, text.trim_end());

    let seen = head.seen()?;

    assert_eq!(seen, (NEUTRAL, vec![at(UNREADABLE, 1, Severity::Minor)]));
    Ok(())
}

#[test]
fn only_regular_jsonl_files_directly_in_the_journal_directory_are_read()
-> Result<(), Box<dyn Error>> {
    let open = lines(&[raised("wi-aa4c3b", T0, None)]);
    let mut head = Head::default();
    head.file(&format!("{JOURNALS}/notes.md"), &open);
    head.file(&format!("{JOURNALS}/old/s0.jsonl"), &open);
    head.file(&format!("{JOURNALS}.jsonl"), &open);
    head.entry(
        &format!("{JOURNALS}/linked.jsonl"),
        EntryKind::Symlink,
        "../s.jsonl",
    );
    head.entry(&format!("{JOURNALS}/sub.jsonl"), EntryKind::Submodule, "");

    let seen = head.seen()?;

    let refused = |name: &str| {
        (
            UNREADABLE.to_owned(),
            format!("{JOURNALS}/{name}"),
            1,
            Severity::Minor,
        )
    };
    assert_eq!(
        seen,
        (NEUTRAL, vec![refused("linked.jsonl"), refused("sub.jsonl")])
    );
    Ok(())
}

#[test]
fn a_journal_directory_that_is_a_file_is_refused() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    head.file(JOURNALS, "");

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![(
                UNREADABLE.to_owned(),
                JOURNALS.to_owned(),
                1,
                Severity::Minor
            )]
        )
    );
    Ok(())
}

#[test]
fn an_atom_not_exactly_in_the_documented_form_is_refused() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    let bad = format!("{DECISIONS}/wi-aa4c3b-ship.md");
    head.file(
        &bad,
        &atom("wi-aa4c3b").replace("\"wi-aa4c3b\"", "wi-aa4c3b"),
    );
    head.file(
        &format!("{DECISIONS}/kb-decision-other.md"),
        "---\ntitle: x\n---\n",
    );
    head.file(&format!("{DECISIONS}/wi-0a1b-ok.md"), &atom("wi-0a1b"));
    head.file(&format!("{DECISIONS}/README.txt"), "not an atom");

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![(UNREADABLE.to_owned(), bad, 1, Severity::Minor)]
        )
    );
    Ok(())
}

#[test]
fn an_open_item_with_an_atom_says_the_atom_proves_nothing() -> Result<(), Box<dyn Error>> {
    let mut head = head_with(&[raised("wi-aa4c3b", T0, None)]);
    head.file(
        &format!("{DECISIONS}/wi-aa4c3b-ship-it.md"),
        &atom("wi-aa4c3b"),
    );

    let (_, findings) = head.findings()?;

    let bodies: Vec<&str> = findings.iter().map(|f| f.body.as_str()).collect();
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies.iter().all(|body| body
            .contains("`.kb/_intake/decisions/wi-aa4c3b-ship-it.md` names it")
            && body.contains("an atom alone proves nothing")),
        "{bodies:?}"
    );
    Ok(())
}

#[test]
fn every_warning_says_g2_is_enforced_by_cp1_until_d_wi() -> Result<(), Box<dyn Error>> {
    let mut head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        raised("wi-0a1b", T0, None),
        triaged("wi-0a1b", T1, Some("one-way"), Some("authority")),
    ]);
    head.journal("s2", &[event("item_reopened", "wi-aa4c3b", T1)]);

    let (_, findings) = head.findings()?;

    assert_eq!(findings.len(), 3);
    for finding in findings {
        assert!(
            finding
                .body
                .contains("G2 is enforced by CP1 (`assay check --local`)")
                && finding.body.contains("D-WI"),
            "{}",
            finding.body
        );
    }
    Ok(())
}

#[test]
fn a_triaged_value_cannot_break_out_of_its_code_span() -> Result<(), Box<dyn Error>> {
    let head = head_with(&[
        raised("wi-aa4c3b", T0, None),
        triaged(
            "wi-aa4c3b",
            T1,
            Some("x`\n[l](https://e)"),
            Some("judgment"),
        ),
    ]);

    let (_, findings) = head.findings()?;

    let texts: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| (f.evidence.as_str(), f.body.as_str()))
        .collect();
    assert_eq!(texts.len(), 1);
    assert!(
        texts.iter().all(|(evidence, body)| {
            evidence.contains("door x???l??https?e?")
                && body.contains("its door is `x???l??https?e?`")
                && !body.contains("https://")
        }),
        "{texts:?}"
    );
    Ok(())
}

#[test]
fn a_ledger_past_the_byte_cap_is_reported_and_not_read() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    let big = "x".repeat(16 << 20);
    head.file(&format!("{DECISIONS}/wi-aa4c3b-big.md"), &big);
    head.journal("s1", &[raised("wi-aa4c3b", T0, None)]);
    head.revisions.failing_read = Some(JOURNAL.to_owned());

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![(UNREADABLE.to_owned(), ".kb".to_owned(), 1, Severity::Minor)]
        ),
        "nothing is read: a read would fail"
    );
    Ok(())
}

#[test]
fn a_ledger_at_the_byte_cap_is_read() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    let journal = lines(&[raised("wi-aa4c3b", T0, None)]);
    let padding = (16 << 20) - journal.len();
    head.file(JOURNAL, &journal);
    head.file(
        &format!("{DECISIONS}/pad.md"),
        &format!("---\n{}", "x".repeat(padding - 4)),
    );

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![
                (
                    UNREADABLE.to_owned(),
                    format!("{DECISIONS}/pad.md"),
                    1,
                    Severity::Minor
                ),
                at(PENDING, 1, Severity::Minor),
            ]
        ),
        "the padding atom never closes its front matter"
    );
    Ok(())
}

#[test]
fn a_ledger_past_the_file_cap_is_reported_and_not_read() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    for index in 0..=10_000 {
        head.file(&format!("{JOURNALS}/s{index}.jsonl"), "");
    }

    let seen = head.seen()?;

    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![(UNREADABLE.to_owned(), ".kb".to_owned(), 1, Severity::Minor)]
        )
    );
    Ok(())
}

#[test]
fn more_than_a_thousand_findings_end_with_one_truncation_note() -> Result<(), Box<dyn Error>> {
    let events: Vec<_> = (0..1_005)
        .map(|index| raised(&format!("wi-{index:04x}"), T0, None))
        .collect();
    let head = head_with(&events);

    let (outcome, findings) = head.findings()?;

    let rules: Vec<&str> = findings.iter().filter_map(|f| f.rule.as_deref()).collect();
    assert_eq!(outcome, NEUTRAL);
    assert_eq!(rules.len(), 1_001);
    assert_eq!(rules.iter().filter(|rule| **rule == PENDING).count(), 1_000);
    assert_eq!(rules.last(), Some(&"weigh-in-truncated"));
    assert!(
        findings
            .last()
            .is_some_and(|last| last.body.starts_with("5 more Weigh-In findings")),
        "{:?}",
        findings.last()
    );
    Ok(())
}

#[test]
fn refuses_a_required_weigh_in_plugin_before_reading() -> Result<(), Box<dyn Error>> {
    let mut head = Head::default();
    head.revisions.failing = Some(JOURNALS.to_owned());

    let ran = head.run(true, "weigh-in", GateJob::Static)?;

    assert!(
        matches!(ran, Err(WeighInError::Required { ref plugin }) if plugin == "weigh-in"),
        "{ran:?}"
    );
    Ok(())
}

#[test]
fn refuses_another_adapter_and_any_job_but_gates_static() -> Result<(), Box<dyn Error>> {
    let head = Head::default();

    let temper = head.run(false, "temper", GateJob::Static)?;
    let lint = head.run(false, "weigh-in", GateJob::Build(BuildJob::Lint))?;
    let test = head.run(false, "weigh-in", GateJob::Build(BuildJob::Test))?;

    assert!(
        matches!(temper, Err(WeighInError::NotWeighIn { ref plugin }) if plugin == "temper"),
        "{temper:?}"
    );
    assert!(
        matches!(
            lint,
            Err(WeighInError::OutsideStatic {
                job: GateJob::Build(BuildJob::Lint)
            })
        ),
        "{lint:?}"
    );
    assert!(
        matches!(
            test,
            Err(WeighInError::OutsideStatic {
                job: GateJob::Build(BuildJob::Test)
            })
        ),
        "{test:?}"
    );
    Ok(())
}

#[test]
fn a_listing_or_read_failure_is_an_error_never_a_pass() -> Result<(), Box<dyn Error>> {
    let mut listing = Head::default();
    listing.revisions.failing = Some(DECISIONS.to_owned());
    let mut reading = head_with(&[raised("wi-aa4c3b", T0, None)]);
    reading.revisions.failing_read = Some(JOURNAL.to_owned());

    let listed = listing.run(false, "weigh-in", GateJob::Static)?;
    let read = reading.run(false, "weigh-in", GateJob::Static)?;

    assert!(
        matches!(listed, Err(WeighInError::Revision { path, .. }) if path == DECISIONS),
        "{listed:?}"
    );
    assert!(
        matches!(read, Err(WeighInError::Revision { path: ".kb", .. })),
        "{read:?}"
    );
    Ok(())
}

#[test]
fn a_reader_returning_fewer_blobs_is_an_error() -> Result<(), Box<dyn Error>> {
    let mut head = head_with(&[raised("wi-aa4c3b", T0, None)]);
    head.revisions.short_read = true;

    let ran = head.run(false, "weigh-in", GateJob::Static)?;

    assert!(
        matches!(ran, Err(WeighInError::BlobCount { asked: 1, read: 0 })),
        "{ran:?}"
    );
    Ok(())
}

#[test]
fn refuses_an_empty_run_key() {
    let revisions = FakeRevisions::default();

    let adapter = WeighInAdapter::new(&revisions, "");

    assert!(
        matches!(adapter, Err(WeighInSettingsError::EmptyRunKey)),
        "{adapter:?}"
    );
}

#[test]
fn reads_this_repositorys_own_ledger_without_refusing_a_file() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut head = Head::default();
    for dir in [JOURNALS, DECISIONS] {
        for entry in fs::read_dir(root.join(dir))? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_name| "a UTF-8 name")?;
            let bytes = fs::read(entry.path())?;
            let path = format!("{dir}/{name}");
            head.revisions
                .insert((Revision::Head, &path), EntryKind::File, &bytes);
        }
    }

    let (_, findings) = head.findings()?;

    let refused: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule.as_deref() == Some(UNREADABLE))
        .map(|f| f.file.as_str())
        .collect();
    assert_eq!(refused, Vec::<&str>::new());
    Ok(())
}
