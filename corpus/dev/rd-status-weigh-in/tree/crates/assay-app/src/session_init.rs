//! A Claude Code session's `system/init` message and the clean-init rule
//! (`docs/architecture/03-contracts.md` §8).
//!
//! The runner reads the message out of the stream; this module bounds what it holds and judges
//! it. A session is clean when it runs the pinned CLI version with no MCP server, exactly the
//! tools `Glob`, `Grep` and `Read`, and only built-in plugins, skills and agents on the
//! allowlist pinned next to that version. Built-ins cannot be switched off headlessly, so they
//! are allowlisted rather than required absent.

use std::collections::BTreeSet;
use std::fmt;

use assay_domain::BuiltinSet;
use thiserror::Error;

use crate::config::{
    BuiltinAllowlist, BuiltinKind, ExactVersion, MAX_BUILTIN_NAME_BYTES, MAX_BUILTINS,
};

/// The tools of a clean session, sorted (03 §8: `--tools Read,Grep,Glob`).
const CLEAN_TOOLS: [&str; 3] = ["Glob", "Grep", "Read"];

/// The `path` of a plugin the CLI ships.
const BUILTIN_PATH: &str = "builtin";

/// The suffix of a built-in plugin's `source`, after its name.
const BUILTIN_SOURCE_SUFFIX: &str = "@builtin";

/// One entry of `init.plugins`, as the CLI wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPlugin {
    /// The plugin's name.
    pub name: String,
    /// Where it was loaded from; `builtin` for one the CLI ships.
    pub path: String,
    /// `<name>@<marketplace>`; `<name>@builtin` for one the CLI ships.
    pub source: String,
}

/// The fields of a `system/init` message the clean-init rule reads, as the runner found them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitParts {
    /// `claude_code_version`.
    pub cli_version: String,
    /// `tools`.
    pub tools: Vec<String>,
    /// The `name` of each `mcp_servers` entry.
    pub mcp_servers: Vec<String>,
    /// `plugins`.
    pub plugins: Vec<InitPlugin>,
    /// `skills`.
    pub skills: Vec<String>,
    /// `agents`.
    pub agents: Vec<String>,
}

/// A `system/init` message within bounds.
///
/// Every list holds at most [`MAX_BUILTINS`] entries and every name is 1 to
/// [`MAX_BUILTIN_NAME_BYTES`] bytes with no control character. Build it with
/// [`SessionInit::new`]; judge it with [`check_init`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInit {
    // Boxed: every `ModelResult` carries one, and the lists would otherwise make it, and the
    // retry error holding it, several hundred bytes.
    parts: Box<InitParts>,
}

/// Why no [`SessionInit`] could be read from a transcript. Every variant fails the preflight.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InitReadError {
    /// The transcript's first message is not a `system/init` message.
    #[error("the transcript does not start with a system/init message")]
    Missing,
    /// A second `system/init` message follows the first.
    #[error("the transcript holds more than one system/init message")]
    Repeated,
    /// An object in the `system/init` message repeats a key, so which value counts depends on
    /// the parser (RFC 8259 §4).
    #[error("the system/init message repeats a key")]
    RepeatedKey,
    /// A field the rule reads is absent or not of the expected type.
    #[error("system/init field `{field}` is absent or malformed")]
    Field {
        /// The field.
        field: &'static str,
    },
    /// An `mcp_servers` or `plugins` entry is not an object of the expected keys and types.
    #[error("system/init `{field}` entry {index} is malformed")]
    Entry {
        /// The list.
        field: &'static str,
        /// The entry's position.
        index: usize,
    },
    /// A list holds more than [`MAX_BUILTINS`] entries.
    #[error("system/init `{field}` holds {count} entries; at most {MAX_BUILTINS} are read")]
    TooMany {
        /// The list.
        field: &'static str,
        /// How many entries it holds.
        count: usize,
    },
    /// A name is empty, longer than [`MAX_BUILTIN_NAME_BYTES`] bytes or holds a control
    /// character.
    #[error("system/init `{field}` entry {index} is not a readable name")]
    Name {
        /// The list, or `claude_code_version`.
        field: &'static str,
        /// The entry's position (0 for the version).
        index: usize,
    },
}

