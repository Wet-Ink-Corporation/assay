//! The facts doctor reads from one workflow file (05 §7, T20, T22): its triggers, the actions
//! and reusable workflows it uses, and for each job whether it holds a secret or a write token
//! and whether it carries the `github.run_attempt == 1` guard.
//!
//! Every reading over-approximates towards a failure (wi-b5affa): a job holds a secret when
//! the text `secrets`, in any ASCII case, appears anywhere in it or in the workflow's `env`, or
//! it names an
//! `environment`; it holds a write token unless `permissions` at job or workflow level grants
//! only `read` or `none` (a job with none declared gets the repository's default token, which
//! may write); it is guarded only when its `if` is a conjunction, at the top level, of which
//! one term is exactly `github.run_attempt == 1`.

use std::fmt;

use super::yaml::{self, Node, YamlError};

#[cfg(test)]
use super::yaml::YamlRefusal;

/// The guard every job holding a secret or a write token carries (T22, R3-05).
const GUARD: &str = "github.run_attempt == 1";

/// The expression context of secrets, matched in any ASCII case as GitHub reads it.
const SECRETS: &str = "secrets";

/// What doctor reads from one workflow file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workflow {
    /// The events under `on`, in file order.
    pub(crate) triggers: Vec<String>,
    /// Every `uses` of a job or a step, in file order.
    pub(crate) uses: Vec<String>,
    /// The jobs, in file order.
    pub(crate) jobs: Vec<Job>,
}

/// One job of a workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Job {
    /// Its id under `jobs`.
    pub(crate) id: String,
    /// Whether its `if` carries the guard.
    pub(crate) guarded: bool,
    /// What privileged thing it holds, if any.
    pub(crate) holds: Option<Holding>,
}

/// Why a job counts as privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holding {
    /// It names an `environment`, whose secrets it receives.
    Environment,
    /// It, or the workflow's `env`, mentions `secrets`.
    Secret,
    /// Its token may write: `permissions` grants a `write`, or is declared nowhere.
    WriteToken,
}

impl fmt::Display for Holding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Environment => "an environment",
            Self::Secret => "a secret",
            Self::WriteToken => "a token that may write",
        })
    }
}

/// Why a workflow file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    /// The YAML reader refused it.
    #[error("unreadable: {0}")]
    Unreadable(#[source] YamlError),
    /// It reads as YAML but not as a workflow doctor can classify.
    #[error("unexpected shape: {0}")]
    Shape(&'static str),
}

/// A token's reach, from `permissions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// Nothing it grants writes.
    ReadOnly,
    /// Some scope it grants writes.
    Write,
}

impl Workflow {
    /// Reads the workflow in `text`.
    ///
    /// # Errors
    ///
    /// [`WorkflowError::Unreadable`] when the YAML reader refuses it, and
    /// [`WorkflowError::Shape`] when it has no triggers or jobs, or a `uses` or `permissions`
    /// the reader cannot classify.
    pub(crate) fn read(text: &str) -> Result<Self, WorkflowError> {
        let mut root = yaml::read(text).map_err(WorkflowError::Unreadable)?;
        let triggers = triggers(root.remove("on"))?;
        let Some(Node::Map(jobs)) = root.remove("jobs") else {
            return Err(WorkflowError::Shape("no `jobs` mapping"));
        };
        let workflow_token = token(root.get("permissions"))?;
        let env_secret = root
            .get("env")
            .is_some_and(|env| env.mentions_ignoring_case(SECRETS));
        let mut uses = Vec::new();
        let mut read_jobs = Vec::with_capacity(jobs.len());
        for (id, mut job) in jobs {
            let Node::Map(_) = job else {
                return Err(WorkflowError::Shape("a job is not a mapping"));
            };
            let job_token = token(job.get("permissions"))?.or(workflow_token);
            let guarded =
                matches!(job.get("if"), Some(Node::Scalar(condition)) if is_guarded(condition));
            let holds = holding(&job, env_secret, job_token);
            job_uses(&mut job, &mut uses)?;
            read_jobs.push(Job { id, guarded, holds });
        }
        Ok(Self {
            triggers,
            uses,
            jobs: read_jobs,
        })
    }
}

/// The event names under `on`.
fn triggers(on: Option<Node>) -> Result<Vec<String>, WorkflowError> {
    let names = match on {
        Some(Node::Scalar(name)) => vec![name],
        Some(Node::Seq(items)) => items
            .into_iter()
            .map(|item| match item {
                Node::Scalar(name) => Ok(name),
                Node::Null | Node::Text(_) | Node::Seq(_) | Node::Map(_) => {
                    Err(WorkflowError::Shape("an `on` item is not an event name"))
                }
            })
            .collect::<Result<_, _>>()?,
        Some(Node::Map(entries)) => entries.into_iter().map(|(name, _)| name).collect(),
        Some(Node::Null | Node::Text(_)) | None => Vec::new(),
    };
    if names.is_empty() {
        return Err(WorkflowError::Shape("no `on` triggers"));
    }
    Ok(names)
}

