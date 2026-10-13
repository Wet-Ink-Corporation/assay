//! The free-text egress scan (03 §3, 05 T2 and T8): a finding holding the OAuth token, an
//! encoding of it, or a long run found in neither the tree nor base `REVIEW.md` is dropped as
//! `injection-suspect` and raises the egress alarm.

use std::error::Error;

use assay_app::{
    EgressHit, EgressKind, EgressScan, EgressToken, FindingRef, FreeTextField, KnownRuns, Validated,
};
use assay_domain::Disposition;
use serde_json::{Value, json};

use super::fixture::{Run, document, finding, finding_with};

/// A fake OAuth token: its `~`, `?` and `>` make its base64 hold `+` and `/` (`-` and `_` in
/// the URL-safe alphabet).
const TOKEN: &str = "sk-ant-oat01-Zq3Lw9_Xv2Rt~~~Hp8NcYb4?MdKs7Ue1Ja6Gf0Ti5Wo>>?";

/// `TOKEN` behind 0, 1 and 2 bytes of other text, base64-encoded and cut into groups of 16 so
/// no run reaches 20 characters: only the token windows can catch them.
const BASE64_AT_EACH_ALIGNMENT: [&str; 3] = [
    "c2stYW50LW9hdDAx LVpxM0x3OV9YdjJS dH5+fkhwOE5jWWI0 P01kS3M3VWUxSmE2 R2YwVGk1V28+Pj8h",
    "YXNrLWFudC1vYXQw MS1acTNMdzlfWHYy UnR+fn5IcDhOY1li ND9NZEtzN1VlMUph NkdmMFRpNVdvPj4/ IQ==",
    "YWJzay1hbnQtb2F0 MDEtWnEzTHc5X1h2 MlJ0fn5+SHA4TmNZ YjQ/TWRLczdVZTFK YTZHZjBUaTVXbz4+ PyE=",
];

/// `TOKEN` behind one byte, in the URL-safe alphabet, cut as above.
const BASE64_URL_SAFE: &str =
    "YXNrLWFudC1vYXQw MS1acTNMdzlfWHYy UnR-fn5IcDhOY1li ND9NZEtzN1VlMUph NkdmMFRpNVdvPj4_";

/// `TOKEN` in lowercase and uppercase hex, cut as above.
const HEX: [&str; 2] = [
    "736b2d616e742d6f 617430312d5a7133 4c77395f58763252 747e7e7e4870384e 635962343f4d644b",
    "736B2D616E742D6F 617430312D5A7133 4C77395F58763252 747E7E7E4870384E 635962343F4D644B",
];

fn token() -> Result<EgressToken, Box<dyn Error>> {
    Ok(EgressToken::new(TOKEN)?)
}

/// Validates `findings` in one document, holding the token as Z3 does.
fn z3(findings: &[Value]) -> Result<Validated, Box<dyn Error>> {
    let run = Run {
        token: Some(token()?),
        ..Run::default()
    };
    Ok(run.validate(&[document(findings)])?)
}

/// Validates `findings` in one document without the token, as Z4 does.
fn z4(findings: &[Value]) -> Result<Validated, Box<dyn Error>> {
    Ok(Run::default().validate(&[document(findings)])?)
}

/// The egress kind of a finding whose body is `body`, in Z3.
fn kind_of(body: &str) -> Result<Option<EgressKind>, Box<dyn Error>> {
    let validated = z3(&[finding_with(json!({ "body": body }))])?;
    Ok(validated.egress.first().map(|hit| hit.kind))
}

const FIRST: FindingRef = FindingRef::Finding {
    document: 0,
    index: 0,
};

#[test]
fn drops_a_finding_holding_the_plain_token_and_raises_the_alarm() -> Result<(), Box<dyn Error>> {
    let leaking = finding_with(json!({ "body": format!("The key is {TOKEN} here.") }));

    let validated = z3(&[leaking, finding()])?;

    let codes: Vec<_> = validated.transitions.iter().map(|t| t.code).collect();
    assert_eq!(
        (
            codes,
            validated.findings.len(),
            validated.raises_egress_alarm(),
            validated.egress
        ),
        (
            vec![Disposition::InjectionSuspect],
            1,
            true,
            vec![EgressHit {
                at: FIRST,
                field: FreeTextField::Body,
                kind: EgressKind::Token,
            }]
        )
    );
    Ok(())
}

