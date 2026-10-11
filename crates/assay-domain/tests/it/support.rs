//! Access to the schemas, golden fixtures and the in-tree specification.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The parsed schema `schemas/<stem>.schema.json`.
pub(crate) fn schema(stem: &str) -> Result<Value, Box<dyn Error>> {
    let bytes = fs::read(repo_root().join(format!("schemas/{stem}.schema.json")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// The string values of the JSON array at `pointer` in `schema`.
pub(crate) fn enum_at(schema: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    Ok(schema
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no enum at {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// The wire form of each value, which must serialise to a JSON string.
pub(crate) fn wire_codes<T: Serialize>(
    values: impl IntoIterator<Item = T>,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    values
        .into_iter()
        .map(|value| match serde_json::to_value(value)? {
            Value::String(code) => Ok(code),
            other => Err(format!("not a string code: {other}").into()),
        })
        .collect()
}

/// The first line of `docs/architecture/03-contracts.md` that starts with `prefix`.
pub(crate) fn spec_line(prefix: &str) -> io::Result<String> {
    let spec = fs::read_to_string(repo_root().join("docs/architecture/03-contracts.md"))?;
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

/// Every valid golden fixture of `contract` (`findings`, `event`, ...), by file stem.
pub(crate) fn valid_fixtures(contract: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    fixtures(contract, "valid")
}

/// Every invalid golden fixture of `contract`, by file stem.
pub(crate) fn invalid_fixtures(contract: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    fixtures(contract, "invalid")
}

fn fixtures(contract: &str, validity: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let dir = repo_root()
        .join("schemas/fixtures")
        .join(contract)
        .join(validity);
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
