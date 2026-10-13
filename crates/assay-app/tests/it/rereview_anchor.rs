//! The deterministic anchor re-check of carried findings (`docs/architecture/04-pipeline.md` §7,
//! #99): evidence still at head moves the finding there; evidence gone leaves it where it was
//! published, needing re-verification (the anchor-gone fixture).

use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    CarriedAnchor, CarriedFinding, CarriedParts, Change, DiffPaths, FileDiff, recheck_anchors,
};
use assay_domain::{Class, Severity};

/// A carried finding published at `line` of `file`, quoting `evidence`.
fn published(file: &str, line: u64, evidence: &str) -> Result<CarriedFinding, Box<dyn Error>> {
    Ok(CarriedFinding::read(
        CarriedParts {
            fp: "00000000000000aa".parse()?,
            file: file.to_owned(),
            line: NonZeroU64::new(line).ok_or("line 0")?,
            class: Class::Logic,
            severity: Severity::Blocker,
            title: "off by one in the loop bound".to_owned(),
            trigger: Some("an empty slice reaches the loop".to_owned()),
            evidence: evidence.to_owned(),
        },
        "Wet-Ink-Corporation/assay",
    )?)
}

/// A change whose head holds `files` and whose diffs are `diffs`.
fn change(files: &[(&str, &str)], diffs: Vec<FileDiff>) -> Change {
    Change {
        head_files: files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        base_files: BTreeMap::new(),
        diffs,
    }
}

const EVIDENCE: &str = "for i in 0..=items.len() {";

/// Where each re-checked finding is: `(anchor, file, line)`.
fn placed(rechecked: &[CarriedFinding]) -> Vec<(CarriedAnchor, &str, u64)> {
    rechecked
        .iter()
        .map(|found| (found.anchor(), found.file(), found.line().get()))
        .collect()
}

#[test]
fn evidence_still_at_head_moves_the_finding_to_it() -> Result<(), Box<dyn Error>> {
    let head = "// a comment\n\n\n\nfn sum(items: &[u8]) {\n    let mut total = 0;\n    for i in 0..=items.len() {\n";
    let change = change(&[("src/lib.rs", head)], Vec::new());

    let rechecked = recheck_anchors(vec![published("src/lib.rs", 3, EVIDENCE)?], &change);

    assert_eq!(
        placed(&rechecked),
        [(CarriedAnchor::AtHead, "src/lib.rs", 7)]
    );
    Ok(())
}

#[test]
fn evidence_gone_at_head_leaves_the_finding_where_it_was_published() -> Result<(), Box<dyn Error>> {
    let head = "fn sum(items: &[u8]) -> u32 {\n    items.iter().map(|&i| u32::from(i)).sum()\n}\n";
    let change = change(&[("src/lib.rs", head)], Vec::new());

    let rechecked = recheck_anchors(vec![published("src/lib.rs", 3, EVIDENCE)?], &change);

    assert_eq!(placed(&rechecked), [(CarriedAnchor::Gone, "src/lib.rs", 3)]);
    Ok(())
}

#[test]
fn a_file_renamed_since_is_followed_to_its_head_path() -> Result<(), Box<dyn Error>> {
    let head = "fn sum(items: &[u8]) {\n    for i in 0..=items.len() {\n";
    let renamed = FileDiff {
        paths: DiffPaths::Renamed {
            base: "src/old.rs".to_owned(),
            head: "src/new.rs".to_owned(),
        },
        hunks: Vec::new(),
    };
    let change = change(&[("src/new.rs", head)], vec![renamed]);

    let rechecked = recheck_anchors(vec![published("src/old.rs", 2, EVIDENCE)?], &change);

    assert_eq!(
        placed(&rechecked),
        [(CarriedAnchor::AtHead, "src/new.rs", 2)]
    );
    Ok(())
}

#[test]
fn a_file_deleted_at_head_leaves_the_anchor_gone() -> Result<(), Box<dyn Error>> {
    let deleted = FileDiff {
        paths: DiffPaths::Deleted {
            base: "src/lib.rs".to_owned(),
        },
        hunks: Vec::new(),
    };
    let mut change = change(&[], vec![deleted]);
    change.base_files.insert(
        "src/lib.rs".to_owned(),
        "fn sum(items: &[u8]) {\n    for i in 0..=items.len() {\n".to_owned(),
    );

    let rechecked = recheck_anchors(vec![published("src/lib.rs", 2, EVIDENCE)?], &change);

    assert_eq!(placed(&rechecked), [(CarriedAnchor::Gone, "src/lib.rs", 2)]);
    Ok(())
}

#[test]
fn of_two_matches_the_one_nearest_the_published_line_wins() -> Result<(), Box<dyn Error>> {
    let head = "for i in 0..=items.len() {\n}\n\n\n\n\n\n\n\nfor i in 0..=items.len() {\n}\n";
    let change = change(&[("src/lib.rs", head)], Vec::new());

    let rechecked = recheck_anchors(
        vec![
            published("src/lib.rs", 3, EVIDENCE)?,
            published("src/lib.rs", 9, EVIDENCE)?,
        ],
        &change,
    );

    assert_eq!(
        placed(&rechecked),
        [
            (CarriedAnchor::AtHead, "src/lib.rs", 1),
            (CarriedAnchor::AtHead, "src/lib.rs", 10),
        ]
    );
    Ok(())
}
