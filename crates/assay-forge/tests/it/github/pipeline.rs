//! The pipeline's reads (#60 ST-3b) over recorded responses: a PR's identity, the workflow
//! run's `created_at`, whether a push was forced, and the PR's REST file list.
//! `pull_files_pr.json` and `pull_files.json` are PR 317 and its files, `compare_*.json` the
//! four statuses of `compare/<before>...<after>?per_page=1` between commits of this repository,
//! recorded with `gh api` and trimmed (URLs, patches, commit and file lists).

use std::num::NonZeroU64;

use assay_app::{CommitId, RestFile, RestFiles};
use assay_forge::github::{ForgeError, MAX_LIST_ITEMS, Method, Operation, PullIdentity, PullState};
use serde_json::json;

use super::recorded::{
    Recorded, Unscripted, Waits, fixture, fixture_json, github, id, json, owner, path, reply,
};

/// The PR the file list was recorded from.
const FILES_PR: u64 = 317;
/// This repository's id.
const REPO_ID: u64 = 1_413_773_798;
/// The base of PR 317, an ancestor of [`MAIN`].
const BASE: &str = "94ea66af94f631022ef97bc57375e47fe5a0e0ae";
/// `main` five commits after [`BASE`].
const MAIN: &str = "61f2abc72590701fd7809d0e31981dcfa56bb491";
/// The tip of a branch that forked from `main` before [`MAIN`] and gained a commit of its own.
const FORK: &str = "f5362695c702c25ee3384565d8f792de15142426";

/// A change to a recorded body, for a table of refusals.
type Edit = fn(&mut serde_json::Value);