impl SessionInit {
    /// Bounds `parts`.
    ///
    /// # Errors
    /// [`InitReadError::TooMany`] for a list over [`MAX_BUILTINS`] entries and
    /// [`InitReadError::Name`] for a name (or the version, or any field of a plugin entry) that
    /// is empty, over [`MAX_BUILTIN_NAME_BYTES`] bytes or holds a control character.
    pub fn new(parts: InitParts) -> Result<Self, InitReadError> {
        if !is_readable(&parts.cli_version) {
            return Err(InitReadError::Name {
                field: "claude_code_version",
                index: 0,
            });
        }
        names("tools", &parts.tools)?;
        names("mcp_servers", &parts.mcp_servers)?;
        bounded("plugins", parts.plugins.len())?;
        let unreadable_plugin = parts.plugins.iter().position(|plugin| {
            !(is_readable(&plugin.name) && is_readable(&plugin.path) && is_readable(&plugin.source))
        });
        if let Some(index) = unreadable_plugin {
            return Err(InitReadError::Name {
                field: "plugins",
                index,
            });
        }
        names("skills", &parts.skills)?;
        names("agents", &parts.agents)?;
        Ok(Self {
            parts: Box::new(parts),
        })
    }

    /// `claude_code_version`.
    #[must_use]
    pub fn cli_version(&self) -> &str {
        &self.parts.cli_version
    }

    /// `tools`, as listed.
    #[must_use]
    pub fn tools(&self) -> &[String] {
        &self.parts.tools
    }

    /// The MCP servers' names, as listed.
    #[must_use]
    pub fn mcp_servers(&self) -> &[String] {
        &self.parts.mcp_servers
    }

    /// `plugins`, as listed.
    #[must_use]
    pub fn plugins(&self) -> &[InitPlugin] {
        &self.parts.plugins
    }

    /// `skills`, as listed.
    #[must_use]
    pub fn skills(&self) -> &[String] {
        &self.parts.skills
    }

    /// `agents`, as listed.
    #[must_use]
    pub fn agents(&self) -> &[String] {
        &self.parts.agents
    }

    /// The plugin, skill and agent names as listed, for `PreflightPassed|Failed` (03 §6).
    #[must_use]
    pub fn builtin_set(&self) -> BuiltinSet {
        BuiltinSet {
            plugins: self.parts.plugins.iter().map(|p| p.name.clone()).collect(),
            skills: self.parts.skills.clone(),
            agents: self.parts.agents.clone(),
        }
    }

    /// The names of `kind`, as listed.
    fn names(&self, kind: BuiltinKind) -> impl Iterator<Item = &str> + '_ {
        let (plugins, others): (&[InitPlugin], &[String]) = match kind {
            BuiltinKind::Plugin => (&self.parts.plugins, &[]),
            BuiltinKind::Skill => (&[], &self.parts.skills),
            BuiltinKind::Agent => (&[], &self.parts.agents),
        };
        plugins
            .iter()
            .map(|plugin| plugin.name.as_str())
            .chain(others.iter().map(String::as_str))
    }
}

fn bounded(field: &'static str, count: usize) -> Result<(), InitReadError> {
    if count > MAX_BUILTINS {
        Err(InitReadError::TooMany { field, count })
    } else {
        Ok(())
    }
}

fn names(field: &'static str, names: &[String]) -> Result<(), InitReadError> {
    bounded(field, names.len())?;
    match names.iter().position(|name| !is_readable(name)) {
        Some(index) => Err(InitReadError::Name { field, index }),
        None => Ok(()),
    }
}

/// 1 to [`MAX_BUILTIN_NAME_BYTES`] bytes, none of them a control character.
pub(crate) fn is_readable(name: &str) -> bool {
    (1..=MAX_BUILTIN_NAME_BYTES).contains(&name.len()) && !name.chars().any(char::is_control)
}

/// Why a session's init is not clean (`init-unclean`, 03 §8). Names are CLI output, possibly
/// planted by a PR, so they are shown escaped.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InitUnclean {
    /// The session ran another CLI version than the pinned one, whose built-ins the allowlist
    /// does not describe.
    #[error("the session ran CLI {found:?}, not the pinned {pinned}")]
    CliVersion {
        /// The version the init reported.
        found: String,
        /// The pinned version.
        pinned: String,
    },
    /// An MCP server was loaded.
    #[error("MCP server {name:?} is loaded")]
    McpServer {
        /// Its name.
        name: String,
    },
    /// The tools are not exactly `Glob`, `Grep` and `Read`, each once.
    #[error("the tools are {found:?}, not exactly Glob, Grep and Read")]
    Tools {
        /// The tools as listed.
        found: Vec<String>,
    },
    /// A plugin was not shipped with the CLI: its path is not `builtin` or its source not
    /// `<name>@builtin`.
    #[error("plugin {name:?} is not a built-in")]
    NotBuiltin {
        /// Its name.
        name: String,
    },
    /// A built-in, skill or agent is not on the allowlist.
    #[error("{kind} {name:?} is not on the built-in allowlist")]
    Unlisted {
        /// What it is.
        kind: BuiltinKind,
        /// Its name.
        name: String,
    },
    /// A name is listed twice, so one entry may shadow the other.
    #[error("{kind} {name:?} is listed twice")]
    Repeated {
        /// What it is.
        kind: BuiltinKind,
        /// Its name.
        name: String,
    },
}

