//! The base P10 identity lists `assay status` reports the viewer against (#273, 03 §1):
//! `.assay/owners` and `.assay/agents` read at a base commit, absent files naming nobody, and
//! anything else refused rather than guessed at (wi-b5affa).

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_app::{
    BaseIdentitiesError, CommitId, Identities, IdentitiesConfigError, UserId, base_identities,
};

use crate::plan_fakes::{BASE, FakePr};

const OWNERS: &str = ".assay/owners";
const AGENTS: &str = ".assay/agents";

/// The base commit.
fn base() -> CommitId {
    BASE.parse().expect("a full commit id")
}

/// The ids `ids` as a set.
fn users(ids: &[u64]) -> BTreeSet<UserId> {
    ids.iter()
        .map(|&id| UserId(NonZeroU64::new(id).expect("test ids are not zero")))
        .collect()
}

/// The fake PR with `files` (path and text) written into its base.
fn with(files: &[(&str, &str)]) -> FakePr {
    let mut pr = FakePr::new();
    for (path, text) in files {
        pr.write(BASE, path, text.as_bytes());
    }
    pr
}

#[test]
fn a_base_without_the_lists_names_nobody() {
    let identities = base_identities(&FakePr::new(), &base());

    assert_eq!(identities.expect("read"), Identities::default());
}