fn sha(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

fn files_pr() -> NonZeroU64 {
    id(FILES_PR)
}

fn pull_path() -> String {
    path(&format!("pulls/{FILES_PR}"))
}

fn files_path(page: usize) -> String {
    if page == 1 {
        path(&format!("pulls/{FILES_PR}/files?per_page=100"))
    } else {
        path(&format!("pulls/{FILES_PR}/files?per_page=100&page={page}"))
    }
}

fn next_link(page: usize) -> String {
    format!(
        "<https://api.github.com/{}>; rel=\"next\"",
        files_path(page + 1)
    )
}

fn malformed<V>(read: &Result<V, ForgeError<Unscripted>>, operation: Operation) -> Option<&str> {
    match read {
        Err(ForgeError::Malformed {
            operation: found,
            detail,
        }) if *found == operation => Some(detail),
        _ => None,
    }
}

/// The recorded PR with `changed_files` set, then `pages` as its file list.
fn listing(changed_files: u64, pages: &[serde_json::Value]) -> Recorded {
    let mut pull = fixture_json("pull_files_pr.json");
    pull["changed_files"] = json!(changed_files);
    let recorded = Recorded::default().ok(Method::Get, &pull_path(), json(&pull));
    let last = pages.len();
    pages
        .iter()
        .enumerate()
        .fold(recorded, |recorded, (index, page)| {
            let number = index + 1;
            let link = next_link(number);
            let headers: &[(&str, &str)] = if number == last {
                &[]
            } else {
                &[("Link", &link)]
            };
            recorded.then(
                Method::Get,
                &files_path(number),
                reply(200, headers, json(page)),
            )
        })
}

/// The recorded file list with `edit` applied to its first file.
fn edited_first(edit: impl FnOnce(&mut serde_json::Value)) -> Recorded {
    let mut files = fixture_json("pull_files.json");
    edit(&mut files[0]);
    listing(16, &[files])
}

/// `count` distinct modified files, as pages of 100.
fn generated_pages(count: usize) -> Vec<serde_json::Value> {
    let files: Vec<_> = (0..count)
        .map(|index| json!({ "filename": format!("src/f{index}.rs"), "status": "modified" }))
        .collect();
    files.chunks(100).map(|page| json!(page)).collect()
}

/// PR 317's recorded files, none renamed, as the adapter must return them.
fn recorded_files() -> RestFiles {
    let files = fixture_json("pull_files.json")
        .as_array()
        .expect("an array")
        .iter()
        .map(|file| RestFile {
            path: file["filename"].as_str().expect("a name").to_owned(),
            previous_path: None,
        })
        .collect();
    RestFiles {
        files,
        changed_files: 16,
    }
}

#[test]
fn the_files_are_listed_with_the_prs_changed_files() {
    let recorded = Recorded::default()
        .ok(Method::Get, &pull_path(), fixture("pull_files_pr.json"))
        .ok(Method::Get, &files_path(1), fixture("pull_files.json"));
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert_eq!(listed.expect("listed"), recorded_files());
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn every_page_of_the_file_list_is_read() {
    let files = fixture_json("pull_files.json");
    let (first, second) = files
        .as_array()
        .expect("an array")
        .split_at_checked(10)
        .expect("16 files");
    let recorded = listing(16, &[json!(first), json!(second)]);
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert_eq!(listed.expect("listed"), recorded_files());
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn a_renamed_file_carries_its_previous_path() {
    let recorded = edited_first(|file| {
        file["status"] = json!("renamed");
        file["previous_filename"] = json!("crates/assay-app/CHARTER.md");
    });
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert_eq!(
        listed.expect("listed").files.first(),
        Some(&RestFile {
            path: "crates/assay-app/AGENTS.md".to_owned(),
            previous_path: Some("crates/assay-app/CHARTER.md".to_owned()),
        })
    );
}

#[test]
fn a_list_of_exactly_the_cap_is_kept_whole() {
    let recorded = listing(3_500, &generated_pages(MAX_LIST_ITEMS));
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    let listed = listed.expect("listed");
    assert_eq!(
        (listed.files.len(), listed.changed_files),
        (MAX_LIST_ITEMS, 3_500)
    );
}

#[test]
fn a_file_past_the_cap_is_refused() {
    let recorded = listing(3_500, &generated_pages(MAX_LIST_ITEMS + 1));
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert!(
        matches!(
            listed,
            Err(ForgeError::TooMany {
                operation: Operation::ListPullFiles,
                cap: MAX_LIST_ITEMS
            })
        ),
        "{listed:?}"
    );
}

#[test]
fn a_path_listed_twice_is_refused() {
    let mut files = fixture_json("pull_files.json");
    // The duplicate is the point: two entries name one path.
    files[15]["filename"] = files[3]["filename"].clone();
    let recorded = listing(16, &[files]);
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert_eq!(
        malformed(&listed, Operation::ListPullFiles),
        Some("a file is listed twice")
    );
}

#[test]
fn a_malformed_previous_filename_is_refused() {
    let cases = [
        (
            "modified",
            json!("old.rs"),
            "only a renamed or copied file has a previous_filename",
        ),
        (
            "renamed",
            json!(null),
            "a renamed or copied file has no previous_filename",
        ),
        (
            "copied",
            json!(null),
            "a renamed or copied file has no previous_filename",
        ),
        ("renamed", json!(""), "a previous_filename is empty"),
        (
            "renamed",
            json!("crates/assay-app/AGENTS.md"),
            "a previous_filename is the filename",
        ),
    ];
    for (status, previous, expected) in cases {
        let case = format!("{status} with {previous}");
        let recorded = edited_first(move |file| {
            file["status"] = json!(status);
            file["previous_filename"] = previous;
        });
        let waits = Waits::default();

        let listed = github(&recorded, &waits).pull_files(files_pr());

        assert_eq!(
            malformed(&listed, Operation::ListPullFiles),
            Some(expected),
            "{case}"
        );
    }
}

#[test]
fn an_empty_filename_or_unknown_status_is_refused() {
    let cases = [
        ("filename", json!(""), "a file has no filename"),
        (
            "status",
            json!("moved"),
            "a file's status is not one GitHub documents",
        ),
    ];
    for (field, value, expected) in cases {
        let recorded = edited_first(move |file| file[field] = value);
        let waits = Waits::default();

        let listed = github(&recorded, &waits).pull_files(files_pr());

        assert_eq!(
            malformed(&listed, Operation::ListPullFiles),
            Some(expected),
            "{field}"
        );
    }
}

#[test]
fn a_file_list_for_another_pr_is_refused_before_listing() {
    let mut pull = fixture_json("pull_files_pr.json");
    pull["number"] = json!(316);
    let recorded = Recorded::default().ok(Method::Get, &pull_path(), json(&pull));
    let waits = Waits::default();

    let listed = github(&recorded, &waits).pull_files(files_pr());

    assert_eq!(
        malformed(&listed, Operation::ReadPull),
        Some("GitHub returned another PR")
    );
    assert_eq!(recorded.sent().len(), 1);
}

#[test]
fn the_runs_created_at_is_a_ledger_time() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("actions/runs/39168909542"),
        fixture("run.json"),
    );
    let waits = Waits::default();

    let created = github(&recorded, &waits).run_created_at(id(39_168_909_542));

    assert_eq!(
        created.expect("read"),
        "2026-10-18T21:40:53.000Z".parse().expect("a ledger time")
    );
}

#[test]
fn another_runs_answer_or_time_form_is_refused() {
    let cases = [
        (
            "id",
            json!(39_168_909_543_u64),
            "GitHub returned another run",
        ),
        (
            "created_at",
            json!("2026-10-18T22:40:53+01:00"),
            "a time is not YYYY-MM-DDTHH:MM:SSZ",
        ),
        (
            "created_at",
            json!("2026-10-18T21:40:53.5Z"),
            "a time is not YYYY-MM-DDTHH:MM:SSZ",
        ),
    ];
    for (field, value, expected) in cases {
        let mut run = fixture_json("run.json");
        run[field] = value;
        let recorded =
            Recorded::default().ok(Method::Get, &path("actions/runs/39168909542"), json(&run));
        let waits = Waits::default();

        let created = github(&recorded, &waits).run_created_at(id(39_168_909_542));

        assert_eq!(
            malformed(&created, Operation::ListRuns),
            Some(expected),
            "{field}"
        );
    }
}

fn compare_path(before: &str, after: &str) -> String {
    path(&format!("compare/{before}...{after}?per_page=1"))
}

#[test]
fn a_push_is_forced_when_after_does_not_contain_before() {
    let cases = [
        ("compare_ahead.json", BASE, MAIN, false),
        ("compare_identical.json", MAIN, MAIN, false),
        ("compare_behind.json", MAIN, BASE, true),
        ("compare_diverged.json", FORK, MAIN, true),
    ];
    for (name, before, after, expected) in cases {
        let recorded =
            Recorded::default().ok(Method::Get, &compare_path(before, after), fixture(name));
        let waits = Waits::default();

        let forced = github(&recorded, &waits).forced(&sha(before), &sha(after));

        assert_eq!(forced.expect("compared"), expected, "{name}");
    }
}

#[test]
fn a_comparison_that_does_not_add_up_is_refused() {
    let cases: [(Edit, &str); 2] = [
        (
            |compare| compare["status"] = json!("unrelated"),
            "a comparison's status is not one GitHub documents",
        ),
        (
            |compare| compare["base_commit"]["sha"] = json!(MAIN),
            "GitHub compared another base",
        ),
    ];
    for (edit, expected) in cases {
        let mut compare = fixture_json("compare_ahead.json");
        edit(&mut compare);
        let recorded =
            Recorded::default().ok(Method::Get, &compare_path(BASE, MAIN), json(&compare));
        let waits = Waits::default();

        let forced = github(&recorded, &waits).forced(&sha(BASE), &sha(MAIN));

        assert_eq!(
            malformed(&forced, Operation::CompareCommits),
            Some(expected),
            "{expected}"
        );
    }
}

#[test]
fn a_fast_forward_that_does_not_add_up_is_refused() {
    let comparisons = [
        ("compare_ahead.json", BASE, MAIN),
        ("compare_identical.json", MAIN, MAIN),
    ];
    let cases: [(Edit, &str); 2] = [
        (
            |compare| compare["behind_by"] = json!(1),
            "a fast-forward comparison is behind its base",
        ),
        (
            |compare| compare["merge_base_commit"]["sha"] = json!(FORK),
            "a fast-forward comparison's merge base is not its base",
        ),
    ];
    for (name, before, after) in comparisons {
        for (edit, expected) in cases {
            let mut compare = fixture_json(name);
            edit(&mut compare);
            let recorded =
                Recorded::default().ok(Method::Get, &compare_path(before, after), json(&compare));
            let waits = Waits::default();

            let forced = github(&recorded, &waits).forced(&sha(before), &sha(after));

            assert_eq!(
                malformed(&forced, Operation::CompareCommits),
                Some(expected),
                "{name}: {expected}"
            );
        }
    }
}

#[test]
fn a_pull_identity_is_read_whole() {
    let recorded = Recorded::default().ok(Method::Get, &pull_path(), fixture("pull_files_pr.json"));
    let waits = Waits::default();

    let identity = github(&recorded, &waits).pull_identity(files_pr());

    assert_eq!(
        identity.expect("read"),
        PullIdentity {
            number: files_pr(),
            head_sha: sha("f5c3c78d3ef0bef8fa9eca4066027e06648ff6e5"),
            base_sha: sha(BASE),
            base_ref: "main".to_owned(),
            draft: false,
            author: owner(),
            head_repo: Some(id(REPO_ID)),
            state: PullState::Closed,
        }
    );
}

#[test]
fn an_open_draft_from_a_deleted_fork_has_no_head_repository() {
    let mut pull = fixture_json("pull_files_pr.json");
    pull["state"] = json!("open");
    pull["draft"] = json!(true);
    pull["head"]["repo"] = json!(null);
    let recorded = Recorded::default().ok(Method::Get, &pull_path(), json(&pull));
    let waits = Waits::default();

    let identity = github(&recorded, &waits)
        .pull_identity(files_pr())
        .expect("read");

    assert_eq!(
        (identity.state, identity.draft, identity.head_repo),
        (PullState::Open, true, None)
    );
}

#[test]
fn a_pull_identity_that_cannot_be_read_exactly_is_refused() {
    let cases: [(Edit, &str); 5] = [
        (
            |pull| pull["base"]["ref"] = json!(""),
            "a PR's base branch is empty",
        ),
        (
            |pull| pull["number"] = json!(316),
            "GitHub returned another PR",
        ),
        (
            |pull| pull["state"] = json!("merged"),
            "a PR is neither open nor closed",
        ),
        (
            |pull| pull["head"]["repo"]["id"] = json!(0),
            "a repository id is zero",
        ),
        (|pull| pull["user"] = json!(null), "a PR has no author"),
    ];
    for (edit, expected) in cases {
        let mut pull = fixture_json("pull_files_pr.json");
        edit(&mut pull);
        let recorded = Recorded::default().ok(Method::Get, &pull_path(), json(&pull));
        let waits = Waits::default();

        let identity = github(&recorded, &waits).pull_identity(files_pr());

        assert_eq!(
            malformed(&identity, Operation::ReadPull),
            Some(expected),
            "{expected}"
        );
    }
}
