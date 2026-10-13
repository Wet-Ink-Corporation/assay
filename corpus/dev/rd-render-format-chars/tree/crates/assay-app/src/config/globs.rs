//! Base `.assay/globs.toml`: the globs a repository adds to tiering's shipped lists
//! (`docs/architecture/03-contracts.md` §1, `04-pipeline.md` §2).

use assay_domain::{Glob, GlobError, RepoGlobs};
use serde::Deserialize;
use thiserror::Error;

use super::error::TomlError;

/// Why `.assay/globs.toml` was refused. The file comes from the protected base, so errors may
/// quote it.
#[derive(Debug, Error)]
pub enum GlobsConfigError {
    /// Not TOML, an unknown key, or a value that is not a list of strings.
    #[error("globs.toml is not a valid configuration")]
    Syntax(#[source] TomlError),
    /// A pattern is not a valid glob.
    #[error("`{key}` has an invalid glob `{pattern}`")]
    Glob {
        /// The list: `risk`, `trust`, `exclude` or `docs`.
        key: &'static str,
        /// The pattern given.
        pattern: String,
        /// What was wrong with it.
        #[source]
        source: GlobError,
    },
}

/// The wire shape: four optional lists of patterns, and nothing else, so no key can remove a
/// shipped glob.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobsDto {
    #[serde(default)]
    risk: Vec<String>,
    #[serde(default)]
    trust: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    docs: Vec<String>,
}

/// Parses base `.assay/globs.toml`: optional lists `risk`, `trust`, `exclude` and `docs` of
/// globs, each added to the shipped list of that name.
///
/// # Errors
///
/// [`GlobsConfigError::Syntax`] for text that is not TOML or has any other key, and
/// [`GlobsConfigError::Glob`] for a pattern [`Glob`] refuses.
///
/// # Examples
///
/// ```
/// let globs = assay_app::parse_globs(r#"risk = ["**/billing/**"]"#)?;
/// assert!(globs.risk.iter().any(|glob| glob.matches("src/billing/mod.rs")));
/// # Ok::<(), assay_app::GlobsConfigError>(())
/// ```
pub fn parse_globs(text: &str) -> Result<RepoGlobs, GlobsConfigError> {
    let dto: GlobsDto = toml::from_str(text).map_err(|e| GlobsConfigError::Syntax(TomlError(e)))?;
    Ok(RepoGlobs {
        risk: globs("risk", dto.risk)?,
        trust: globs("trust", dto.trust)?,
        exclude: globs("exclude", dto.exclude)?,
        docs: globs("docs", dto.docs)?,
    })
}

fn globs(key: &'static str, patterns: Vec<String>) -> Result<Vec<Glob>, GlobsConfigError> {
    patterns
        .into_iter()
        .map(|pattern| match pattern.parse() {
            Ok(glob) => Ok(glob),
            Err(source) => Err(GlobsConfigError::Glob {
                key,
                pattern,
                source,
            }),
        })
        .collect()
}
