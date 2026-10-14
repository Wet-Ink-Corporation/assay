//! The carried findings' intake (`docs/architecture/04-pipeline.md` §7, #99): findings open from
//! earlier runs are model-written text, so every field is checked, the set is capped and a
//! fingerprint may appear once; anything else is refused, never repaired (wi-b5affa).

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    CarriedAnchor, CarriedField, CarriedFinding, CarriedParts, CarriedProblem, CarriedRefusal,
    CarryRefused, MAX_CARRIED, MAX_CARRIED_PATH_BYTES, read_carried,
};
use assay_domain::{Class, Fingerprint, Severity, VerifierVerdict};

/// The repository under review.
const REPOSITORY: &str = "Wet-Ink-Corporation/assay";

/// The fingerprint written as `n` in 16 hex digits.
fn fp(n: usize) -> Result<Fingerprint, Box<dyn Error>> {
    Ok(format!("{n:016x}").parse()?)
}

/// A carried finding that is read as is, fingerprinted `n`.
fn parts(n: usize) -> Result<CarriedParts, Box<dyn Error>> {
    Ok(CarriedParts {
        fp: fp(n)?,
        file: "src/lib.rs".to_owned(),
        line: NonZeroU64::new(12).ok_or("12 is not zero")?,
        end_line: None,
        class: Class::ErrorHandling,
        severity: Severity::Major,
        verdict: None,
        title: "unwrap on user input panics".to_owned(),
        trigger: Some("a non-numeric input reaches parse".to_owned()),
        evidence: "let x = input.parse().unwrap();".to_owned(),
    })
}

/// Why the one finding `edit` turns `parts(0)` into was refused.
fn refusal(edit: impl FnOnce(&mut CarriedParts)) -> Result<CarriedRefusal, Box<dyn Error>> {
    let mut part = parts(0)?;
    edit(&mut part);
    match CarriedFinding::read(part, REPOSITORY) {
        Ok(_) => Err("the edited finding was read".into()),
        Err(refusal) => Ok(refusal),
    }
}

/// Whether the one finding `edit` turns `parts(0)` into is read.
fn reads(edit: impl FnOnce(&mut CarriedParts)) -> Result<bool, Box<dyn Error>> {
    let mut part = parts(0)?;
    edit(&mut part);
    Ok(CarriedFinding::read(part, REPOSITORY).is_ok())
}

const fn refused(field: CarriedField, problem: CarriedProblem) -> CarriedRefusal {
    CarriedRefusal { field, problem }
}

#[test]
fn a_well_formed_finding_is_read_with_its_fields_and_its_anchor_unchecked()
-> Result<(), Box<dyn Error>> {
    let read = CarriedFinding::read(parts(7)?, REPOSITORY)?;

    assert_eq!(
        (
            read.fp(),
            read.file(),
            read.line().get(),
            read.class(),
            read.severity(),
            read.title(),
            read.trigger(),
            read.evidence(),
            read.anchor(),
        ),
        (
            fp(7)?,
            "src/lib.rs",
            12,
            Class::ErrorHandling,
            Severity::Major,
            "unwrap on user input panics",
            Some("a non-numeric input reaches parse"),
            "let x = input.parse().unwrap();",
            CarriedAnchor::Unchecked,
        )
    );
    Ok(())
}

#[test]
fn a_finding_without_a_trigger_is_read() -> Result<(), Box<dyn Error>> {
    let read = reads(|part| part.trigger = None)?;

    assert!(read);
    Ok(())
}

#[test]
fn an_empty_path_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.file = String::new())?;

    assert_eq!(refusal, refused(CarriedField::File, CarriedProblem::Empty));
    Ok(())
}

#[test]
fn an_absolute_path_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.file = "/etc/passwd".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::File, CarriedProblem::Absolute)
    );
    Ok(())
}

#[test]
fn a_path_climbing_out_with_dot_dot_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.file = "src/../../x.rs".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::File, CarriedProblem::Segment)
    );
    Ok(())
}

#[test]
fn a_path_with_an_empty_or_dot_segment_is_refused() -> Result<(), Box<dyn Error>> {
    let empty = refusal(|part| part.file = "src//x.rs".to_owned())?;
    let dot = refusal(|part| part.file = "./src/x.rs".to_owned())?;

    let segment = refused(CarriedField::File, CarriedProblem::Segment);
    assert_eq!((empty, dot), (segment, segment));
    Ok(())
}

#[test]
fn a_path_with_a_control_character_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.file = "src/x\n.rs".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::File, CarriedProblem::Control)
    );
    Ok(())
}

#[test]
fn a_path_at_the_byte_cap_is_read_and_one_byte_more_is_refused() -> Result<(), Box<dyn Error>> {
    let at_cap = reads(|part| part.file = "a".repeat(MAX_CARRIED_PATH_BYTES))?;
    let over = refusal(|part| part.file = "a".repeat(MAX_CARRIED_PATH_BYTES + 1))?;

    assert!(at_cap);
    assert_eq!(over, refused(CarriedField::File, CarriedProblem::TooLong));
    Ok(())
}

#[test]
fn a_blank_title_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.title = " \t ".to_owned())?;

    assert_eq!(refusal, refused(CarriedField::Title, CarriedProblem::Empty));
    Ok(())
}

#[test]
fn a_title_with_a_control_character_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.title = "unwrap\u{7} panics".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::Title, CarriedProblem::Control)
    );
    Ok(())
}

#[test]
fn a_title_with_an_invisible_character_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.title = "unwrap panics\u{202e}".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::Title, CarriedProblem::Control)
    );
    Ok(())
}

#[test]
fn a_title_the_injection_screen_flags_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.title = "see https://evil.example/x".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::Title, CarriedProblem::Injection)
    );
    Ok(())
}

