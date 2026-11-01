//! The base P10 identity lists `assay status` reports the viewer against (#273, 03 §1):
//! `.assay/owners` and `.assay/agents` read at a base commit, absent files naming nobody, and
//! anything else refused rather than guessed at (wi-b5affa). The files are parsed by
//! `EligibilityLists::parse` (#305), whose own rules `eligibility_lists.rs` pins; these tests pin
//! that `base_identities` refuses at least what it refused with its former parser.

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_app::{
    BaseIdentitiesError, CommitId, EligibilityLists, Identities, ListFile, ListName, ListsError,
    UserId, base_identities, base_lists,
};

use crate::plan_fakes::{BASE, FakePr};

const OWNERS: &str = ".assay/owners";
const AGENTS: &str = ".assay/agents";
/// The lists `.assay/owners` must hold besides `owners`.
const OTHER_OWNERS_LISTS: &str = "approvers = []\nmaintainers = []\n";

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
            r#"agents = [{ id = 124330161, login = "Ryan-Britton" }, { id = 13, login = "helper" }]"#,
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

/// #60 ST-14: decide's GitHub state snapshot judges eligibility by the same reading of the
/// base lists `status` reports: the lists `EligibilityLists::parse` gives for the base files.
#[test]
fn the_base_lists_are_the_files_as_the_one_parser_reads_them() {
    let owners = "owners = [{ id = 11, login = \"break-glass\" }]\n\
                  approvers = []\nmaintainers = [{ id = 7, login = \"daily\" }]\n";
    let agents = "agents = [{ id = 13, login = \"helper\" }]";
    let pr = with(&[(OWNERS, owners), (AGENTS, agents)]);

    let lists = base_lists(&pr, &base());

    assert_eq!(
        lists.expect("read"),
        EligibilityLists::parse(Some(owners), Some(agents)).expect("valid lists")
    );
    assert_eq!(
        base_lists(&FakePr::new(), &base()).expect("read"),
        EligibilityLists::default()
    );
}

/// #305: the former parser read a missing list as empty; the one parser requires every list of
/// a present file, the stricter rule, so an empty file is refused too.
#[test]
fn a_list_left_out_of_a_present_file_is_refused() {
    for (path, text, file) in [
        (
            OWNERS,
            "approvers = [{ id = 12, login = \"approver\" }]\n",
            ListFile::Owners,
        ),
        (OWNERS, "", ListFile::Owners),
        (AGENTS, "", ListFile::Agents),
    ] {
        let refused = refusal(&[(path, text)]);

        assert!(
            matches!(
                &refused,
                BaseIdentitiesError::Lists(ListsError::Toml { file: refused, .. }) if *refused == file
            ),
            "{path}: {text:?}: {refused:?}"
        );
    }
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
        // Every other list is present, so each owners case is refused for its own defect.
        let (file, text) = if path == OWNERS {
            (ListFile::Owners, format!("{text}{OTHER_OWNERS_LISTS}"))
        } else {
            (ListFile::Agents, text.to_owned())
        };
        let refused = refusal(&[(path, &text)]);

        assert!(
            matches!(
                &refused,
                BaseIdentitiesError::Lists(ListsError::Toml { file: refused, .. }) if *refused == file
            ),
            "{path}: {text:?}: {refused:?}"
        );
    }
}

/// A zero id is refused by the id's type (`Toml`), a bad login by the login rule; #305 adds the
/// `[bot]` suffix the former parser accepted.
#[test]
fn a_zero_id_or_a_login_github_would_not_give_is_refused() {
    let cases = [
        ("owners = [{ id = 0, login = \"a\" }]\n", "0"),
        ("owners = [{ id = 11, login = \"\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"-a\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"a b\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"a[bot]x\" }]\n", "login"),
        ("owners = [{ id = 11, login = \"helper[bot]\" }]\n", "login"),
        (
            "owners = [{ id = 11, login = \"a1234567890123456789012345678901234567890\" }]\n",
            "login",
        ),
    ];
    for (text, what) in cases {
        let refused = refusal(&[(OWNERS, &format!("{text}{OTHER_OWNERS_LISTS}"))]);

        let expected = match &refused {
            BaseIdentitiesError::Lists(ListsError::Toml {
                file: ListFile::Owners,
                ..
            }) => "0",
            BaseIdentitiesError::Lists(ListsError::Login {
                list: ListName::Owners,
                id,
            }) if id.0.get() == 11 => "login",
            _ => "something else",
        };
        assert_eq!(expected, what, "{text:?}: {refused:?}");
    }
}

