//! The artifact scan (05 §5): what the review job uploads holds no token and no token-shaped
//! string the tree does not explain.

use std::error::Error;

use assay_app::{EgressKind, EgressScan, KnownRuns};

use super::super::fixture::{document, finding};
use super::{TOKEN, token};

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

/// `TOKEN` with every fifth character written as a JSON `\u` escape, so no twelve-character
/// window survives in the raw bytes.
fn escaped_token() -> String {
    TOKEN
        .chars()
        .enumerate()
        .map(|(at, c)| {
            if at % 5 == 0 {
                format!("\\u{:04x}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect()
}

#[test]
fn flags_the_token_in_an_artifact_even_behind_json_escapes() -> Result<(), Box<dyn Error>> {
    let plain = format!(r#"{{"note": "{TOKEN}"}}"#);
    let escaped = format!(r#"{{"note": "{}"}}"#, escaped_token());

    assert_eq!(
        (artifact_kind(&plain, &[])?, artifact_kind(&escaped, &[])?),
        (Some(EgressKind::Token), Some(EgressKind::Token))
    );
    Ok(())
}

#[test]
fn unescapes_an_artifact_that_repeats_a_key() -> Result<(), Box<dyn Error>> {
    let repeated = format!(r#"{{"note": "{}", "note": "x"}}"#, escaped_token());

    assert_eq!(artifact_kind(&repeated, &[])?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn unescapes_an_artifact_with_a_trailing_byte() -> Result<(), Box<dyn Error>> {
    let trailing = format!(r#"{{"note": "{}"}}x"#, escaped_token());

    assert_eq!(artifact_kind(&trailing, &[])?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn unescapes_an_artifact_nested_past_the_parsers_depth() -> Result<(), Box<dyn Error>> {
    let deep = format!(
        "{}\"{}\"{}",
        "[".repeat(200),
        escaped_token(),
        "]".repeat(200)
    );

    assert_eq!(artifact_kind(&deep, &[])?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn refuses_an_artifact_it_cannot_unescape() {
    // Not strict JSON (a trailing byte), and `\q` is no escape: what it spells is unknown.
    let ambiguous = br#"{"note": "\q"}x"#;
    let scan = EgressScan::without_token(KnownRuns::default());

    assert_eq!(scan.scan_artifact(ambiguous), Some(EgressKind::TokenShaped));
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

#[test]
fn flags_a_lookalike_alphanumeric_in_an_artifact() {
    let scan = EgressScan::without_token(KnownRuns::default());

    assert_eq!(
        scan.scan_artifact(r#"{"note": "\ud835\udc33"}"#.as_bytes()),
        Some(EgressKind::Obfuscated)
    );
}