#[test]
fn a_title_at_100_characters_is_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    // Multi-byte characters: the cap counts characters, as findings-1 does.
    let at_cap = reads(|part| part.title = "é".repeat(100))?;
    let over = refusal(|part| part.title = "é".repeat(101))?;

    assert!(at_cap);
    assert_eq!(over, refused(CarriedField::Title, CarriedProblem::TooLong));
    Ok(())
}

#[test]
fn a_blank_trigger_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.trigger = Some(String::new()))?;

    assert_eq!(
        refusal,
        refused(CarriedField::Trigger, CarriedProblem::Empty)
    );
    Ok(())
}

#[test]
fn a_trigger_with_a_control_character_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.trigger = Some("an input\rreaches parse".to_owned()))?;

    assert_eq!(
        refusal,
        refused(CarriedField::Trigger, CarriedProblem::Control)
    );
    Ok(())
}

#[test]
fn a_trigger_the_injection_screen_flags_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| {
        part.trigger = Some("[click](https://evil.example/a) reaches parse".to_owned());
    })?;

    assert_eq!(
        refusal,
        refused(CarriedField::Trigger, CarriedProblem::Injection)
    );
    Ok(())
}

#[test]
fn a_trigger_at_300_characters_is_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let at_cap = reads(|part| part.trigger = Some("é".repeat(300)))?;
    let over = refusal(|part| part.trigger = Some("é".repeat(301)))?;

    assert!(at_cap);
    assert_eq!(
        over,
        refused(CarriedField::Trigger, CarriedProblem::TooLong)
    );
    Ok(())
}

#[test]
fn evidence_with_too_little_content_to_search_for_is_refused() -> Result<(), Box<dyn Error>> {
    let refusal = refusal(|part| part.evidence = "x();  ".to_owned())?;

    assert_eq!(
        refusal,
        refused(CarriedField::Evidence, CarriedProblem::NoContent)
    );
    Ok(())
}

#[test]
fn evidence_at_400_characters_is_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let at_cap = reads(|part| part.evidence = "é".repeat(400))?;
    let over = refusal(|part| part.evidence = "é".repeat(401))?;

    assert!(at_cap);
    assert_eq!(
        over,
        refused(CarriedField::Evidence, CarriedProblem::TooLong)
    );
    Ok(())
}

#[test]
fn a_set_at_the_cap_is_read_in_order() -> Result<(), Box<dyn Error>> {
    let set = (0..MAX_CARRIED).map(parts).collect::<Result<Vec<_>, _>>()?;

    let read = read_carried(set, REPOSITORY)?;

    let fps: Vec<Fingerprint> = read.iter().map(CarriedFinding::fp).collect();
    let expected = (0..MAX_CARRIED).map(fp).collect::<Result<Vec<_>, _>>()?;
    assert_eq!(fps, expected);
    Ok(())
}

#[test]
fn a_set_one_over_the_cap_is_refused_whole() -> Result<(), Box<dyn Error>> {
    let set = (0..=MAX_CARRIED)
        .map(parts)
        .collect::<Result<Vec<_>, _>>()?;

    let refused = read_carried(set, REPOSITORY);

    assert_eq!(
        refused.err(),
        Some(CarryRefused::TooMany {
            count: MAX_CARRIED + 1
        })
    );
    Ok(())
}

#[test]
fn one_refused_finding_refuses_the_set_and_names_its_position() -> Result<(), Box<dyn Error>> {
    let mut bad = parts(1)?;
    bad.title = String::new();
    let set = vec![parts(0)?, bad, parts(2)?];

    let refused = read_carried(set, REPOSITORY);

    assert_eq!(
        refused.err(),
        Some(CarryRefused::Finding {
            index: 1,
            refusal: CarriedRefusal {
                field: CarriedField::Title,
                problem: CarriedProblem::Empty,
            },
        })
    );
    Ok(())
}

#[test]
fn a_fingerprint_given_twice_refuses_the_set_at_the_repeat() -> Result<(), Box<dyn Error>> {
    let set = vec![parts(0)?, parts(1)?, parts(0)?];

    let refused = read_carried(set, REPOSITORY);

    assert_eq!(refused.err(), Some(CarryRefused::Repeated { index: 2 }));
    Ok(())
}

/// #224: a carried range keeps its last line and its last verdict.
#[test]
fn a_finding_is_read_with_its_line_range_and_last_verdict() -> Result<(), Box<dyn Error>> {
    let mut part = parts(0)?;
    part.end_line = NonZeroU64::new(14);
    part.verdict = Some(VerifierVerdict::Unclear);

    let read = CarriedFinding::read(part, REPOSITORY)?;

    assert_eq!(
        (read.end_line().map(NonZeroU64::get), read.verdict()),
        (Some(14), Some(VerifierVerdict::Unclear))
    );
    Ok(())
}

#[test]
fn a_range_ending_before_its_line_is_refused() -> Result<(), Box<dyn Error>> {
    let refused = refusal(|part| part.end_line = NonZeroU64::new(11))?;

    assert_eq!(
        refused,
        CarriedRefusal {
            field: CarriedField::EndLine,
            problem: CarriedProblem::BeforeLine,
        }
    );
    assert!(
        reads(|part| part.end_line = NonZeroU64::new(12))?,
        "a range of one line is read"
    );
    Ok(())
}

#[test]
fn a_refusal_shows_no_carried_text() -> Result<(), Box<dyn Error>> {
    let mut bad = parts(0)?;
    bad.title = "SECRET-TITLE \u{7}".to_owned();

    let refused = read_carried(vec![bad], REPOSITORY)
        .err()
        .ok_or("a control character was read")?;

    let shown = format!("{refused} {refused:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    Ok(())
}
