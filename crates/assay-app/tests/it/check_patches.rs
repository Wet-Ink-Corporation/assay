//! `check_local` reads the change's patches only for a review, with the pack's diff cap as
//! the most text the patches port may hold (H-13); a withheld text is "not shown" while its
//! hunks still place findings.

use std::error::Error;

use assay_app::{
    ChangedFiles, CheckPorts, LocalReport, MAX_DIFF_BYTES, NoEvents, PatchBudget, ReviewStatus,
    check_local,
};
use assay_domain::{FileChange, Lines, Tier};

use crate::check::{lib_text, prompt_with, validator};
use crate::check_fakes::{
    FakeGates, FakeModel, FakePatches, LIB_AT_BASE, Setup, dirty_tree, unwrap_finding,
};
use crate::plan_fakes::{BASE, FakePr, HEAD};

/// Checks `pr` with `patches` and `model`.
fn check_with(
    pr: &FakePr,
    patches: &FakePatches,
    model: &FakeModel,
) -> Result<LocalReport, Box<dyn Error>> {
    let setup = Setup::of(pr)?;
    let ports = CheckPorts {
        objects: pr,
        patches,
        gates: &mut FakeGates::passing(),
        model,
        events: &mut NoEvents,
    };
    Ok(check_local(&setup.request(), ports, &validator()?)?)
}

#[test]
fn a_change_without_an_llm_stage_reads_no_patches() -> Result<(), Box<dyn Error>> {
    let mut pr = dirty_tree();
    pr.write(HEAD, "README.md", b"# changed\n");
    pr.write(HEAD, "src/lib.rs", LIB_AT_BASE.as_bytes());
    pr.write(BASE, "src/lib.rs", LIB_AT_BASE.as_bytes());
    pr.changes = ChangedFiles::Listed(vec![FileChange::new(
        "README.md",
        Lines::Counted {
            added: 1,
            removed: 1,
        },
    )]);
    let patches = FakePatches::lib();

    let report = check_with(&pr, &patches, &FakeModel::finding(&[]))?;

    assert_eq!(report.tier(), Tier::Trivial);
    assert!(
        patches.budgets.borrow().is_empty(),
        "no review, no diff read"
    );
    Ok(())
}

#[test]
fn the_patches_are_read_with_the_diff_cap_as_their_text_budget() -> Result<(), Box<dyn Error>> {
    let pr = dirty_tree();
    let patches = FakePatches::lib();

    check_with(&pr, &patches, &FakeModel::finding(&[]))?;

    assert_eq!(
        patches.budgets.borrow().as_slice(),
        [PatchBudget::new(MAX_DIFF_BYTES)]
    );
    Ok(())
}

#[test]
fn a_withheld_patch_is_not_shown_and_its_hunks_still_place_findings() -> Result<(), Box<dyn Error>>
{
    let pr = dirty_tree();
    let mut patches = FakePatches::lib();
    if let Some(text) = lib_text(&mut patches) {
        text.push_str(&" ".repeat(MAX_DIFF_BYTES));
    }
    let model = FakeModel::finding(&[unwrap_finding()]);

    let prompt = prompt_with(&patches, &pr)?;
    let report = check_with(&pr, &patches, &model)?;

    assert!(
        prompt.contains("[1 more file not shown: the diff stops at 122880 bytes]"),
        "{prompt:?}"
    );
    assert!(!prompt.contains("diff --git a/src/lib.rs"));
    assert_eq!(report.review().status, ReviewStatus::Completed);
    assert_eq!(
        report.review().findings.len(),
        1,
        "the finding on the unshown file is placed by its hunks"
    );
    Ok(())
}
