//! Validate-on-read for the versioned contracts (`docs/architecture/03-contracts.md`, preamble and
//! §3 to §6).
//!
//! Every contract has a JSON Schema in the repository's `schemas/` directory, embedded here at
//! compile time so the application layer reads no files. A document is accepted only when its
//! `schema` field names the expected contract at a version this build knows and the document
//! satisfies that version's schema: unknown fields, unknown versions and over-length text are all
//! rejected, never repaired.

mod violation;

use std::collections::BTreeSet;
use std::fmt;

use assay_domain::Disposition;
use jsonschema::{ValidationError, Validator};
use serde_json::Value;
use thiserror::Error;

/// The only version of each contract this build reads. A second version adds a schema and an
/// upcast, never an edit of version 1 (OUT-03).
const SUPPORTED_VERSION: u32 = 1;

/// A versioned contract assay reads across a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Contract {
    /// `assay.findings/1`: findings from a gate or a reviewer role (03 §3).
    Findings,
    /// `assay.run/1`: the run description from `plan` and `admit` (03 §4).
    Run,
    /// `assay.decision/1`: the decision `decide` publishes (03 §5).
    Decision,
    /// `assay.event/1`: one line of a job's event stream (03 §6).
    Event,
}

impl Contract {
    /// Every contract.
    pub const ALL: [Self; 4] = [Self::Findings, Self::Run, Self::Decision, Self::Event];

    /// The contract's name without its version, as it appears before the `/` of the `schema`
    /// field (`assay.findings`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Findings => "assay.findings",
            Self::Run => "assay.run",
            Self::Decision => "assay.decision",
            Self::Event => "assay.event",
        }
    }

    /// The embedded JSON Schema of the supported version.
    const fn schema_text(self) -> &'static str {
        match self {
            Self::Findings => include_str!("../../../schemas/findings-1.schema.json"),
            Self::Run => include_str!("../../../schemas/run-1.schema.json"),
            Self::Decision => include_str!("../../../schemas/decision-1.schema.json"),
            Self::Event => include_str!("../../../schemas/event-1.schema.json"),
        }
    }
}

impl fmt::Display for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The compiled schemas of every contract. Build it once at the composition root and share it by
/// reference; compiling is far more expensive than validating.
#[derive(Debug)]
pub struct ContractValidator {
    findings: Compiled,
    run: Compiled,
    decision: Compiled,
    event: Compiled,
}

/// One contract's compiled schema and the property names it declares, the only object keys a
/// reported path may show.
#[derive(Debug)]
struct Compiled {
    validator: Validator,
    declared: BTreeSet<String>,
}

impl ContractValidator {
    /// Compiles the embedded schemas.
    ///
    /// # Errors
    ///
    /// Returns [`ContractSchemaError`] when an embedded schema is not JSON or not a valid
    /// Draft 2020-12 schema. That is a defect in this build, not in any input.
    pub fn new() -> Result<Self, ContractSchemaError> {
        Ok(Self {
            findings: compile(Contract::Findings)?,
            run: compile(Contract::Run)?,
            decision: compile(Contract::Decision)?,
            event: compile(Contract::Event)?,
        })
    }

    /// Parses `document` and accepts it only as a valid `expected` contract at a supported
    /// version, returning the parsed JSON.
    ///
    /// Callers own two obligations this function cannot discharge:
    /// - **Cap the input first.** `document` is parsed whole; read untrusted input through a
    ///   byte limit before calling.
    /// - **Use only the returned [`Value`].** A JSON object with a duplicate key parses with its
    ///   last value; another parser may keep the first. Never re-parse `document`, or the
    ///   validated reading and the one acted on can differ.
    ///
    /// # Errors
    ///
    /// Returns [`ReadContractError`], whose every variant is disposition `schema`
    /// ([`ReadContractError::disposition`]):
    /// - [`Malformed`](ReadContractError::Malformed) when `document` is not JSON;
    /// - [`MissingSchema`](ReadContractError::MissingSchema) when it has no string `schema` field;
    /// - [`UnknownSchema`](ReadContractError::UnknownSchema) when that field is not
    ///   `<expected name>/<number>`;
    /// - [`UnknownVersion`](ReadContractError::UnknownVersion) when the number is not supported;
    /// - [`Invalid`](ReadContractError::Invalid) when the document breaks the schema: an unknown
    ///   field, an over-length text, a missing or mistyped field.
    ///
    /// # Examples
    ///
    /// ```
    /// use assay_app::{Contract, ContractValidator, ReadContractError};
    /// use assay_domain::Disposition;
    ///
    /// let validator = ContractValidator::new()?;
    ///
    /// let read = validator.read(Contract::Run, br#"{"schema": "assay.run/2"}"#);
    ///
    /// let Err(error) = read else {
    ///     return Err("version 2 must be rejected".into());
    /// };
    /// assert!(matches!(error, ReadContractError::UnknownVersion { version: 2, .. }));
    /// assert_eq!(error.disposition(), Disposition::Schema);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read(&self, expected: Contract, document: &[u8]) -> Result<Value, ReadContractError> {
        let value: Value =
            serde_json::from_slice(document).map_err(|source| ReadContractError::Malformed {
                contract: expected,
                source,
            })?;
        check_version(expected, &value)?;
        let compiled = self.compiled(expected);
        if let Err(error) = compiled.validator.validate(&value) {
            return Err(ReadContractError::Invalid {
                contract: expected,
                violation: violation::violation(&error, &value, &compiled.declared),
            });
        }
        Ok(value)
    }

    const fn compiled(&self, contract: Contract) -> &Compiled {
        match contract {
            Contract::Findings => &self.findings,
            Contract::Run => &self.run,
            Contract::Decision => &self.decision,
            Contract::Event => &self.event,
        }
    }
}