#[test]
fn owners_and_approvers_are_privileged_and_agents_are_agents() {
    let pr = with(&[
        (
            OWNERS,
            r#"
owners = [{ id = 11, login = "break-glass" }]
approvers = [{ id = 12, login = "approver" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#,
        ),
        (
            AGENTS,
            r#"agents = [{ id = 124330161, login = "Ryan-Britton" }, { id = 13, login = "helper[bot]" }]"#,
        ),
    ]);

    let identities = base_identities(&pr, &base());

    assert_eq!(
        identities.expect("read"),
        Identities {
            privileged: users(&[11, 12]),
            agents: users(&[13, 124_330_161]),
        },
        "a maintainer is not privileged, and may be an agent (0005)"
    );
}

#[test]
fn a_list_may_be_left_out() {
    let pr = with(&[(OWNERS, "approvers = [{ id = 12, login = \"approver\" }]\n")]);

    let identities = base_identities(&pr, &base());

    assert_eq!(
        identities.expect("read"),
        Identities {
            privileged: users(&[12]),
            agents: BTreeSet::new(),
        }
    );
}

/// What `base_identities` says about a base holding `files`.
fn refusal(files: &[(&str, &str)]) -> BaseIdentitiesError<crate::plan_fakes::FakeError> {
    base_identities(&with(files), &base()).expect_err("refused")
}

#[test]
fn a_list_that_does_not_parse_exactly_is_refused_with_its_file() {
    let cases = [
        (OWNERS, "admins = []\n"),
        (OWNERS, "owners = [{ id = 11 }]\n"),
        (
            OWNERS,
            "owners = [{ id = 11, login = \"a\", name = \"A\" }]\n",
        ),
        (OWNERS, "owners = [{ id = -1, login = \"a\" }]\n"),
        (OWNERS, "owners = \"11\"\n"),
        (AGENTS, "owners = [{ id = 11, login = \"a\" }]\n"),
        (AGENTS, "agents = [11]\n"),
        (AGENTS, "not toml"),
    ];
    for (path, text) in cases {
        let refused = refusal(&[(path, text)]);

        assert!(
            matches!(
                &refused,
                BaseIdentitiesError::Config {
                    path: file,
                    source: IdentitiesConfigError::Syntax(_),
                } if *file == path
            ),
            "{path}: {text:?}: {refused:?}"
        );
    }
}

#[test]
fn a_zero_id_or_a_login_github_would_not_give_is_refused() {
    let cases = [
        ("owners = [{ id = 0, login = \"a\" }]\n", "0"),
        ("owners = [{ id = 11, login = \"\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"-a\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"a b\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"a[bot]x\" }]\n", "login"),
        (
            "owners = [{ id = 11, login = \"a1234567890123456789012345678901234567890\" }]\n",
            "login",
        ),
    ];
    for (text, what) in cases {
        let refused = refusal(&[(OWNERS, text)]);

        let expected = match &refused {
            BaseIdentitiesError::Config {
                source: IdentitiesConfigError::ZeroId,
                ..
            } => "0",
            BaseIdentitiesError::Config {
                source: IdentitiesConfigError::Login { .. },
                ..
            } => "login",
            _ => "something else",
        };
        assert_eq!(expected, what, "{text:?}: {refused:?}");
    }
}

#[test]
fn an_id_listed_twice_is_refused() {
    for text in [
        "owners = [{ id = 11, login = \"a\" }]\nmaintainers = [{ id = 11, login = \"a\" }]\n",
        "owners = [{ id = 11, login = \"a\" }]\napprovers = [{ id = 11, login = \"a\" }]\n",
        "owners = [{ id = 11, login = \"a\" }, { id = 11, login = \"b\" }]\n",
    ] {
        let refused = refusal(&[(OWNERS, text)]);

        assert!(
            matches!(
                refused,
                BaseIdentitiesError::Config {
                    path: OWNERS,
                    source: IdentitiesConfigError::Repeated { id: 11 },
                }
            ),
            "{text:?}: {refused:?}"
        );
    }
    let agents = refusal(&[(
        AGENTS,
        "agents = [{ id = 13, login = \"a\" }, { id = 13, login = \"a\" }]\n",
    )]);
    assert!(
        matches!(
            agents,
            BaseIdentitiesError::Config {
                path: AGENTS,
                source: IdentitiesConfigError::Repeated { id: 13 },
            }
        ),
        "{agents:?}"
    );
}

#[test]
fn an_agent_that_is_privileged_is_refused() {
    let refused = refusal(&[
        (OWNERS, "approvers = [{ id = 12, login = \"approver\" }]\n"),
        (AGENTS, "agents = [{ id = 12, login = \"approver\" }]\n"),
    ]);

    assert!(
        matches!(refused, BaseIdentitiesError::AgentPrivileged { id: 12 }),
        "{refused:?}"
    );
}

#[test]
fn a_list_that_is_not_a_regular_file_is_refused() {
    let mut symlink = FakePr::new();
    symlink.symlink(BASE, OWNERS, b"../elsewhere");
    let mut submodule = FakePr::new();
    submodule.submodule(BASE, AGENTS);
    let mut directory = FakePr::new();
    directory.write(BASE, ".assay/agents/list", b"agents = []\n");

    for (pr, path) in [(symlink, OWNERS), (submodule, AGENTS), (directory, AGENTS)] {
        let refused = base_identities(&pr, &base());

        assert!(
            matches!(
                refused,
                Err(BaseIdentitiesError::NotARegularFile { path: file }) if file == path
            ),
            "{path}: {refused:?}"
        );
    }
}

#[test]
fn a_list_over_the_cap_is_refused_unread_and_one_at_it_is_read() {
    let text = "approvers = [{ id = 12, login = \"approver\" }]\n";
    let mut at = FakePr::new();
    at.write_listed_as((BASE, OWNERS), text.as_bytes(), 65_536);
    let mut over = FakePr::new();
    over.write_listed_as((BASE, OWNERS), text.as_bytes(), 65_537);
    over.failing_reads = true;

    let read = base_identities(&at, &base());
    let refused = base_identities(&over, &base());

    assert_eq!(
        read.expect("a list at the cap is read"),
        Identities {
            privileged: users(&[12]),
            agents: BTreeSet::new(),
        }
    );
    assert!(
        matches!(
            refused,
            Err(BaseIdentitiesError::TooLarge {
                path: OWNERS,
                size: 65_537,
            })
        ),
        "{refused:?}"
    );
}

#[test]
fn a_list_that_is_not_utf8_is_refused() {
    let mut pr = FakePr::new();
    pr.write(BASE, AGENTS, b"agents = [\xff]\n");

    let refused = base_identities(&pr, &base());

    assert!(
        matches!(refused, Err(BaseIdentitiesError::NotUtf8 { path: AGENTS })),
        "{refused:?}"
    );
}

#[test]
fn a_base_that_cannot_be_read_is_an_objects_error() {
    let mut pr = FakePr::new();
    pr.forget(BASE);

    let refused = base_identities(&pr, &base());

    assert!(
        matches!(refused, Err(BaseIdentitiesError::Objects(_))),
        "{refused:?}"
    );
}
