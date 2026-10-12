//! `rubric/v1.toml` holds the data of `docs/architecture/04-pipeline.md` §5 and parses into the
//! domain's `Rubric`.

use std::error::Error;

use assay_domain::Rubric;

use crate::support::rubric_v1;

#[test]
fn rubric_v1_carries_version_1_and_the_thresholds_of_04_section_5() -> Result<(), Box<dyn Error>> {
    let rubric = rubric_v1()?;

    assert_eq!(rubric.version(), "1");
    assert_eq!(rubric.majors_block_threshold().get(), 3, "A-07");
    assert_eq!(rubric.max_requeues(), 3, "03 §5: at most 3 requeues");
    Ok(())
}

#[test]
fn rubric_v1_treats_only_the_micro_reviewer_as_micro() -> Result<(), Box<dyn Error>> {
    let rubric = rubric_v1()?;

    let micro: Vec<&str> = [
        "bugs",
        "security",
        "design",
        "meta",
        "combined",
        "combined-micro",
    ]
    .into_iter()
    .filter(|role| rubric.is_micro_role(role))
    .collect();

    assert_eq!(micro, ["combined-micro"]);
    Ok(())
}

#[test]
fn rubric_v1_waives_only_temper_human_decision_findings() -> Result<(), Box<dyn Error>> {
    let rubric = rubric_v1()?;
    let candidates = [
        ("temper", "raised"),
        ("temper", "exception"),
        ("temper", "gate-config"),
        ("temper", "test-inventory"),
        ("temper", "manifest-switch"),
        ("temper", "clippy::unwrap_used"),
        ("temper", "build"),
        ("temper", "tests"),
        ("temper", "deny"),
        ("custom-lint", "raised"),
    ];

    let waivable: Vec<(&str, &str)> = candidates
        .into_iter()
        .filter(|(gate, rule)| rubric.is_waivable(gate, rule))
        .collect();

    assert_eq!(
        waivable,
        [
            ("temper", "raised"),
            ("temper", "exception"),
            ("temper", "gate-config"),
            ("temper", "test-inventory"),
            ("temper", "manifest-switch"),
        ]
    );
    Ok(())
}

#[test]
fn a_rubric_without_a_version_is_rejected() {
    let text = "version = \"\"\nmajors_block_threshold = 3\nmax_requeues = 3\nmicro_roles = []\n";

    let parsed = toml::from_str::<Rubric>(text).map_err(|e| e.message().to_owned());

    assert_eq!(parsed.err().as_deref(), Some("the rubric version is empty"));
}

#[test]
fn a_rubric_with_an_unknown_key_is_rejected() {
    let text = "version = \"1\"\nmajors_block_threshold = 3\nmax_requeues = 3\nmicro_roles = []\nextra = 1\n";

    let parsed = toml::from_str::<Rubric>(text);

    assert!(
        parsed
            .as_ref()
            .is_err_and(|e| e.message().contains("unknown field `extra`")),
        "{parsed:?}"
    );
}
