use std::assert_matches;
use std::fs;

use super::*;

const OPEN: LedgerJob = LedgerJob {
    name: "open",
    purpose: "open records the push in the ledger",
};

const RELEASE: LedgerJob = LedgerJob {
    name: "release",
    purpose: "release settles the run in the ledger",
};

/// `cause` of `job`, as the binary prints it.
fn shown(job: LedgerJob, cause: Cause) -> String {
    LedgerSetupError { job, cause }.to_string()
}

#[test]
fn a_setup_refusal_reads_as_each_job_wrote_it_before() {
    let cases = [
        (
            OPEN,
            Cause::NoLedgerRepo,
            "open: base config sets no [assay].ledger_repo: open records the push in the ledger",
        ),
        (
            RELEASE,
            Cause::NoLedgerRepo,
            "release: base config sets no [assay].ledger_repo: release settles the run in the \
             ledger",
        ),
        (
            OPEN,
            Cause::LedgerRepo,
            "open: base config's [assay].ledger_repo is not owner/name",
        ),
        (
            RELEASE,
            Cause::LedgerRepo,
            "release: base config's [assay].ledger_repo is not owner/name",
        ),
        (
            OPEN,
            Cause::NoGh,
            "open: no gh on PATH: open reaches GitHub through gh",
        ),
        (
            RELEASE,
            Cause::NoGh,
            "release: no gh on PATH: release reaches GitHub through gh",
        ),
        (
            OPEN,
            Cause::Scratch(io::Error::other("disk full")),
            "open: the scratch directory: disk full",
        ),
        (
            RELEASE,
            Cause::Scratch(io::Error::other("disk full")),
            "release: the scratch directory: disk full",
        ),
    ];

    for (job, cause, expected) in cases {
        assert_eq!(shown(job, cause), expected);
    }
}

#[test]
fn a_git_or_remote_refusal_names_the_ledger_push_side() {
    let remote = Cause::Remote(LedgerRemoteError::Remote(
        "http://example.com/x.git"
            .parse::<assay_forge::git::HttpsRemote>()
            .expect_err("not https"),
    ));

    let line = shown(RELEASE, remote);

    assert!(
        line.starts_with("release: the ledger's push remote: "),
        "{line}"
    );
}

#[test]
fn a_variable_refusal_reads_as_before() {
    let missing = VariableRefusal {
        job: "release",
        name: LEDGER_TOKEN,
        holds: "the assay-ledger-writer App's installation token",
        problem: Problem::Missing,
    };
    let invalid = VariableRefusal {
        problem: Problem::Invalid,
        job: "open",
        ..missing
    };

    assert_eq!(
        (missing.to_string(), invalid.to_string()),
        (
            "release: ASSAY_LEDGER_TOKEN is not set: it holds the assay-ledger-writer App's \
             installation token"
                .to_owned(),
            "open: ASSAY_LEDGER_TOKEN is not the assay-ledger-writer App's installation token"
                .to_owned(),
        )
    );
}

#[test]
fn a_token_is_refused_unset_or_unprintable() {
    assert_matches!(token(None), Err(Problem::Missing));
    assert_matches!(token(Some(OsStr::new(""))), Err(Problem::Invalid));
    assert_matches!(token(Some(OsStr::new("ghs one"))), Err(Problem::Invalid));
    assert_matches!(token(Some(OsStr::new("ghs_token"))), Ok(_));
}

#[cfg(unix)]
#[test]
fn a_token_that_is_not_utf8_is_invalid() {
    use std::os::unix::ffi::OsStrExt;

    assert_matches!(
        token(Some(OsStr::from_bytes(b"ghs_\xff"))),
        Err(Problem::Invalid)
    );
}

/// Base config whose `[assay]` table ends with `extra`, in `dir`.
fn config(dir: &Path, extra: &str) -> PathBuf {
    let path = dir.join("plugins.toml");
    let digest = format!("sha256:{}", "0".repeat(64));
    fs::write(&path, format!("[assay]\ndigest = \"{digest}\"\n{extra}")).expect("a config");
    path
}

#[test]
fn the_ledger_repository_is_read_from_base_config() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let named = config(
        dir.path(),
        "ledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n",
    );

    let ledger = ledger_repo(OPEN, &named).expect("a ledger repository");

    assert_eq!(ledger.to_string(), "Wet-Ink-Corporation/assay-ledger");
}

#[test]
fn a_config_naming_no_ledger_or_not_owner_name_is_refused() {
    for (extra, expected) in [
        ("", "release: base config sets no [assay].ledger_repo"),
        (
            "ledger_repo = \"assay-ledger\"\n",
            "release: base config's [assay].ledger_repo is not owner/name",
        ),
    ] {
        let dir = tempfile::tempdir().expect("a scratch directory");

        let refused = ledger_repo(RELEASE, &config(dir.path(), extra))
            .expect_err("refused")
            .to_string();

        assert!(refused.starts_with(expected), "{extra}: {refused}");
    }
}

#[test]
fn a_left_behind_directory_reads_as_before() {
    let lines = [
        Left::Gh(LEDGER_TOKEN, io::Error::other("busy")),
        Left::Scratch("release", io::Error::other("busy")),
    ]
    .map(|left| left.to_string());

    assert_eq!(
        lines,
        [
            "could not remove gh's scratch configuration directory (ASSAY_LEDGER_TOKEN): busy",
            "could not remove release's scratch directory: busy",
        ]
    );
}