/// Judges `init` against the pinned CLI version and its built-in allowlist (03 §8), reporting
/// the first reason it is unclean in the order: version, MCP servers, tools, plugins, skills,
/// agents.
///
/// # Errors
/// [`InitUnclean`] naming the first rule `init` breaks.
pub fn check_init(
    init: &SessionInit,
    pinned: &ExactVersion,
    allowlist: &BuiltinAllowlist,
) -> Result<(), InitUnclean> {
    if init.cli_version() != pinned.as_str() {
        return Err(InitUnclean::CliVersion {
            found: init.cli_version().to_owned(),
            pinned: pinned.as_str().to_owned(),
        });
    }
    if let Some(name) = init.mcp_servers().first() {
        return Err(InitUnclean::McpServer { name: name.clone() });
    }
    let mut tools: Vec<&str> = init.tools().iter().map(String::as_str).collect();
    tools.sort_unstable();
    if tools != CLEAN_TOOLS {
        return Err(InitUnclean::Tools {
            found: init.tools().to_vec(),
        });
    }
    if let Some(plugin) = init.plugins().iter().find(|plugin| !is_builtin(plugin)) {
        return Err(InitUnclean::NotBuiltin {
            name: plugin.name.clone(),
        });
    }
    for kind in BuiltinKind::ALL {
        listed(kind, init.names(kind), allowlist)?;
    }
    Ok(())
}

/// Whether `plugin` is one the CLI ships: path `builtin`, source `<name>@builtin`.
fn is_builtin(plugin: &InitPlugin) -> bool {
    plugin.path == BUILTIN_PATH
        && plugin
            .source
            .strip_suffix(BUILTIN_SOURCE_SUFFIX)
            .is_some_and(|name| name == plugin.name)
}

/// Fails on the first of `names` that repeats an earlier one or is not allowlisted.
fn listed<'a>(
    kind: BuiltinKind,
    names: impl Iterator<Item = &'a str>,
    allowlist: &BuiltinAllowlist,
) -> Result<(), InitUnclean> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(InitUnclean::Repeated {
                kind,
                name: name.to_owned(),
            });
        }
        if !allowlist.contains(kind, name) {
            return Err(InitUnclean::Unlisted {
                kind,
                name: name.to_owned(),
            });
        }
    }
    Ok(())
}

/// How the built-ins a session listed differ from the allowlist, both sides sorted. After a CLI
/// pin bump this is the change to review (03 §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowlistDrift {
    /// Listed by the session, not on the allowlist.
    pub unlisted: BuiltinSet,
    /// On the allowlist, not listed by the session.
    pub unobserved: BuiltinSet,
}

/// The difference between the plugins, skills and agents `init` lists and `allowlist`, or
/// `None` when they are the same sets.
#[must_use]
pub fn allowlist_drift(init: &SessionInit, allowlist: &BuiltinAllowlist) -> Option<AllowlistDrift> {
    let mut drift = AllowlistDrift::default();
    for kind in BuiltinKind::ALL {
        let observed: BTreeSet<&str> = init.names(kind).collect();
        let pinned: BTreeSet<&str> = allowlist.names(kind).collect();
        *side(&mut drift.unlisted, kind) = observed
            .difference(&pinned)
            .map(|name| (*name).to_owned())
            .collect();
        *side(&mut drift.unobserved, kind) = pinned
            .difference(&observed)
            .map(|name| (*name).to_owned())
            .collect();
    }
    (drift != AllowlistDrift::default()).then_some(drift)
}

const fn side(set: &mut BuiltinSet, kind: BuiltinKind) -> &mut Vec<String> {
    match kind {
        BuiltinKind::Plugin => &mut set.plugins,
        BuiltinKind::Skill => &mut set.skills,
        BuiltinKind::Agent => &mut set.agents,
    }
}

fn list(set: &BuiltinSet, kind: BuiltinKind) -> &[String] {
    match kind {
        BuiltinKind::Plugin => &set.plugins,
        BuiltinKind::Skill => &set.skills,
        BuiltinKind::Agent => &set.agents,
    }
}

impl fmt::Display for AllowlistDrift {
    /// One `+ <kind> "<name>"` per unlisted name, then one `- <kind> "<name>"` per unobserved
    /// name, comma-separated.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for (sign, set) in [("+", &self.unlisted), ("-", &self.unobserved)] {
            for kind in BuiltinKind::ALL {
                for name in list(set, kind) {
                    write!(f, "{separator}{sign} {kind} {name:?}")?;
                    separator = ", ";
                }
            }
        }
        Ok(())
    }
}
