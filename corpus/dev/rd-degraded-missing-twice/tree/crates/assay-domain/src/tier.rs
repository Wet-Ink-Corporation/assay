//! Review tiers and PR classes (`docs/architecture/04-pipeline.md` §2.3,
//! `docs/architecture/03-contracts.md` §1).

use crate::vocabulary::closed_enum;

closed_enum! {
    /// How much review a PR gets: the first matching row of 04 §2.3.
    pub enum Tier {
        /// Over 5,000 filtered changed lines: no review, the PR fails `needs-split` (rule 3).
        NeedsSplit => "needs-split",
        /// Only files in the base docs allowlist: gates only.
        Trivial => "trivial",
        /// A trust path, exec surface, risk glob or more than 150 lines: the full roster.
        Full => "full",
        /// 21 to 150 lines, or a dependency change: the `combined` reviewer.
        Lite => "lite",
        /// Everything else: the `combined-micro` reviewer, advisory only.
        Micro => "micro",
    }
}

closed_enum! {
    /// A risk class of a PR (03 §1); a PR may be in both or neither.
    #[derive(PartialOrd, Ord)]
    pub enum PrClass {
        /// A changed file matches the trust-path list (04 §2.2).
        TrustPath => "trust-path",
        /// The change adds or alters code that runs at build time (04 §2.2).
        ExecSurface => "exec-surface",
    }
}
