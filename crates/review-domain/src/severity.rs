//! Severities and the marker each finding is displayed with (`docs/architecture/03-contracts.md`
//! §3).

use std::cmp::Ordering;
use std::fmt;

use crate::vocabulary::closed_enum;

closed_enum! {
    /// How bad a finding is (03 §3; operational definitions for reviewers in 04 §3).
    ///
    /// Ordered from least to most severe, so `Severity::Nit < Severity::Blocker`; `ALL` lists
    /// them as 03 does, most severe first.
    pub enum Severity {
        /// A problem that must be fixed before merge.
        Blocker => "blocker",
        /// A problem that should be fixed before merge.
        Major => "major",
        /// A real but low-impact problem.
        Minor => "minor",
        /// A matter of taste or polish.
        Nit => "nit",
    }
}

impl Severity {
    /// Position from least (0) to most (3) severe.
    const fn rank(self) -> u8 {
        match self {
            Self::Nit => 0,
            Self::Minor => 1,
            Self::Major => 2,
            Self::Blocker => 3,
        }
    }
}

impl Ord for Severity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for Severity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The marker a finding is displayed with in comments and the check summary (03 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Marker {
    /// 🔴: a `blocker`.
    Blocker,
    /// 🟡: a `major`.
    Major,
    /// 🟡 (nit): a `minor` or a `nit`.
    Nit,
    /// 🟣: about code the diff did not introduce, whatever its severity.
    PreExisting,
}

/// Whether the diff introduced a finding, for its [`Marker`]: a finding's `pre_existing` flag,
/// named (#147).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// The diff introduced it.
    Introduced,
    /// It is about code the diff did not introduce.
    PreExisting,
}

impl Provenance {
    /// The provenance a finding's `pre_existing` flag names: `true` is [`Self::PreExisting`].
    #[must_use]
    pub const fn from_pre_existing(pre_existing: bool) -> Self {
        if pre_existing {
            Self::PreExisting
        } else {
            Self::Introduced
        }
    }
}

impl Marker {
    /// The marker for a finding of `severity` and `provenance`: a pre-existing finding is 🟣
    /// whatever its severity.
    #[must_use]
    pub const fn of(severity: Severity, provenance: Provenance) -> Self {
        match (provenance, severity) {
            (Provenance::PreExisting, _) => Self::PreExisting,
            (Provenance::Introduced, Severity::Blocker) => Self::Blocker,
            (Provenance::Introduced, Severity::Major) => Self::Major,
            (Provenance::Introduced, Severity::Minor | Severity::Nit) => Self::Nit,
        }
    }

    /// The text shown for this marker.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocker => "🔴",
            Self::Major => "🟡",
            Self::Nit => "🟡 (nit)",
            Self::PreExisting => "🟣",
        }
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_rank_from_nit_to_blocker() {
        let mut ranked = Severity::ALL;

        ranked.sort_unstable();

        assert_eq!(
            ranked,
            [
                Severity::Nit,
                Severity::Minor,
                Severity::Major,
                Severity::Blocker
            ]
        );
    }

    #[test]
    fn the_pre_existing_flag_names_its_provenance() {
        let named = [false, true].map(Provenance::from_pre_existing);

        assert_eq!(named, [Provenance::Introduced, Provenance::PreExisting]);
    }

    #[test]
    fn introduced_findings_are_marked_by_severity() {
        let shown =
            Severity::ALL.map(|severity| Marker::of(severity, Provenance::Introduced).to_string());

        assert_eq!(shown, ["🔴", "🟡", "🟡 (nit)", "🟡 (nit)"]);
    }

    #[test]
    fn pre_existing_findings_are_purple_whatever_their_severity() {
        let shown = Severity::ALL.map(|severity| Marker::of(severity, Provenance::PreExisting));

        assert_eq!(shown, [Marker::PreExisting; 4]);
        assert_eq!(Marker::PreExisting.to_string(), "🟣");
    }
}