fn compile(contract: Contract) -> Result<Compiled, ContractSchemaError> {
    let schema: Value = serde_json::from_str(contract.schema_text())
        .map_err(|source| ContractSchemaError::NotJson { contract, source })?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|error| ContractSchemaError::NotASchema {
            contract,
            source: SchemaCompileError(Box::new(error)),
        })?;
    Ok(Compiled {
        validator,
        declared: violation::declared_names(&schema),
    })
}

/// Checks the `schema` field before the schema itself, so an unknown version is reported as such
/// rather than as a failed `const`.
fn check_version(expected: Contract, document: &Value) -> Result<(), ReadContractError> {
    let id = document
        .get("schema")
        .and_then(Value::as_str)
        .ok_or(ReadContractError::MissingSchema { contract: expected })?;
    let version = id
        .split_once('/')
        .filter(|(name, _)| *name == expected.name())
        .and_then(|(_, version)| version.parse::<u32>().ok())
        .ok_or(ReadContractError::UnknownSchema { expected })?;
    if version == SUPPORTED_VERSION {
        Ok(())
    } else {
        Err(ReadContractError::UnknownVersion {
            contract: expected,
            version,
        })
    }
}

/// Why a document was not accepted as a contract. Every variant is disposition `schema`.
#[derive(Debug, Error)]
pub enum ReadContractError {
    /// The document is not JSON.
    #[error("{contract} document is not JSON")]
    Malformed {
        /// The contract the caller expected.
        contract: Contract,
        /// The parser's error, with line and column.
        #[source]
        source: serde_json::Error,
    },
    /// The document has no string `schema` field naming its contract and version.
    #[error("{contract} document has no string `schema` field")]
    MissingSchema {
        /// The contract the caller expected.
        contract: Contract,
    },
    /// The `schema` field names another contract or is not `<name>/<number>`.
    #[error("document is not a {expected} document")]
    UnknownSchema {
        /// The contract the caller expected.
        expected: Contract,
    },
    /// The document is the expected contract at a version this build does not read.
    #[error("{contract}/{version} is not a supported version")]
    UnknownVersion {
        /// The contract the caller expected.
        contract: Contract,
        /// The version the document declares.
        version: u32,
    },
    /// The document breaks its schema.
    #[error("{contract} document is invalid at `{}`: {}", violation.instance_path, violation.message)]
    Invalid {
        /// The contract the caller expected.
        contract: Contract,
        /// The first rule the document breaks.
        violation: SchemaViolation,
    },
}

impl ReadContractError {
    /// The disposition of a finding or document rejected this way: always
    /// [`Disposition::Schema`] (`docs/architecture/04-pipeline.md` §4 step 1).
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        match self {
            Self::Malformed { .. }
            | Self::MissingSchema { .. }
            | Self::UnknownSchema { .. }
            | Self::UnknownVersion { .. }
            | Self::Invalid { .. } => Disposition::Schema,
        }
    }
}

/// The first schema rule a document breaks. No field quotes the document: not its values, and
/// not its object keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    /// JSON Pointer to the offending value (`/findings/0/evidence`); empty for the root. An
    /// object key the contract's schema does not name, such as a key of `artifact_names`, is
    /// shown as `*`.
    pub instance_path: String,
    /// The schema keyword that rejected it (`maxLength`, `additionalProperties`, `required`).
    pub keyword: String,
    /// A human-readable description that quotes only the schema; for `additionalProperties` it
    /// counts the unexpected properties without naming them.
    pub message: String,
}

/// An embedded schema failed to compile: a defect in this build.
#[derive(Debug, Error)]
pub enum ContractSchemaError {
    /// The embedded schema text is not JSON.
    #[error("embedded {contract} schema is not JSON")]
    NotJson {
        /// The contract whose schema is broken.
        contract: Contract,
        /// The parser's error.
        #[source]
        source: serde_json::Error,
    },
    /// The embedded schema is not a valid Draft 2020-12 schema.
    #[error("embedded {contract} schema is not a valid JSON Schema")]
    NotASchema {
        /// The contract whose schema is broken.
        contract: Contract,
        /// The schema compiler's error.
        #[source]
        source: SchemaCompileError,
    },
}

/// Why the schema compiler rejected an embedded schema. Opaque, so the compiler's own error
/// type stays out of this crate's API (ERR-04).
#[derive(Debug, Error)]
#[error(transparent)]
pub struct SchemaCompileError(Box<ValidationError<'static>>);
