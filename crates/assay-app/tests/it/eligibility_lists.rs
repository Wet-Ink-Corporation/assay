//! Base `.assay/owners` and `.assay/agents` (03 §1, 0005) and the timestamps the eligibility rule
//! compares (04 §5): what parses, what is refused, and decide-only's narrowing (R3-26).

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use assay_app::{
    EligibilityLists, Identity, InvalidTimestamp, ListFile, ListName, ListsError, Timestamp,
};

use crate::eligibility_fakes::user;

const OWNERS: &str = r#"
owners = [{ id = 1001, login = "break-glass" }]
approvers = [{ id = 1002, login = "approver" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#;

const AGENTS: &str = r#"
agents = [{ id = 124330161, login = "Ryan-Britton" }]
"#;

fn ids(list: &[Identity]) -> Vec<u64> {
    list.iter().map(|identity| identity.id.0.get()).collect()
}

#[test]
fn the_four_lists_parse_with_ids_and_logins() -> Result<(), ListsError> {
    let lists = EligibilityLists::parse(Some(OWNERS), Some(AGENTS))?;

    assert_eq!(ids(lists.owners()), [1001]);
    assert_eq!(ids(lists.approvers()), [1002]);
    assert_eq!(ids(lists.maintainers()), [124_330_161]);
    assert_eq!(ids(lists.agents()), [124_330_161]);
    assert_eq!(
        lists
            .approvers()
            .first()
            .map(|identity| identity.login.as_str()),
        Some("approver")
    );
    Ok(())
}

#[test]
fn absent_files_read_as_empty_lists_so_nobody_is_privileged() -> Result<(), ListsError> {
    let lists = EligibilityLists::parse(None, None)?;

    assert_eq!(lists.owners(), []);
    assert_eq!(lists.approvers(), []);
    assert_eq!(lists.maintainers(), []);
    assert_eq!(lists.agents(), []);
    Ok(())
}

#[test]
fn a_repeated_id_in_one_list_is_refused() {
    let owners = r#"
owners = []
approvers = [{ id = 7, login = "a" }, { id = 7, login = "b" }]
maintainers = []
"#;

    let parsed = EligibilityLists::parse(Some(owners), Some("agents = []"));

    assert!(
        matches!(&parsed, Err(ListsError::Repeated {
            list: ListName::Approvers,
            id
        }) if *id == user(7)),
        "{parsed:?}"
    );
}

#[test]
fn a_maintainer_that_is_also_privileged_is_refused() {
    let owners = r#"
owners = [{ id = 9, login = "daily" }]
approvers = []
maintainers = [{ id = 9, login = "daily" }]
"#;

    let parsed = EligibilityLists::parse(Some(owners), Some("agents = []"));

    assert!(
        matches!(&parsed, Err(ListsError::MaintainerPrivileged { id }) if *id == user(9)),
        "{parsed:?}"
    );
}

#[test]
fn an_agent_that_is_also_privileged_is_refused() {
    let agents = r#"agents = [{ id = 1002, login = "approver" }]"#;

    let parsed = EligibilityLists::parse(Some(OWNERS), Some(agents));

    assert!(
        matches!(&parsed, Err(ListsError::AgentPrivileged { id }) if *id == user(1002)),
        "{parsed:?}"
    );
}

#[test]
fn a_zero_id_an_unknown_key_or_a_missing_list_is_refused() {
    let zero = "owners = [{ id = 0, login = \"x\" }]\napprovers = []\nmaintainers = []";
    let unknown = "owners = []\napprovers = []\nmaintainers = []\nadmins = []";
    let missing = "owners = []\nmaintainers = []";

    for text in [zero, unknown, missing] {
        let parsed = EligibilityLists::parse(Some(text), Some("agents = []"));

        assert!(
            matches!(
                parsed,
                Err(ListsError::Toml {
                    file: ListFile::Owners,
                    ..
                })
            ),
            "{text:?} parsed as {parsed:?}"
        );
    }
}

#[test]
fn a_login_github_would_not_accept_is_refused() {
    let owners = "owners = [{ id = 5, login = \"a b\" }]\napprovers = []\nmaintainers = []";

    let parsed = EligibilityLists::parse(Some(owners), Some("agents = []"));

    assert!(
        matches!(&parsed, Err(ListsError::Login {
            list: ListName::Owners,
            id
        }) if *id == user(5)),
        "{parsed:?}"
    );
}

#[test]
fn a_list_past_its_cap_is_refused() {
    let entries: Vec<String> = (1..=65)
        .map(|id| format!("{{ id = {id}, login = \"u{id}\" }}"))
        .collect();
    let agents = format!("agents = [{}]", entries.join(", "));

    let parsed = EligibilityLists::parse(None, Some(&agents));

    assert!(
        matches!(
            &parsed,
            Err(ListsError::TooMany {
                list: ListName::Agents
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn a_file_past_its_byte_cap_is_refused_unparsed() {
    let agents = format!("agents = []\n#{}", "x".repeat(64 << 10));

    let parsed = EligibilityLists::parse(None, Some(&agents));

    assert!(
        matches!(
            &parsed,
            Err(ListsError::TooLarge {
                file: ListFile::Agents
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn decide_only_keeps_privileged_ids_on_both_lists_and_every_restricted_one()
-> Result<(), ListsError> {
    let base = EligibilityLists::parse(Some(OWNERS), Some(AGENTS))?;
    let tip_owners = r#"
owners = [{ id = 1001, login = "break-glass" }]
approvers = [{ id = 1003, login = "new-approver" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }, { id = 77, login = "second" }]
"#;
    let tip = EligibilityLists::parse(
        Some(tip_owners),
        Some("agents = [{ id = 88, login = \"bot\" }]"),
    )?;

    let narrowed = base.narrowed_to(&tip);

    assert_eq!(ids(narrowed.owners()), [1001]);
    assert_eq!(
        ids(narrowed.approvers()),
        Vec::<u64>::new(),
        "removed at once, added later"
    );
    assert_eq!(ids(narrowed.maintainers()), [77, 124_330_161]);
    assert_eq!(ids(narrowed.agents()), [88, 124_330_161]);
    Ok(())
}

#[test]
fn timestamps_order_by_instant_and_display_canonically() -> Result<(), Box<dyn Error>> {
    let early: Timestamp = "2026-10-13T09:30:00Z".parse()?;
    let fraction: Timestamp = "2026-10-13T09:30:00.5Z".parse()?;
    let late: Timestamp = "2026-10-13T09:30:01Z".parse()?;
    let next_day: Timestamp = "2026-10-14T00:00:00Z".parse()?;

    assert!(early < fraction && fraction < late && late < next_day);
    assert_eq!(
        "2026-10-13T09:30:00.500Z".parse::<Timestamp>()?,
        fraction,
        "the same instant written twice is one time"
    );
    assert_eq!(fraction.to_string(), "2026-10-13T09:30:00.5Z");
    Ok(())
}

#[test]
fn a_timestamp_with_an_offset_or_no_zone_is_refused() {
    for text in [
        "2026-10-13T09:30:00+01:00",
        "2026-10-13T09:30:00",
        "2026-13-01T00:00:00Z",
        "",
    ] {
        assert_eq!(
            text.parse::<Timestamp>().err(),
            Some(InvalidTimestamp),
            "{text:?} parsed"
        );
    }
}

/// Decision 0005: this repository's lists hold the daily account in `agents` and `maintainers`
/// only, and no privileged identity yet, so nobody is eligible here until the approver exists.
#[test]
fn this_repository_s_lists_hold_decision_0005_s_ids() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.assay");
    let owners = fs::read_to_string(root.join("owners"))?;
    let agents = fs::read_to_string(root.join("agents"))?;

    let lists = EligibilityLists::parse(Some(&owners), Some(&agents))?;

    assert_eq!(ids(lists.owners()), Vec::<u64>::new());
    assert_eq!(ids(lists.approvers()), Vec::<u64>::new());
    assert_eq!(ids(lists.maintainers()), [124_330_161]);
    assert_eq!(ids(lists.agents()), [124_330_161]);
    let logins: Vec<&str> = lists
        .maintainers()
        .iter()
        .chain(lists.agents())
        .map(|identity| identity.login.as_str())
        .collect();
    assert_eq!(logins, ["Ryan-Britton", "Ryan-Britton"]);
    assert_eq!(lists.privileged(user(124_330_161)), None);
    Ok(())
}
