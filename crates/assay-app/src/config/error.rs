//! Why `.assay/plugins.toml` was refused.

use thiserror::Error;

use super::MAX_TIMEOUT_SECS;
use super::builtins::BuiltinsError;
use super::pin::{ParseDigestError, ParsePinError};

/// Why `.assay/plugins.toml` was refused. The file comes from the protected base, so errors may
/// quote it.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Not TOML, an unknown key, a missing required key, or a value of the wrong type.
    #[error("plugins.toml is not a valid configuration")]
    Syntax(#[source] TomlError),
    /// A digest under `[assay]` is not `sha256:<64 hex>`.
    #[error("`{field}` must be an immutable digest")]
    Digest {
        /// The key, such as `assay.digest`.
        field: &'static str,
        /// What was wrong with it.
        #[source]
        source: ParseDigestError,
    },
    /// `claude_cli` is not an exact `<major>.<minor>.<patch>` version.
    #[error("`assay.claude_cli` must be an exact version, not `{version}`")]
    Version {
        /// The value given.
        version: String,
    },
    /// `claude_cli_builtins` was refused.
    #[error(transparent)]
    Builtins(#[from] BuiltinsError),
    /// A plugin's pin is a tag or has no known scheme.
    #[error("plugin `{plugin}` has an invalid pin")]
    Pin {
        /// The plugin.
        plugin: String,
        /// What was wrong with the pin.
        #[source]
        source: ParsePinError,
    },
    /// A plugin or step name is not 1 to 64 of `a-z`, `0-9` and `-`.
    #[error("`{name}` is not a valid plugin or step name")]
    Name {
        /// The name given.
        name: String,
    },
    /// Two plugins, or two steps of one plugin (`plugin.step`), share a name.
    #[error("`{name}` is declared twice")]
    DuplicateName {
        /// The repeated name.
        name: String,
    },
    /// `adapter` is none of `builtin:temper`, `builtin:weigh-in` and `exec`.
    #[error("plugin `{gate}` names an unknown adapter `{adapter}`")]
    UnknownAdapter {
        /// The plugin.
        gate: String,
        /// The adapter given.
        adapter: String,
    },
    /// The adapter needs a key the entry lacks (`pin`, `exec`, `job`).
    #[error("`{gate}` needs `{key}`")]
    MissingKey {
        /// The plugin.
        gate: String,
        /// The missing key.
        key: &'static str,
    },
    /// The entry has a key its adapter does not use: a pin on `exec` (its script is
    /// PR-controlled), steps outside temper, `exec` or `job` on a built-in.
    #[error("`{gate}` does not take `{key}`")]
    UnexpectedKey {
        /// The plugin.
        gate: String,
        /// The key that is not allowed.
        key: &'static str,
    },
    /// A `job` is not a Z2b job (`lint` or `test`).
    #[error("`{gate}` names job `{job}`; only `lint` and `test` run declared commands")]
    Job {
        /// The plugin, or `plugin.step`.
        gate: String,
        /// The job given.
        job: String,
    },
    /// A command is empty, or has an empty argument or one holding a NUL byte.
    #[error("`{gate}` has an empty or malformed command")]
    Command {
        /// The plugin, or `plugin.step`.
        gate: String,
    },
    /// `timeout_s` is zero or longer than [`MAX_TIMEOUT_SECS`].
    #[error("`{gate}` has timeout_s = {seconds}; it must be 1 to {MAX_TIMEOUT_SECS}")]
    Timeout {
        /// The plugin, or `plugin.step`.
        gate: String,
        /// The value given.
        seconds: u64,
    },
    /// A declared step passes `--no-tests` in any spelling but exactly `--no-tests=fail`, which
    /// could let a run of zero tests pass (`docs/architecture/03-contracts.md` §2.1 step 7).
    #[error("`{gate}` passes `{arg}`; a declared step may pass only `--no-tests=fail`")]
    NoTests {
        /// The `plugin.step`.
        gate: String,
        /// The refused argument.
        arg: String,
    },
}

/// The TOML parser's error, with line and column. Opaque, so the parser's type stays out of this
/// crate's API (ERR-04).
#[derive(Debug, Error)]
#[error(transparent)]
pub struct TomlError(pub(super) toml::de::Error);
