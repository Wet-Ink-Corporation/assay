//! The reviewer prompt bundle shipped with this binary (`plugin/prompts/v1/`, 04 §3), embedded
//! when it is built. `check --local` reviews with it and `plan` puts its hash in the evidence
//! key, so both take it from [`embedded`].

use assay_app::{BundleFiles, InvalidBundle, PromptBundle};

/// The system prompt, which `check --local` also writes out for the CLI.
pub(crate) const SYSTEM_PROMPT: &str = include_str!("../../../plugin/prompts/v1/system.md");
const COMBINED: &str = include_str!("../../../plugin/prompts/v1/combined.md");
const COMBINED_MICRO: &str = include_str!("../../../plugin/prompts/v1/combined-micro.md");
const VERIFIER: &str = include_str!("../../../plugin/prompts/v1/verifier.md");
const VERIFIER_CARRIED: &str = include_str!("../../../plugin/prompts/v1/verifier-carried.md");
const COORDINATOR: &str = include_str!("../../../plugin/prompts/v1/coordinator.md");
const BUGS: &str = include_str!("../../../plugin/prompts/v1/bugs.md");
const SECURITY: &str = include_str!("../../../plugin/prompts/v1/security.md");
const DESIGN: &str = include_str!("../../../plugin/prompts/v1/design.md");
const META: &str = include_str!("../../../plugin/prompts/v1/meta.md");

/// The embedded bundle. The specialists', verifier's and coordinator's instructions are part
/// of its hash even where a command runs none of them.
///
/// # Errors
///
/// [`InvalidBundle`] when a shipped file is refused (a defect of the build).
pub(crate) fn embedded() -> Result<PromptBundle, InvalidBundle> {
    PromptBundle::new(BundleFiles {
        system: SYSTEM_PROMPT.to_owned(),
        bugs: BUGS.to_owned(),
        security: SECURITY.to_owned(),
        design: DESIGN.to_owned(),
        meta: META.to_owned(),
        combined: COMBINED.to_owned(),
        combined_micro: COMBINED_MICRO.to_owned(),
        verifier: VERIFIER.to_owned(),
        verifier_carried: VERIFIER_CARRIED.to_owned(),
        coordinator: COORDINATOR.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn the_embedded_bundle_is_the_shipped_v1_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin/prompts/v1");
        let read = |name: &str| fs::read_to_string(dir.join(name)).expect("a shipped prompt");
        let shipped = PromptBundle::new(BundleFiles {
            system: read("system.md"),
            bugs: read("bugs.md"),
            security: read("security.md"),
            design: read("design.md"),
            meta: read("meta.md"),
            combined: read("combined.md"),
            combined_micro: read("combined-micro.md"),
            verifier: read("verifier.md"),
            verifier_carried: read("verifier-carried.md"),
            coordinator: read("coordinator.md"),
        })
        .expect("the shipped bundle");

        let embedded = embedded().expect("the embedded bundle");

        assert_eq!(embedded.hash(), shipped.hash());
    }
}