/// Appends the job's own `uses` and each step's.
fn job_uses(job: &mut Node, uses: &mut Vec<String>) -> Result<(), WorkflowError> {
    push_uses(job, uses)?;
    match job.remove("steps") {
        None | Some(Node::Null) => Ok(()),
        Some(Node::Seq(mut steps)) => steps.iter_mut().try_for_each(|step| match step {
            Node::Map(_) => push_uses(step, uses),
            Node::Null | Node::Scalar(_) | Node::Text(_) | Node::Seq(_) => {
                Err(WorkflowError::Shape("a step is not a mapping"))
            }
        }),
        Some(Node::Scalar(_) | Node::Text(_) | Node::Map(_)) => {
            Err(WorkflowError::Shape("`steps` is not a sequence"))
        }
    }
}

/// Appends `node`'s `uses`, when it has one.
fn push_uses(node: &mut Node, uses: &mut Vec<String>) -> Result<(), WorkflowError> {
    match node.remove("uses") {
        None => Ok(()),
        Some(Node::Scalar(target)) => {
            uses.push(target);
            Ok(())
        }
        Some(Node::Null | Node::Text(_) | Node::Seq(_) | Node::Map(_)) => {
            Err(WorkflowError::Shape("a `uses` is not a scalar"))
        }
    }
}

/// The reach `permissions` grants; `None` when it is not declared.
fn token(permissions: Option<&Node>) -> Result<Option<Token>, WorkflowError> {
    let unclassified = WorkflowError::Shape("`permissions` is not read, write or none");
    let Some(permissions) = permissions else {
        return Ok(None);
    };
    let reach = match permissions {
        Node::Scalar(all) if all == "read-all" => Token::ReadOnly,
        Node::Scalar(all) if all == "write-all" => Token::Write,
        Node::Map(scopes) => {
            let mut reach = Token::ReadOnly;
            for (_, access) in scopes {
                match access {
                    Node::Scalar(access) if access == "write" => reach = Token::Write,
                    Node::Scalar(access) if access == "read" || access == "none" => {}
                    Node::Null | Node::Scalar(_) | Node::Text(_) | Node::Seq(_) | Node::Map(_) => {
                        return Err(unclassified);
                    }
                }
            }
            reach
        }
        Node::Null | Node::Scalar(_) | Node::Text(_) | Node::Seq(_) => return Err(unclassified),
    };
    Ok(Some(reach))
}

/// What privileged thing `job` holds, given the workflow's `env` and the job's token.
fn holding(job: &Node, env_secret: bool, token: Option<Token>) -> Option<Holding> {
    if job.get("environment").is_some() {
        Some(Holding::Environment)
    } else if env_secret || job.mentions_ignoring_case(SECRETS) {
        Some(Holding::Secret)
    } else if token == Some(Token::ReadOnly) {
        None
    } else {
        Some(Holding::WriteToken)
    }
}

/// Whether `condition`, a job's `if`, is a top-level conjunction one of whose terms is exactly
/// [`GUARD`]. A condition with `||` at the top level, or with text outside its one `${{ }}`
/// (which GitHub reads as a non-empty, so true, string), is not guarded.
pub(crate) fn is_guarded(condition: &str) -> bool {
    let condition = condition.trim();
    let expression = match condition
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
    {
        Some(inner) => inner,
        None => condition,
    };
    if expression.contains("${{") || expression.contains("}}") {
        return false;
    }
    let Some(terms) = top_level_terms(expression) else {
        return false;
    };
    terms.iter().any(|term| term.trim() == GUARD)
}

/// The `&&` terms of `expression` outside parentheses and string literals, or `None` when
/// it has a top-level `||` or unbalanced parentheses or quotes.
fn top_level_terms(expression: &str) -> Option<Vec<&str>> {
    let mut terms = Vec::new();
    let (mut depth, mut quoted, mut start) = (0_usize, false, 0);
    let bytes = expression.as_bytes();
    for (at, &byte) in bytes.iter().enumerate() {
        let next = bytes.get(at.saturating_add(1)).copied();
        match byte {
            b'\'' => quoted = !quoted,
            _ if quoted => {}
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.checked_sub(1)?,
            b'|' if depth == 0 && next == Some(b'|') => return None,
            b'&' if depth == 0 && next == Some(b'&') => {
                terms.push(expression.get(start..at)?);
                start = at.saturating_add(2);
            }
            _ => {}
        }
    }
    if quoted || depth != 0 {
        return None;
    }
    terms.push(expression.get(start..)?);
    Some(terms)
}

/// Whether `uses` names its target immutably: a local path (`./`, versioned with the commit),
/// an image by `@sha256:` digest, or `owner/repo[/path]@<full commit sha>`.
pub(crate) fn is_pinned(uses: &str) -> bool {
    if uses.starts_with("./") {
        return true;
    }
    if let Some(image) = uses.strip_prefix("docker://") {
        return image
            .rsplit_once("@sha256:")
            .is_some_and(|(name, digest)| !name.is_empty() && is_lower_hex(digest, 64));
    }
    let Some((action, revision)) = uses.rsplit_once('@') else {
        return false;
    };
    let named = action.split('/').all(|segment| !segment.is_empty());
    let has_repo = action.split('/').nth(1).is_some();
    named && has_repo && is_lower_hex(revision, 40)
}

/// Whether `text` is `len` lowercase hexadecimal digits.
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests;
