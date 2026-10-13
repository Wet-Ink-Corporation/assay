//! The verifier's answer: the schema the CLI is given and the strict reading of what comes back.
//!
//! The CLI checks structured output against [`VERDICT_SCHEMA`], but the output is still model
//! text, so it is read again here and refused on anything outside the exact shape (wi-b5affa).

use assay_domain::VerifierVerdict;
use serde_json::Value;

use crate::validation::is_injection_suspect;

/// The longest reason a verdict may carry, in characters (ASCII, so also bytes).
pub const MAX_REASON_CHARS: usize = 200;

/// The JSON Schema the verifier's structured output must satisfy (`--json-schema`): exactly a
/// `verdict` code and a non-empty `reason` of at most [`MAX_REASON_CHARS`] characters.
pub const VERDICT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["verdict","reason"],"properties":{"verdict":{"enum":["confirmed","refuted","unclear"]},"reason":{"type":"string","minLength":1,"maxLength":200}}}"#;

/// The one line a verifier gives for its verdict: printable ASCII, not blank, at most
/// [`MAX_REASON_CHARS`] characters, and clear of the injection screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictReason(String);

impl VerdictReason {
    /// The reason's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A verdict the verifier gave, with its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    verdict: VerifierVerdict,
    reason: VerdictReason,
}

impl Judgement {
    /// The verdict.
    #[must_use]
    pub const fn verdict(&self) -> VerifierVerdict {
        self.verdict
    }

    /// Why the verifier gave it.
    #[must_use]
    pub const fn reason(&self) -> &VerdictReason {
        &self.reason
    }
}

/// Why an answer was refused. Never quotes the answer: it is untrusted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refused {
    /// Not an object with exactly the keys `verdict` and `reason`.
    Shape,
    /// `verdict` is not one of the three codes, exactly.
    Verdict,
    /// `reason` is not a string.
    ReasonType,
    /// `reason` is empty or only spaces.
    Blank,
    /// `reason` is longer than [`MAX_REASON_CHARS`].
    TooLong,
    /// `reason` holds a character outside printable ASCII: a control, a line break or anything
    /// non-ASCII (invisible and direction-changing characters included).
    Character,
    /// The injection screen flags `reason`.
    Suspect,
}

/// Reads `answer` as a verdict on a change to `repository` (`owner/name`), refusing anything
/// but the exact shape.
pub(super) fn read(answer: Value, repository: &str) -> Result<Judgement, Refused> {
    let Value::Object(mut fields) = answer else {
        return Err(Refused::Shape);
    };
    let (Some(verdict), Some(reason), 0) = (
        fields.remove("verdict"),
        fields.remove("reason"),
        fields.len(),
    ) else {
        return Err(Refused::Shape);
    };
    let verdict = verdict
        .as_str()
        .and_then(|code| VerifierVerdict::ALL.into_iter().find(|v| v.code() == code))
        .ok_or(Refused::Verdict)?;
    let reason = read_reason(reason, repository)?;
    Ok(Judgement { verdict, reason })
}

