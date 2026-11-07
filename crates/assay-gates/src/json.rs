//! Refusals of JSON a gate tool printed or wrote, said without quoting it.

/// What was wrong with a JSON document, without quoting it: its text may be PR-controlled, and
/// serde's messages can quote values (the assay-app charter's rule for validation errors).
pub(crate) fn json_detail(error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "unreadable",
        serde_json::error::Category::Syntax => "not valid JSON",
        serde_json::error::Category::Data => "not the expected shape",
        serde_json::error::Category::Eof => "truncated",
    };
    format!(
        "{category} at line {} column {}",
        error.line(),
        error.column()
    )
}
