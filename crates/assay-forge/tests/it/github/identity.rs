//! Commit authors and committers resolved to numeric user ids through GraphQL; anything but an
//! exact answer to what was asked is refused.

use assay_app::{Actor, CommitId, CommitIdentity, Forge, UserId};
use assay_forge::github::{ForgeError, MAX_COMMITS, Method};

use super::recorded::{HEAD, Recorded, Waits, fixture, github, head, id, parent};

/// The commits the fixtures resolve.
fn commits() -> [CommitId; 2] {
    [head(), parent()]
}

/// User `number` as an actor.
fn user(number: u64) -> Actor {
    Actor::User(UserId(id(number)))
}

#[test]
fn authors_and_committers_resolve_to_user_ids() {
    let recorded =
        Recorded::default().ok(Method::Post, "graphql", fixture("graphql_identities.json"));
    let waits = Waits::default();
    let [head, parent] = commits();

    let identities = github(&recorded, &waits).commit_identities(&commits());

    assert_eq!(
        identities.expect("resolved"),
        [
            CommitIdentity {
                commit: head,
                authors: vec![user(124_330_161), Actor::NotAUser],
                committer: user(19_864_447),
            },
            CommitIdentity {
                commit: parent,
                authors: vec![Actor::NotAUser],
                committer: Actor::NotAUser,
            },
        ]
    );
}

#[test]
fn the_query_asks_for_each_commit_by_variable() {
    let recorded =
        Recorded::default().ok(Method::Post, "graphql", fixture("graphql_identities.json"));
    let waits = Waits::default();

    github(&recorded, &waits)
        .commit_identities(&commits())
        .expect("resolved");

    let body = recorded
        .sent()
        .into_iter()
        .next()
        .and_then(|sent| sent.body)
        .expect("a query was sent");
    assert_eq!(
        body["variables"],
        serde_json::json!({
            "owner": "Wet-Ink-Corporation",
            "name": "assay",
            "c0": HEAD,
            "c1": "ca42d86e668fbb18dab3ea4255fcd09518412f3b",
        })
    );
    let query = body["query"].as_str().expect("a query string");
    assert!(
        query.contains("c0: object(oid: $c0)") && query.contains("c1: object(oid: $c1)"),
        "{query}"
    );
}

#[test]
fn a_commit_github_does_not_know_is_refused() {
    let recorded = Recorded::default().ok(
        Method::Post,
        "graphql",
        fixture("graphql_unknown_commit.json"),
    );
    let waits = Waits::default();
    let [head, _] = commits();

    let identities = github(&recorded, &waits).commit_identities(&[head]);

    assert!(
        matches!(identities, Err(ForgeError::Malformed { detail, .. }) if detail == "GitHub does not know a commit"),
        "{identities:?}"
    );
}

#[test]
fn graphql_errors_are_refused() {
    let recorded = Recorded::default().ok(Method::Post, "graphql", fixture("graphql_errors.json"));
    let waits = Waits::default();
    let [head, _] = commits();

    let identities = github(&recorded, &waits).commit_identities(&[head]);

    assert!(
        matches!(identities, Err(ForgeError::Malformed { detail, .. }) if detail == "GraphQL returned errors"),
        "{identities:?}"
    );
}

#[test]
fn an_answer_for_other_commits_is_refused() {
    let recorded =
        Recorded::default().ok(Method::Post, "graphql", fixture("graphql_identities.json"));
    let waits = Waits::default();
    let [head, _] = commits();

    let identities = github(&recorded, &waits).commit_identities(&[head]);

    assert!(
        matches!(identities, Err(ForgeError::Malformed { detail, .. }) if detail == "GraphQL answered other commits than asked"),
        "{identities:?}"
    );
}

#[test]
fn more_authors_than_one_page_are_refused() {
    let recorded = Recorded::default().ok(
        Method::Post,
        "graphql",
        fixture("graphql_many_authors.json"),
    );
    let waits = Waits::default();
    let [head, _] = commits();

    let identities = github(&recorded, &waits).commit_identities(&[head]);

    assert!(
        matches!(identities, Err(ForgeError::TooMany { cap: 100, .. })),
        "{identities:?}"
    );
}

#[test]
fn more_commits_than_the_split_bound_are_refused_unasked() {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let [head, _] = commits();
    let range = vec![head; MAX_COMMITS + 1];

    let identities = github(&recorded, &waits).commit_identities(&range);

    assert!(
        matches!(identities, Err(ForgeError::TooMany { cap, .. }) if cap == MAX_COMMITS),
        "{identities:?}"
    );
    assert_eq!(recorded.sent(), Vec::new(), "nothing sent");
}

#[test]
fn no_commits_need_no_query() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    let identities = github(&recorded, &waits).commit_identities(&[]);

    assert_eq!(identities.expect("nothing to resolve"), Vec::new());
    assert_eq!(recorded.sent(), Vec::new(), "nothing sent");
}