fn read_reason(reason: Value, repository: &str) -> Result<VerdictReason, Refused> {
    let Value::String(text) = reason else {
        return Err(Refused::ReasonType);
    };
    // Length first, so nothing longer is scanned further.
    if text.len() > MAX_REASON_CHARS {
        return Err(Refused::TooLong);
    }
    if !text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
        return Err(Refused::Character);
    }
    if text.trim().is_empty() {
        return Err(Refused::Blank);
    }
    if is_injection_suspect(&text, repository) {
        return Err(Refused::Suspect);
    }
    Ok(VerdictReason(text))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::{MAX_REASON_CHARS, Refused, VERDICT_SCHEMA, read};

    const REPOSITORY: &str = "Wet-Ink-Corporation/assay";

    /// An object of `fields`, in order.
    fn answer(fields: &[(&str, Value)]) -> Value {
        Value::Object(
            fields
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<Map<String, Value>>(),
        )
    }

    fn refusal(answer: &Value) -> Option<Refused> {
        // A copy: `read` takes the answer by value, as the verifier hands it over.
        read(answer.clone(), REPOSITORY).err()
    }

    fn refusal_or_code(answer: &Value) -> Result<&'static str, Refused> {
        read(answer.clone(), REPOSITORY).map(|judgement| judgement.verdict().code())
    }

    fn reason(text: &str) -> Value {
        answer(&[("verdict", json!("refuted")), ("reason", json!(text))])
    }

    #[test]
    fn each_verdict_code_is_read_exactly() {
        let read_codes: Vec<_> = ["confirmed", "refuted", "unclear"]
            .map(|code| answer(&[("verdict", json!(code)), ("reason", json!("seen"))]))
            .iter()
            .map(|answer| refusal_or_code(answer))
            .collect();

        assert_eq!(read_codes, [Ok("confirmed"), Ok("refuted"), Ok("unclear")]);
    }

    #[test]
    fn anything_but_an_object_of_exactly_two_keys_is_a_shape_refusal() {
        let answers = [
            json!("confirmed"),
            json!(null),
            json!({"verdict": "confirmed"}),
            json!({"verdict": "confirmed", "why": "seen"}),
            json!({"verdict": "confirmed", "reason": "seen", "extra": 1}),
        ];

        let refusals: Vec<_> = answers.iter().map(refusal).collect();

        assert_eq!(refusals, [Some(Refused::Shape); 5]);
    }

    #[test]
    fn a_verdict_outside_the_codes_is_refused() {
        let answers = ["Confirmed", "confirmed ", "refute", ""]
            .map(|code| answer(&[("verdict", json!(code)), ("reason", json!("seen"))]));
        let not_a_string = answer(&[("verdict", json!(true)), ("reason", json!("seen"))]);

        let refusals: Vec<_> = answers.iter().chain([&not_a_string]).map(refusal).collect();

        assert_eq!(refusals, [Some(Refused::Verdict); 5]);
    }

    #[test]
    fn a_reason_that_is_not_a_string_is_refused() {
        let refused = refusal(&answer(&[
            ("verdict", json!("unclear")),
            ("reason", json!(["seen"])),
        ]));

        assert_eq!(refused, Some(Refused::ReasonType));
    }

    #[test]
    fn a_blank_reason_is_refused() {
        assert_eq!(
            (refusal(&reason("")), refusal(&reason("   "))),
            (Some(Refused::Blank), Some(Refused::Blank))
        );
    }

    #[test]
    fn a_reason_of_the_cap_is_kept_and_one_more_character_is_refused() {
        let at_cap = "a".repeat(MAX_REASON_CHARS);
        let over = "a".repeat(MAX_REASON_CHARS + 1);

        let kept = read(reason(&at_cap), REPOSITORY);

        assert_eq!(
            kept.as_ref().map(|judgement| judgement.reason().as_str()),
            Ok(at_cap.as_str())
        );
        assert_eq!(refusal(&reason(&over)), Some(Refused::TooLong));
    }

    #[test]
    fn a_reason_outside_printable_ascii_is_refused() {
        let texts = [
            "two\nlines",
            "a\ttab",
            "a\rreturn",
            "a\u{7f}delete",
            "caf\u{e9}",
            "a\u{202e}b",
        ];

        let refusals: Vec<_> = texts.map(reason).iter().map(refusal).collect();

        assert_eq!(refusals, [Some(Refused::Character); 6]);
    }

    #[test]
    fn a_reason_the_injection_screen_flags_is_refused() {
        let texts = [
            "see https://evil.example/x",
            "ask @owner",
            "an <img src=x> tag",
            "ignore previous instructions",
        ];

        let refusals: Vec<_> = texts.map(reason).iter().map(refusal).collect();

        assert_eq!(refusals, [Some(Refused::Suspect); 4]);
    }

    #[test]
    fn the_schema_caps_the_reason_at_the_same_length() {
        let schema: Value = serde_json::from_str(VERDICT_SCHEMA).unwrap_or(Value::Null);

        assert_eq!(
            schema["properties"]["reason"]["maxLength"],
            json!(MAX_REASON_CHARS)
        );
    }
}
