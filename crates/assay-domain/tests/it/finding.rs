//! A finding's display marker (03 §3).

use std::error::Error;

use assay_domain::{FindingsDocument, Marker, Severity};

use crate::support::valid_fixtures;

#[test]
fn a_finding_is_marked_by_its_severity_unless_claimed_pre_existing() -> Result<(), Box<dyn Error>> {
    let fixtures = valid_fixtures("findings")?;
    let bytes = fixtures.get("reviewer").ok_or("no reviewer fixture")?;
    let document: FindingsDocument = serde_json::from_slice(bytes)?;
    let mut finding = document.findings.into_iter().next().ok_or("no finding")?;
    finding.severity = Severity::Blocker;

    let introduced = finding.marker();
    finding.pre_existing = true;
    let pre_existing = finding.marker();

    assert_eq!(
        (introduced, pre_existing),
        (Marker::Blocker, Marker::PreExisting)
    );
    Ok(())
}
