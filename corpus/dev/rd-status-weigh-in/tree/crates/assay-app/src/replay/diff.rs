//! The byte-for-byte comparison of a recorded decision with a recomputed one, and where they
//! first differ.
//!
//! Both are compared in one canonical encoding, compact JSON with object keys sorted (the
//! encoding of a `serde_json` [`Value`]), because the bundle keeps the decision as a nested
//! object rather than as separate bytes.

use std::collections::BTreeSet;

use serde_json::Value;

/// Where a recorded decision first differs from the recomputed one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DecisionDiff {
    /// The offset of the first differing byte in the recorded decision's canonical encoding
    /// (compact JSON, keys sorted). When one encoding is a prefix of the other, it is the
    /// shorter encoding's length.
    pub offset: usize,
    /// The JSON pointer of the first differing field, in key order. Only names the recomputed
    /// decision has are written: a key only the recorded one has is `*`, so nothing from the
    /// bundle is echoed. Empty when the documents differ as a whole.
    pub field: String,
}

/// Where `recorded` first differs from `replayed` in canonical encoding, or `None` when their
/// encodings are equal.
pub(super) fn first_difference(
    recorded: &Value,
    replayed: &Value,
) -> Result<Option<DecisionDiff>, serde_json::Error> {
    let recorded_bytes = serde_json::to_vec(recorded)?;
    let replayed_bytes = serde_json::to_vec(replayed)?;
    let differs_at = recorded_bytes
        .iter()
        .zip(&replayed_bytes)
        .position(|(recorded, replayed)| recorded != replayed)
        .or_else(|| {
            (recorded_bytes.len() != replayed_bytes.len())
                .then(|| recorded_bytes.len().min(replayed_bytes.len()))
        });
    Ok(differs_at.map(|offset| {
        let mut field = String::new();
        locate(recorded, replayed, &mut field);
        DecisionDiff { offset, field }
    }))
}

/// Appends to `field` the pointer of the first place `recorded` and `replayed` differ.
///
/// It descends only where both sides are objects or both are arrays, so its depth is at most
/// the recomputed decision's, which `decide` fixes; each level compares subtrees, so the work
/// is the decision's size times that depth.
fn locate(recorded: &Value, replayed: &Value, field: &mut String) {
    match (recorded, replayed) {
        (Value::Object(recorded), Value::Object(replayed)) => {
            let keys: BTreeSet<&String> = recorded.keys().chain(replayed.keys()).collect();
            for key in keys {
                match (recorded.get(key), replayed.get(key)) {
                    (Some(left), Some(right)) if left == right => {}
                    (Some(left), Some(right)) => {
                        push_key(field, key);
                        locate(left, right, field);
                        return;
                    }
                    (None, Some(_)) => {
                        push_key(field, key);
                        return;
                    }
                    (Some(_), None) => {
                        field.push_str("/*");
                        return;
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(recorded), Value::Array(replayed)) => {
            let first = recorded
                .iter()
                .zip(replayed)
                .position(|(left, right)| left != right);
            match first.and_then(|index| Some((index, recorded.get(index)?, replayed.get(index)?)))
            {
                Some((index, left, right)) => {
                    push_index(field, index);
                    locate(left, right, field);
                }
                None => push_index(field, recorded.len().min(replayed.len())),
            }
        }
        _ => {}
    }
}

/// Appends `/key`, escaped as a JSON pointer token (RFC 6901). `key` is a name of the
/// recomputed decision.
fn push_key(field: &mut String, key: &str) {
    field.push('/');
    for character in key.chars() {
        match character {
            '~' => field.push_str("~0"),
            '/' => field.push_str("~1"),
            other => field.push(other),
        }
    }
}

/// Appends `/index`.
fn push_index(field: &mut String, index: usize) {
    field.push('/');
    field.push_str(&index.to_string());
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DecisionDiff, first_difference};

    #[test]
    fn equal_documents_have_no_difference() {
        let decision = json!({"a": [1, {"b": "c"}]});

        assert_eq!(
            first_difference(&decision, &decision).expect("encodable"),
            None
        );
    }

    #[test]
    fn a_differing_array_element_is_named_by_index() {
        let recorded = json!({"reasons": [{"code": "x"}, {"code": "y"}]});
        let replayed = json!({"reasons": [{"code": "x"}, {"code": "z"}]});

        let diff = first_difference(&recorded, &replayed).expect("encodable");

        // `{"reasons":[{"code":"x"},{"code":"` is 34 bytes.
        assert_eq!(
            diff,
            Some(DecisionDiff {
                offset: 34,
                field: "/reasons/1/code".to_owned(),
            })
        );
    }

    #[test]
    fn a_shorter_recorded_array_is_named_at_its_end() {
        let recorded = json!({"degraded": [1]});
        let replayed = json!({"degraded": [1, 2]});

        let diff = first_difference(&recorded, &replayed).expect("encodable");

        // `{"degraded":[1` is 14 bytes; the recorded array closes where the replayed goes on.
        assert_eq!(
            diff,
            Some(DecisionDiff {
                offset: 14,
                field: "/degraded/1".to_owned(),
            })
        );
    }

    #[test]
    fn a_key_the_recorded_decision_lacks_is_named() {
        let recorded = json!({"a": 1});
        let replayed = json!({"a": 1, "b": 2});

        let diff = first_difference(&recorded, &replayed).expect("encodable");

        assert_eq!(
            diff,
            Some(DecisionDiff {
                offset: 6,
                field: "/b".to_owned(),
            })
        );
    }

    #[test]
    fn values_of_different_kinds_differ_at_their_field() {
        let recorded = json!({"counts": "many"});
        let replayed = json!({"counts": {"major": 1}});

        let diff = first_difference(&recorded, &replayed).expect("encodable");

        // `{"counts":` is 10 bytes.
        assert_eq!(
            diff,
            Some(DecisionDiff {
                offset: 10,
                field: "/counts".to_owned(),
            })
        );
    }

    #[test]
    fn a_key_with_pointer_characters_is_escaped() {
        let recorded = json!({"a/b~c": 1});
        let replayed = json!({"a/b~c": 2});

        let diff = first_difference(&recorded, &replayed).expect("encodable");

        assert_eq!(diff.map(|diff| diff.field), Some("/a~1b~0c".to_owned()));
    }
}
