//! Access to the schemas and golden fixtures under the repository's `schemas/` directory.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use assay_app::Contract;

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
