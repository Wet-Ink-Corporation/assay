//! What the record kinds share: an append's outcome and the field rules of their wire forms.

use super::error::Corruption;

/// What an append of a keyed record did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended<R> {
    /// The record was written.
    Recorded,
    /// A record with its key was already there, so nothing was written: this one, which may
    /// differ from the one given (the first writer's wins, R3-16).
    AlreadyRecorded(R),
}

/// `text` parsed as `T`, or the field named `name` is refused.
pub(super) fn field<T: std::str::FromStr>(text: &str, name: &'static str) -> Result<T, Corruption> {
    text.parse().map_err(|_value| Corruption::Field(name))
}

/// Whether `text` is a GitHub `owner/name`: an owner of 1 to 39 ASCII letters, digits and `-`,
/// and a name of 1 to 100 ASCII letters, digits, `.`, `_` and `-`, other than `.` and `..`.
pub(super) fn is_repository(text: &str) -> bool {
    let Some((owner, name)) = text.split_once('/') else {
        return false;
    };
    let owner_ok = (1..=39).contains(&owner.len())
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    owner_ok && name_ok
}
