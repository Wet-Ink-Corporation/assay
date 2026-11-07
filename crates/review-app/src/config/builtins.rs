//! `[assay] claude_cli_builtins`: the Claude Code built-ins a clean session may load, pinned
//! next to `claude_cli` (`docs/architecture/03-contracts.md` §1, §8).

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use thiserror::Error;

/// The most names one list may hold, in the allowlist and in a session's `system/init`.
pub const MAX_BUILTINS: usize = 256;

/// The longest name, in bytes, in the allowlist and in a session's `system/init`.
pub const MAX_BUILTIN_NAME_BYTES: usize = 128;

/// The three kinds of thing a Claude Code session loads that the allowlist names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinKind {
    /// A plugin (mod), listed in `init.plugins`.
    Plugin,
    /// A skill, listed in `init.skills`.
    Skill,
    /// An agent type, listed in `init.agents`.
    Agent,
}

impl BuiltinKind {
    /// Every kind, in the order the allowlist and the init list them.
    pub const ALL: [Self; 3] = [Self::Plugin, Self::Skill, Self::Agent];

    /// The allowlist key and `system/init` field of this kind.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::Plugin => "plugins",
            Self::Skill => "skills",
            Self::Agent => "agents",
        }
    }
}

impl fmt::Display for BuiltinKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Agent => "agent",
        })
    }
}

/// The plugins, skills and agents a clean session may load: the built-ins of the pinned CLI,
/// reviewed as a trust-path change. Empty when the file names none, which admits no built-in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinAllowlist {
    plugins: BTreeSet<String>,
    skills: BTreeSet<String>,
    agents: BTreeSet<String>,
}

impl BuiltinAllowlist {
    /// Whether `name` is allowlisted as a `kind`.
    #[must_use]
    pub fn contains(&self, kind: BuiltinKind, name: &str) -> bool {
        self.list(kind).contains(name)
    }

    /// The allowlisted names of `kind`, in byte order.
    pub fn names(&self, kind: BuiltinKind) -> impl Iterator<Item = &str> + '_ {
        self.list(kind).iter().map(String::as_str)
    }

    const fn list(&self, kind: BuiltinKind) -> &BTreeSet<String> {
        match kind {
            BuiltinKind::Plugin => &self.plugins,
            BuiltinKind::Skill => &self.skills,
            BuiltinKind::Agent => &self.agents,
        }
    }
}

/// Why `claude_cli_builtins` was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinsError {
    /// The allowlist is given without `claude_cli`, the version whose built-ins it lists.
    #[error("`assay.claude_cli_builtins` needs `assay.claude_cli`")]
    WithoutCli,
    /// A name is empty, longer than [`MAX_BUILTIN_NAME_BYTES`], or holds a character other than
    /// `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`.
    #[error("`{name}` is not a valid {kind} name")]
    Name {
        /// The list.
        kind: BuiltinKind,
        /// The name given.
        name: String,
    },
    /// A list names one built-in twice.
    #[error("{kind} `{name}` is listed twice")]
    Repeated {
        /// The list.
        kind: BuiltinKind,
        /// The repeated name.
        name: String,
    },
    /// A list holds more than [`MAX_BUILTINS`] names.
    #[error("{count} {kind} names are listed; at most {MAX_BUILTINS} are allowed")]
    TooMany {
        /// The list.
        kind: BuiltinKind,
        /// How many it holds.
        count: usize,
    },
}

/// The wire shape of `claude_cli_builtins`: all three lists, nothing else.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuiltinsDto {
    plugins: Vec<String>,
    skills: Vec<String>,
    agents: Vec<String>,
}

impl TryFrom<BuiltinsDto> for BuiltinAllowlist {
    type Error = BuiltinsError;

    fn try_from(dto: BuiltinsDto) -> Result<Self, Self::Error> {
        Ok(Self {
            plugins: list(BuiltinKind::Plugin, dto.plugins)?,
            skills: list(BuiltinKind::Skill, dto.skills)?,
            agents: list(BuiltinKind::Agent, dto.agents)?,
        })
    }
}

fn list(kind: BuiltinKind, names: Vec<String>) -> Result<BTreeSet<String>, BuiltinsError> {
    if names.len() > MAX_BUILTINS {
        return Err(BuiltinsError::TooMany {
            kind,
            count: names.len(),
        });
    }
    let mut set = BTreeSet::new();
    for name in names {
        if !is_builtin_name(&name) {
            return Err(BuiltinsError::Name { kind, name });
        }
        if set.contains(&name) {
            return Err(BuiltinsError::Repeated { kind, name });
        }
        set.insert(name);
    }
    Ok(set)
}

/// 1 to [`MAX_BUILTIN_NAME_BYTES`] of `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`.
fn is_builtin_name(name: &str) -> bool {
    (1..=MAX_BUILTIN_NAME_BYTES).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