#[test]
fn raises_no_alarm_for_clean_findings() -> Result<(), Box<dyn Error>> {
    let validated = z3(&[finding()])?;

    assert_eq!(
        (validated.egress, validated.findings.len()),
        (Vec::new(), 1)
    );
    Ok(())
}

#[test]
fn catches_the_token_respelled_with_other_separators_and_case() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of("SK ANT OAT01 zQ3lW9.xV2rT")?,
        Some(EgressKind::Token)
    );
    Ok(())
}

#[test]
fn catches_twelve_normalised_characters_of_the_token_but_not_eleven() -> Result<(), Box<dyn Error>>
{
    // `Zq3Lw9_Xv2Rt` normalises to 11 characters; the token's next one is `h`.
    let eleven = "see Zq3Lw9_Xv2Rt.";
    let twelve = "see Zq3Lw9_Xv2Rt~~~H.";

    assert_eq!(
        (kind_of(eleven)?, kind_of(twelve)?),
        (None, Some(EgressKind::Token))
    );
    Ok(())
}

#[test]
fn catches_base64_of_the_token_at_each_alignment_and_in_either_alphabet()
-> Result<(), Box<dyn Error>> {
    let mut encodings = BASE64_AT_EACH_ALIGNMENT.to_vec();
    encodings.push(BASE64_URL_SAFE);

    let kinds = encodings
        .into_iter()
        .map(kind_of)
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(kinds, vec![Some(EgressKind::Token); 4]);
    Ok(())
}

#[test]
fn catches_hex_of_the_token_in_either_case() -> Result<(), Box<dyn Error>> {
    let kinds = HEX
        .into_iter()
        .map(kind_of)
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(kinds, vec![Some(EgressKind::Token); 2]);
    Ok(())
}

#[test]
fn scans_every_field_the_model_wrote() -> Result<(), Box<dyn Error>> {
    let fields = [
        ("title", FreeTextField::Title),
        ("body", FreeTextField::Body),
        ("fix", FreeTextField::Fix),
        ("trigger", FreeTextField::Trigger),
        ("evidence", FreeTextField::Evidence),
        ("expected_behavior", FreeTextField::ExpectedBehavior),
        ("group_hint", FreeTextField::GroupHint),
        ("rule", FreeTextField::Rule),
    ];
    let mut leaking: Vec<_> = fields
        .iter()
        .map(|(field, _)| finding_with(json!({ *field: TOKEN })))
        .collect();
    leaking.push(finding_with(json!({
        "introduced_by": "interaction",
        "caller": { "file": "src/lib.rs", "line": 2, "evidence": TOKEN },
    })));

    let validated = z3(&leaking)?;

    let scanned: Vec<_> = validated.egress.iter().map(|hit| hit.field).collect();
    let mut expected: Vec<_> = fields.iter().map(|(_, field)| *field).collect();
    expected.push(FreeTextField::CallerEvidence);
    assert_eq!(scanned, expected);
    Ok(())
}

#[test]
fn scans_a_finding_before_its_evidence_is_located() -> Result<(), Box<dyn Error>> {
    let unlocated = finding_with(json!({ "evidence": format!("absent_function({TOKEN});") }));

    let validated = z3(&[unlocated])?;

    let codes: Vec<_> = validated.transitions.iter().map(|t| t.code).collect();
    assert_eq!(
        (codes, validated.raises_egress_alarm()),
        (vec![Disposition::InjectionSuspect], true)
    );
    Ok(())
}

#[test]
fn scans_findings_past_the_documents_cap() -> Result<(), Box<dyn Error>> {
    let mut findings = vec![finding(); 256];
    findings.push(finding_with(json!({ "body": TOKEN })));

    let validated = z3(&findings)?;

    assert_eq!(
        (
            validated.transitions.last().map(|t| (t.at, t.code)),
            validated.egress.first().map(|hit| hit.at)
        ),
        (
            Some((
                FindingRef::Finding {
                    document: 0,
                    index: 256
                },
                Disposition::InjectionSuspect
            )),
            Some(FindingRef::Finding {
                document: 0,
                index: 256
            })
        )
    );
    Ok(())
}

#[test]
fn passes_long_identifiers_found_in_the_tree_or_base_review_md() -> Result<(), Box<dyn Error>> {
    let mut run = Run {
        review_md: "Name types like `ReviewerDocumentsAreScreened`.".to_owned(),
        ..Run::default()
    };
    run.change.head_files.insert(
        "src/names.rs".to_owned(),
        "pub struct EvidenceRelocationOutcome42;\n".to_owned(),
    );
    let citing = finding_with(json!({
        "body": "`EvidenceRelocationOutcome42` and `ReviewerDocumentsAreScreened` disagree.",
    }));

    let validated = run.validate(&[document(&[citing])])?;

    assert_eq!(
        (validated.findings.len(), validated.egress),
        (1, Vec::new())
    );
    Ok(())
}