/// What an id listed twice is refused as.
#[derive(Debug, PartialEq, Eq)]
enum Twice {
    MaintainerPrivileged(u64),
    OwnerApprover(u64),
    Repeated(ListName, u64),
}

/// An id twice in one file, in one list or two, is refused; #305 names each case.
#[test]
fn an_id_listed_twice_is_refused() {
    let twice = |refused: &BaseIdentitiesError<_>| match refused {
        BaseIdentitiesError::Lists(ListsError::MaintainerPrivileged { id }) => {
            Some(Twice::MaintainerPrivileged(id.0.get()))
        }
        BaseIdentitiesError::Lists(ListsError::OwnerApprover { id }) => {
            Some(Twice::OwnerApprover(id.0.get()))
        }
        BaseIdentitiesError::Lists(ListsError::Repeated { list, id }) => {
            Some(Twice::Repeated(*list, id.0.get()))
        }
        _ => None,
    };
    let cases = [
        (
            OWNERS,
            "owners = [{ id = 11, login = \"a\" }]\napprovers = []\nmaintainers = [{ id = 11, login = \"a\" }]\n",
            Twice::MaintainerPrivileged(11),
        ),
        (
            OWNERS,
            "owners = [{ id = 11, login = \"a\" }]\napprovers = [{ id = 11, login = \"a\" }]\nmaintainers = []\n",
            Twice::OwnerApprover(11),
        ),
        (
            OWNERS,
            "owners = [{ id = 11, login = \"a\" }, { id = 11, login = \"b\" }]\napprovers = []\nmaintainers = []\n",
            Twice::Repeated(ListName::Owners, 11),
        ),
        (
            AGENTS,
            "agents = [{ id = 13, login = \"a\" }, { id = 13, login = \"a\" }]\n",
            Twice::Repeated(ListName::Agents, 13),
        ),
    ];
    for (path, text, expected) in cases {
        let refused = refusal(&[(path, text)]);

        assert_eq!(twice(&refused), Some(expected), "{text:?}: {refused:?}");
    }
}

#[test]
fn an_agent_that_is_privileged_is_refused() {
    let refused = refusal(&[
        (
            OWNERS,
            "owners = []\napprovers = [{ id = 12, login = \"approver\" }]\nmaintainers = []\n",
        ),
        (AGENTS, "agents = [{ id = 12, login = \"approver\" }]\n"),
    ]);

    assert!(
        matches!(
            &refused,
            BaseIdentitiesError::Lists(ListsError::AgentPrivileged { id }) if id.0.get() == 12
        ),
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
fn a_config_directory_that_is_a_symlink_or_submodule_is_refused() {
    let mut symlink = FakePr::new();
    symlink.remove(BASE, ".assay/plugins.toml");
    symlink.symlink(BASE, ".assay", b"elsewhere");
    let mut submodule = FakePr::new();
    submodule.remove(BASE, ".assay/plugins.toml");
    submodule.submodule(BASE, ".assay");

    for pr in [symlink, submodule] {
        let refused = base_identities(&pr, &base());

        assert!(
            matches!(
                refused,
                Err(BaseIdentitiesError::NotADirectory { path: ".assay" })
            ),
            "{refused:?}"
        );
    }
}

#[test]
fn a_list_over_the_cap_is_refused_unread_and_one_at_it_is_read() {
    let text = "owners = []\napprovers = [{ id = 12, login = \"approver\" }]\nmaintainers = []\n";
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
