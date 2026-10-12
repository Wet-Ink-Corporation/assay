//! The gate environment allowlist (`docs/architecture/03-contracts.md` §2).

use std::ffi::OsString;

use assay_gates::GateEnvironment;

fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

#[test]
fn keeps_only_path_home_ci_and_assay_variables() {
    let inherited = vars(&[
        ("GITHUB_TOKEN", "ghs_secret"),
        ("PATH", "/usr/bin:/bin"),
        ("ACTIONS_RUNTIME_TOKEN", "secret"),
        ("HOME", "/home/runner"),
        ("CI", "true"),
        ("ASSAY_RUN_KEY", "k"),
        ("CARGO_HOME", "/cargo"),
        ("ASSAY_BASE_SHA", "abc"),
    ]);

    let environment = GateEnvironment::from_vars(inherited);

    assert_eq!(
        environment.vars(),
        vars(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/runner"),
            ("CI", "true"),
            ("ASSAY_RUN_KEY", "k"),
            ("ASSAY_BASE_SHA", "abc"),
        ])
    );
}

#[test]
fn matches_names_exactly_and_case_sensitively() {
    let inherited = vars(&[
        ("path", "/tmp"),
        ("PATHEXT", "x"),
        ("CI_JOB_TOKEN", "secret"),
        ("ASSAY", "x"),
        ("ASSAY_", "x"),
        ("assay_run_key", "x"),
        ("XASSAY_RUN_KEY", "x"),
    ]);

    let environment = GateEnvironment::from_vars(inherited);

    assert_eq!(environment.vars(), []);
}

#[test]
fn keeps_only_the_three_assay_variables_of_the_gate_contract() {
    // 03 §2 names three; a future ASSAY_* secret must not reach PR-controlled code.
    let inherited = vars(&[
        ("ASSAY_BASE_SHA", "abc"),
        ("ASSAY_APP_PRIVATE_KEY", "secret"),
        ("ASSAY_HEAD_SHA", "def"),
        ("ASSAY_FINDINGS_OUT", "/tmp/elsewhere.json"),
        ("ASSAY_RUN_KEY", "k"),
        ("ASSAY_RUN_KEYS", "x"),
    ]);

    let environment = GateEnvironment::from_vars(inherited);

    assert_eq!(
        environment.vars(),
        vars(&[
            ("ASSAY_BASE_SHA", "abc"),
            ("ASSAY_HEAD_SHA", "def"),
            ("ASSAY_RUN_KEY", "k"),
        ])
    );
}

#[test]
fn drops_names_that_are_not_unicode() {
    use std::os::unix::ffi::OsStringExt;
    let inherited = vec![(
        OsString::from_vec(b"ASSAY_\xff".to_vec()),
        OsString::from("x"),
    )];

    let environment = GateEnvironment::from_vars(inherited);

    assert_eq!(environment.vars(), []);
}
