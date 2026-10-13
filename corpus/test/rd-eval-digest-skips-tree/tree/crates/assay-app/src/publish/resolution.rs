//! The note a carried finding's thread is resolved with when its re-verification says the
//! defect is gone (`docs/architecture/04-pipeline.md` §6, §7).

use super::sanitise::prose;
use crate::verify::{MAX_REASON_CHARS, VerdictReason};

/// The note: fixed text, then the verifier's one-line reason made inert like any model prose
/// (#175), so nothing in it is a link, mention or markup.
pub(crate) fn resolution_note(reason: &VerdictReason) -> String {
    format!(
        "Resolved by assay: re-verified at this head, the defect is no longer present.\n\n\
         Verifier: {}",
        prose(reason.as_str(), MAX_REASON_CHARS)
    )
}
