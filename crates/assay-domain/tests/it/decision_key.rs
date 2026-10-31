//! The decision key of 02 §5 (D14): `sha256:<hex>` over seven named fields, each name and value
//! length-prefixed, as the evidence key is.

use assay_domain::{DecisionKeyParts, InfraPolicy, decision_key};
use proptest::prelude::*;

const EVIDENCE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const APPROVALS: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const WAIVERS: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const DISMISSALS: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
const DEFERRAL: &str = r#"{"llm":{"status":"completed"},"readiness":"ready","requeues":0}"#;

fn parts() -> DecisionKeyParts<'static> {
    DecisionKeyParts {
        evidence_key: EVIDENCE,
        approvals_digest: APPROVALS,
        waivers_digest: WAIVERS,
        override_state: "null",
        dismissals_digest: DISMISSALS,
        on_infra_failure: InfraPolicy::Block,
        deferral_state: DEFERRAL,
    }
}

/// Computed independently, with Python's `hashlib` over the same seven pairs: for each field
/// its name, then its value, each prefixed by its byte length as a big-endian `u64`.
#[test]
fn the_decision_key_matches_its_independently_computed_golden_digest() {
    assert_eq!(
        decision_key(&parts()).to_string(),
        "sha256:823a5137d94a4019d15491e8755198926bcc0fd50cd5440ca91c08a46052385c"
    );
}

#[test]
fn a_byte_moved_between_two_fields_changes_the_key() {
    let moved = DecisionKeyParts {
        override_state: "nul",
        dismissals_digest: &["l", DISMISSALS].concat(),
        ..parts()
    };

    assert_ne!(decision_key(&moved), decision_key(&parts()));
}

#[test]
fn the_infra_policy_is_part_of_the_key() {
    let warn = DecisionKeyParts {
        on_infra_failure: InfraPolicy::Warn,
        ..parts()
    };

    assert_ne!(decision_key(&warn), decision_key(&parts()));
}

/// The six text fields of `parts`, in key order.
fn texts(parts: &DecisionKeyParts<'_>) -> [String; 6] {
    [
        parts.evidence_key,
        parts.approvals_digest,
        parts.waivers_digest,
        parts.override_state,
        parts.dismissals_digest,
        parts.deferral_state,
    ]
    .map(str::to_owned)
}

/// The key of the six text fields `texts`, in key order, with `policy`.
fn key_of(texts: &[String; 6], policy: InfraPolicy) -> String {
    let [
        evidence,
        approvals,
        waivers,
        override_state,
        dismissals,
        deferral,
    ] = texts;
    decision_key(&DecisionKeyParts {
        evidence_key: evidence,
        approvals_digest: approvals,
        waivers_digest: waivers,
        override_state,
        dismissals_digest: dismissals,
        on_infra_failure: policy,
        deferral_state: deferral,
    })
    .to_string()
}

proptest! {
    #[test]
    fn changing_any_one_text_field_changes_the_key(
        fields in prop::array::uniform6(".{0,24}"),
        pick in 0_usize..6,
        replacement in ".{0,24}",
        warn in any::<bool>(),
    ) {
        let policy = if warn { InfraPolicy::Warn } else { InfraPolicy::Block };
        let before = key_of(&fields, policy);
        let mut changed = fields;
        let field = changed.get_mut(pick).expect("six fields");
        prop_assume!(*field != replacement);
        *field = replacement;

        prop_assert_ne!(key_of(&changed, policy), before);
    }

    #[test]
    fn the_key_is_deterministic(fields in prop::array::uniform6(".{0,24}")) {
        prop_assert_eq!(
            key_of(&fields, InfraPolicy::Warn),
            key_of(&fields, InfraPolicy::Warn)
        );
    }
}

#[test]
fn the_text_fields_helper_reads_every_field_in_key_order() {
    assert_eq!(
        texts(&parts()),
        [EVIDENCE, APPROVALS, WAIVERS, "null", DISMISSALS, DEFERRAL].map(str::to_owned)
    );
    assert_eq!(
        key_of(&texts(&parts()), InfraPolicy::Block),
        decision_key(&parts()).to_string()
    );
}
