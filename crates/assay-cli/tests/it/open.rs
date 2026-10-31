//! `assay open` (#54, #60 ST-4): its run from the Actions environment through `RunEnv`, the
//! re-run refusal before any token is read (R3-05, I1), and its one flag, `--config`.

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_cli::{OpenCommand, OpenUsage, RunVariables};
use assay_domain::RunUid;

/// `n` (at least 1) as a non-zero id.
fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(n.saturating_sub(1))
}

fn variables(run_id: &str, attempt: &str, repository_id: &str) -> RunVariables {
    RunVariables {
        run_id: Some(OsString::from(run_id)),
        run_attempt: Some(OsString::from(attempt)),
        repository_id: Some(OsString::from(repository_id)),
        repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
    }
}

/// `--config base/.assay/plugins.toml`, the workflow's argv.
fn config() -> Vec<OsString> {
    vec!["--config".into(), "base/.assay/plugins.toml".into()]
}

#[test]
fn a_first_attempt_is_read_from_the_actions_environment() {
    let parsed = OpenCommand::parse(config(), &variables("39168909542", "1", "42"));

    let parsed = parsed.map(|command| (command.run.run_env().run_uid, command.config));
    assert_eq!(
        parsed,
        Ok((
            RunUid {
                repo_id: id(42),
                run_id: id(39_168_909_542),
                run_attempt: id(1),
            },
            PathBuf::from("base/.assay/plugins.toml"),
        ))
    );
}

#[test]
fn a_re_run_is_refused_before_anything_is_wired() {
    let parsed = OpenCommand::parse(config(), &variables("39168909542", "2", "42"));

    assert_eq!(
        parsed.map(|command| command.config),
        Err(OpenUsage::NotFirstAttempt { attempt: id(2) })
    );
}

#[test]
fn a_missing_or_malformed_variable_is_refused_by_name() {
    let missing = RunVariables {
        run_attempt: None,
        ..variables("1", "1", "42")
    };

    assert_eq!(
        [
            OpenCommand::parse(config(), &missing),
            OpenCommand::parse(config(), &variables("0", "1", "42")),
            OpenCommand::parse(config(), &variables("1", "1", "+42")),
            OpenCommand::parse(config(), &variables("1", "", "42")),
        ]
        .map(|parsed| parsed.map(|command| command.config)),
        [
            Err(OpenUsage::Missing("GITHUB_RUN_ATTEMPT")),
            Err(OpenUsage::Invalid("GITHUB_RUN_ID")),
            Err(OpenUsage::Invalid("GITHUB_REPOSITORY_ID")),
            Err(OpenUsage::Invalid("GITHUB_RUN_ATTEMPT")),
        ]
    );
}

#[test]
fn a_zero_padded_attempt_is_refused_as_run_env_refuses_it() {
    let parsed = OpenCommand::parse(config(), &variables("1", "01", "42"));

    assert_eq!(
        parsed.map(|command| command.config),
        Err(OpenUsage::Invalid("GITHUB_RUN_ATTEMPT"))
    );
}

#[test]
fn open_refuses_an_argument_it_does_not_take() {
    let parsed = OpenCommand::parse(vec![OsString::from("--json")], &variables("1", "1", "42"));

    assert_eq!(
        parsed.map(|command| command.config),
        Err(OpenUsage::Arguments)
    );
}

#[test]
fn open_takes_config_once_and_nothing_else() {
    let refused = [
        Vec::new(),
        vec![OsString::from("--config")],
        [config(), config()].concat(),
        [config(), vec![OsString::from("--json")]].concat(),
    ];

    for args in refused {
        let parsed = OpenCommand::parse(args.clone(), &variables("1", "1", "42"));

        assert_eq!(
            parsed.map(|command| command.config),
            Err(OpenUsage::Arguments),
            "{args:?}"
        );
    }
}
