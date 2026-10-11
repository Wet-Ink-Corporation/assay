//! Access to the schemas and golden fixtures under the repository's `schemas/` directory.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;

use assay_app::Contract;
use serde_json::Value;

/// Whether a fixture must pass or fail its schema.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Validity {
    Valid,
    Invalid,
}

pub(crate) fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

fn contract_dir(contract: Contract) -> &'static str {
    match contract {
        Contract::Findings => "findings",
        Contract::Run => "run",
        Contract::Decision => "decision",
        Contract::Event => "event",
    }
}

/// Every fixture of `contract` with the given validity, by file stem.
pub(crate) fn fixtures(
    contract: Contract,
    validity: Validity,
) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let sub = match validity {
        Validity::Valid => "valid",
        Validity::Invalid => "invalid",
    };
    let dir = schemas_dir()
        .join("fixtures")
        .join(contract_dir(contract))
        .join(sub);
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| io::Error::other(format!("bad fixture name {}", path.display())))?
                .to_owned();
            found.insert(stem, fs::read(&path)?);
        }
    }
    Ok(found)
}

/// The parsed schema `schemas/<stem>.schema.json`.
pub(crate) fn schema(stem: &str) -> Result<Value, Box<dyn Error>> {
    let bytes = fs::read(schemas_dir().join(format!("{stem}.schema.json")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// The first line of `docs/architecture/03-contracts.md` that starts with `prefix`.
pub(crate) fn spec_line(prefix: &str) -> io::Result<String> {
    let spec = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/architecture/03-contracts.md"),
    )?;
    spec.lines()
        .find(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("03-contracts.md has no line starting {prefix:?}")))
}

/// The names quoted in backticks in `text`, without their `{fields}`, with every grouped
/// `StemA|B` expanded to `StemA` and `StemB` (the stem is the first name less its last
/// capitalised word).
pub(crate) fn spec_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for quoted in text.split('`').skip(1).step_by(2) {
        let bare = quoted.split_once('{').map_or(quoted, |(name, _)| name);
        let mut alternatives = bare.split('|');
        let Some(first) = alternatives.next() else {
            continue;
        };
        let stem = first
            .rfind(|c: char| c.is_ascii_uppercase())
            .and_then(|at| first.get(..at))
            .unwrap_or_default();
        names.insert(first.to_owned());
        for rest in alternatives {
            names.insert(format!("{stem}{rest}"));
        }
    }
    names
}
