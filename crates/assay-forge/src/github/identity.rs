//! Commit authors (co-authors included) and committers resolved to numeric GitHub user ids
//! through GraphQL, for the pusher set (02 §4, 04 §5).

use std::collections::HashMap;

use assay_app::{Actor, CommitId, CommitIdentity, UserId};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::api::{decode, encode};
use super::dto;
use super::error::{ForgeError, Operation};
use super::transport::{Request, Sleep, Transport};
use super::{GitHub, api_path};

/// The most commits resolved in one call: the spec's `needs-split` bound (02 §4).
pub const MAX_COMMITS: usize = 250;
/// Commits per GraphQL query.
const BATCH: usize = 50;
/// The most authors read per commit (GraphQL's page maximum); a commit with more is refused.
const MAX_AUTHORS: usize = 100;

/// A GraphQL response.
#[derive(Debug, Deserialize)]
struct GraphResponse {
    data: Option<GraphData>,
    errors: Option<Vec<Value>>,
}

/// `data`.
#[derive(Debug, Deserialize)]
struct GraphData {
    repository: Option<HashMap<String, Option<CommitNode>>>,
}

/// One aliased `object(oid:)` as a commit.
#[derive(Debug, Deserialize)]
struct CommitNode {
    oid: String,
    authors: Authors,
    committer: Option<GitActor>,
}

/// `authors(first:)`.
#[derive(Debug, Deserialize)]
struct Authors {
    #[serde(rename = "totalCount")]
    total_count: u64,
    nodes: Vec<GitActor>,
}

/// A `GitActor`, whose `user` is `null` when no GitHub user is linked.
#[derive(Debug, Deserialize)]
struct GitActor {
    user: Option<GraphUser>,
}

/// A linked user.
#[derive(Debug, Deserialize)]
struct GraphUser {
    #[serde(rename = "databaseId")]
    database_id: Option<u64>,
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// The authors and committer of each of `commits`, in order.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TooMany`] past [`MAX_COMMITS`] commits or [`MAX_AUTHORS`] authors on one;
    /// [`ForgeError::Malformed`] for GraphQL errors, a commit GitHub does not know, or a
    /// response that does not answer exactly what was asked.
    pub(crate) fn resolve_commits(
        &self,
        commits: &[CommitId],
    ) -> Result<Vec<CommitIdentity>, ForgeError<T::Error>> {
        let operation = Operation::ResolveCommits;
        if commits.len() > MAX_COMMITS {
            return Err(ForgeError::TooMany {
                operation,
                cap: MAX_COMMITS,
            });
        }
        let path = api_path(operation, "graphql".to_owned())?;
        let mut identities = Vec::with_capacity(commits.len());
        for batch in commits.chunks(BATCH) {
            let body = encode(operation, &self.query(batch))?;
            let response = self.api.send(operation, Request::post(&path, &body))?;
            let parsed: GraphResponse = decode(operation, &response.body)?;
            identities.extend(answers(operation, batch, parsed)?);
        }
        Ok(identities)
    }

    /// The query and variables resolving `batch`, each commit under alias `c<index>`.
    fn query(&self, batch: &[CommitId]) -> Value {
        let mut declarations = String::new();
        let mut selections = String::new();
        let mut variables = Map::new();
        variables.insert("owner".to_owned(), Value::from(self.repository.owner()));
        variables.insert("name".to_owned(), Value::from(self.repository.name()));
        for (index, commit) in batch.iter().enumerate() {
            let alias = format!("c{index}");
            declarations.push_str(", $");
            declarations.push_str(&alias);
            declarations.push_str(": GitObjectID!");
            selections.push(' ');
            selections.push_str(&alias);
            selections.push_str(": object(oid: $");
            selections.push_str(&alias);
            selections.push_str(") { ...C }");
            variables.insert(alias, Value::from(commit.as_str()));
        }
        let query = format!(
            "query($owner: String!, $name: String!{declarations}) {{ repository(owner: $owner, name: $name) {{{selections} }} }} \
             fragment C on Commit {{ oid authors(first: {MAX_AUTHORS}) {{ totalCount nodes {{ user {{ databaseId }} }} }} \
             committer {{ user {{ databaseId }} }} }}"
        );
        let mut body = Map::new();
        body.insert("query".to_owned(), Value::from(query));
        body.insert("variables".to_owned(), Value::Object(variables));
        Value::Object(body)
    }
}

/// The identities `response` gives for `batch`, refusing anything but one commit per alias.
fn answers<E>(
    operation: Operation,
    batch: &[CommitId],
    response: GraphResponse,
) -> Result<Vec<CommitIdentity>, ForgeError<E>> {
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    if response.errors.is_some_and(|errors| !errors.is_empty()) {
        return Err(malformed("GraphQL returned errors"));
    }
    let mut nodes = response
        .data
        .and_then(|data| data.repository)
        .ok_or_else(|| malformed("GraphQL returned no repository"))?;
    if nodes.len() != batch.len() {
        return Err(malformed("GraphQL answered other commits than asked"));
    }
    batch
        .iter()
        .enumerate()
        .map(|(index, commit)| {
            let node = nodes
                .remove(&format!("c{index}"))
                .ok_or_else(|| malformed("GraphQL answered other commits than asked"))?
                .ok_or_else(|| malformed("GitHub does not know a commit"))?;
            identity(operation, commit, &node)
        })
        .collect()
}

/// One commit's identity.
fn identity<E>(
    operation: Operation,
    commit: &CommitId,
    node: &CommitNode,
) -> Result<CommitIdentity, ForgeError<E>> {
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    if node.oid != commit.as_str() {
        return Err(malformed("GraphQL answered another commit"));
    }
    let total = usize::try_from(node.authors.total_count).unwrap_or(usize::MAX);
    if total > MAX_AUTHORS {
        return Err(ForgeError::TooMany {
            operation,
            cap: MAX_AUTHORS,
        });
    }
    if node.authors.nodes.len() != total {
        return Err(malformed("a commit's author list is incomplete"));
    }
    let authors = node
        .authors
        .nodes
        .iter()
        .map(|author| actor(operation, author))
        .collect::<Result<Vec<_>, _>>()?;
    let committer = node
        .committer
        .as_ref()
        .ok_or_else(|| malformed("a commit has no committer"))?;
    Ok(CommitIdentity {
        // The caller keeps its list; the identity owns its own copy of the id.
        commit: commit.clone(),
        authors,
        committer: actor(operation, committer)?,
    })
}

/// A git actor as a user id, or [`Actor::NotAUser`] when GitHub links none.
fn actor<E>(operation: Operation, actor: &GitActor) -> Result<Actor, ForgeError<E>> {
    match &actor.user {
        None => Ok(Actor::NotAUser),
        Some(user) => {
            let id = user.database_id.ok_or(ForgeError::Malformed {
                operation,
                detail: "a linked user has no id",
            })?;
            dto::nonzero(operation, id, "a user id is zero").map(|id| Actor::User(UserId(id)))
        }
    }
}
