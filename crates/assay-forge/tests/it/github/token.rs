//! Installation tokens are minted narrowed to the job's repositories and permissions, and
//! refused when GitHub grants anything else (05 §4).

use std::error::Error;

use assay_forge::github::{
    Access, Api, ForgeError, InstallationId, Method, Permission, ScopeError, TokenScope,
};
use serde_json::json;

use super::recorded::{Recorded, Waits, first_write, fixture, id, reply};

/// The mint endpoint of the `assay-verdict` installation on `assay`
/// (`docs/decisions/0004-assay-verdict-app.md`).
const MINT: &str = "app/installations/172801043/access_tokens";

/// `decide`'s consumer-repo scope (05 §4).
fn decide_scope() -> Result<TokenScope, ScopeError> {
    TokenScope::new(
        ["assay"],
        [
            (Permission::Checks, Access::Write),
            (Permission::PullRequests, Access::Write),
        ],
    )
}

/// GitHub answering the mint with fixture `name`.
fn minting(name: &str) -> Recorded {
    Recorded::default().then(Method::Post, MINT, reply(201, &[], fixture(name)))
}

#[test]
fn a_token_is_requested_for_named_repositories_and_permissions_only() -> Result<(), Box<dyn Error>>
{
    let recorded = minting("installation_token.json");
    let waits = Waits::default();

    let token = Api::new(&recorded, &waits)
        .mint_installation_token(InstallationId(id(172_801_043)), &decide_scope()?)?;

    assert_eq!(token.secret(), "ghs_fixture-not-a-token");
    assert_eq!(token.expires_at(), "2026-10-19T04:00:00Z");
    assert_eq!(
        first_write(&recorded),
        Some(json!({
            "repositories": ["assay"],
            "permissions": {"checks": "write", "pull_requests": "write"},
        }))
    );
    Ok(())
}

#[test]
fn a_token_with_more_permissions_than_asked_is_refused() -> Result<(), Box<dyn Error>> {
    let recorded = minting("installation_token_broader.json");
    let waits = Waits::default();

    let token = Api::new(&recorded, &waits)
        .mint_installation_token(InstallationId(id(172_801_043)), &decide_scope()?);

    assert!(matches!(token, Err(ForgeError::ScopeMismatch)), "{token:?}");
    Ok(())
}

#[test]
fn a_token_over_every_repository_is_refused() -> Result<(), Box<dyn Error>> {
    let recorded = minting("installation_token_all_repositories.json");
    let waits = Waits::default();

    let token = Api::new(&recorded, &waits)
        .mint_installation_token(InstallationId(id(172_801_043)), &decide_scope()?);

    assert!(matches!(token, Err(ForgeError::ScopeMismatch)), "{token:?}");
    Ok(())
}

#[test]
fn a_token_whose_grant_differs_from_the_request_is_refused() -> Result<(), Box<dyn Error>> {
    let two_repositories = TokenScope::new(
        ["assay", "assay-ledger"],
        [
            (Permission::Checks, Access::Write),
            (Permission::PullRequests, Access::Write),
        ],
    )?;
    for (fixture_name, scope) in [
        ("installation_token_extra_repository.json", decide_scope()?),
        ("installation_token_other_repository.json", decide_scope()?),
        ("installation_token.json", two_repositories),
        ("installation_token_narrower.json", decide_scope()?),
    ] {
        let recorded = minting(fixture_name);
        let waits = Waits::default();

        let token = Api::new(&recorded, &waits)
            .mint_installation_token(InstallationId(id(172_801_043)), &scope);

        assert!(
            matches!(token, Err(ForgeError::ScopeMismatch)),
            "{fixture_name}: {token:?}"
        );
    }
    Ok(())
}

#[test]
fn a_token_never_shows_in_debug_output() -> Result<(), Box<dyn Error>> {
    let recorded = minting("installation_token.json");
    let waits = Waits::default();

    let token = Api::new(&recorded, &waits)
        .mint_installation_token(InstallationId(id(172_801_043)), &decide_scope()?)?;

    let shown = format!("{token:?}");
    assert!(!shown.contains("ghs_"), "{shown}");
    Ok(())
}

#[test]
fn a_scope_that_would_widen_to_everything_is_refused() {
    let no_repository = TokenScope::new([], [(Permission::Checks, Access::Write)]);
    let no_permission = TokenScope::new(["assay"], []);
    let bad_name = TokenScope::new(["acme/assay"], [(Permission::Checks, Access::Write)]);

    assert_eq!(no_repository, Err(ScopeError::NoRepository));
    assert_eq!(no_permission, Err(ScopeError::NoPermission));
    assert_eq!(bad_name, Err(ScopeError::InvalidName));
}