#[test]
fn flags_a_run_of_twenty_unexplained_characters_but_not_nineteen() -> Result<(), Box<dyn Error>> {
    let nineteen = finding_with(json!({ "body": "see Abcdefghij123456789 here" }));
    let twenty = finding_with(json!({ "body": "see Abcdefghij1234567890 here" }));

    let validated = z4(&[nineteen, twenty])?;

    assert_eq!(
        validated.egress,
        vec![EgressHit {
            at: FindingRef::Finding {
                document: 0,
                index: 1
            },
            field: FreeTextField::Body,
            kind: EgressKind::UnexplainedRun,
        }]
    );
    Ok(())
}

#[test]
fn reads_an_invisible_character_as_part_of_a_run() -> Result<(), Box<dyn Error>> {
    // Ten letters, a zero-width space, ten letters: one run of 21 characters, not two of ten.
    let joined = finding_with(json!({ "body": "see abcdefghij\u{200b}klmnopqrst here" }));

    let validated = z4(&[joined])?;

    assert_eq!(
        validated.egress.first().map(|hit| hit.kind),
        Some(EgressKind::UnexplainedRun)
    );
    Ok(())
}

#[test]
fn flags_an_encoded_token_as_an_unexplained_run_without_the_token() -> Result<(), Box<dyn Error>> {
    let encoded = finding_with(json!({
        "body": "c2stYW50LW9hdDAxLVpxM0x3OV9YdjJSdH5+fkhwOE5jWWI0P01kS3M3VWUxSmE2R2YwVGk1V28+Pj8h",
    }));

    let validated = z4(&[encoded])?;

    assert_eq!(
        validated.egress.first().map(|hit| hit.kind),
        Some(EgressKind::UnexplainedRun)
    );
    Ok(())
}

/// Scans `artifact` as the review job does before uploading it, holding the token.
fn artifact_kind(artifact: &str, tree: &[&str]) -> Result<Option<EgressKind>, Box<dyn Error>> {
    let token = token()?;
    let known = KnownRuns::index(tree.iter().map(|text| text.as_bytes()))?;
    Ok(EgressScan::with_token(&token, known).scan_artifact(artifact.as_bytes()))
}

#[test]
fn passes_a_clean_artifact() -> Result<(), Box<dyn Error>> {
    let clean = String::from_utf8(document(&[finding()]))?;

    assert_eq!(artifact_kind(&clean, &[])?, None);
    Ok(())
}

#[test]
fn flags_the_token_in_an_artifact_even_behind_json_escapes() -> Result<(), Box<dyn Error>> {
    let plain = format!(r#"{{"note": "{TOKEN}"}}"#);
    // Every fifth character escaped, so no twelve-character window survives in the raw bytes.
    let escaped: String = TOKEN
        .chars()
        .enumerate()
        .map(|(at, c)| {
            if at % 5 == 0 {
                format!("\\u{:04x}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect();
    let escaped = format!(r#"{{"note": "{escaped}"}}"#);

    assert_eq!(
        (artifact_kind(&plain, &[])?, artifact_kind(&escaped, &[])?),
        (Some(EgressKind::Token), Some(EgressKind::Token))
    );
    Ok(())
}

#[test]
fn flags_token_shaped_strings_in_an_artifact_without_the_token() {
    let shaped = [
        "the credential sk-ant-api03-short",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "ghp_0123456789abcdefghijABCDEFGHIJ0123456789",
    ];
    let known = KnownRuns::default();
    let scan = EgressScan::without_token(known);

    let kinds = shaped.map(|text| scan.scan_artifact(text.as_bytes()));

    assert_eq!(kinds, [Some(EgressKind::TokenShaped); 3]);
}

#[test]
fn passes_token_shaped_runs_found_in_the_tree_and_digests() -> Result<(), Box<dyn Error>> {
    let quoted = "const VECTOR: &str = \"0123456789abcdefghijABCDEFGHIJ0123456789\";";
    let digest = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    assert_eq!(
        (
            artifact_kind(quoted, &[quoted])?,
            artifact_kind(digest, &[])?
        ),
        (None, None)
    );
    Ok(())
}
