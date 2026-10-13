//! `.assay/plugins.toml` parsing and validation (`docs/architecture/03-contracts.md` §1).

use std::error::Error;
use std::time::Duration;

use assay_app::{
    Adapter, BuildJob, CommitId, ConfigError, ExactVersion, InfraFailurePolicy, ParseCommitIdError,
    ParseDigestError, ParsePinError, Pin, Plugin, PluginsConfig, PrClass, Promotion, RunnerKind,
};

const GIT: &str = "0123456789abcdef0123456789abcdef01234567";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// The 03 §1 example, written as valid TOML.
fn spec_example() -> String {
    format!(
        r#"
[assay]
digest = "sha256:{HEX64}"
warm_target = "sha256:{HEX64}"
claude_cli = "2.1.296"
runner = "claude-oauth"
protected_bases = ["main"]
llm_blockers = "advisory"
llm_majors = "block"
on_infra_failure = "block"
block_on_infra_for = ["exec-surface"]
ledger_repo = "Wet-Ink-Corporation/assay-ledger"

[gates]
binary_globs = ["assets/**"]

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{GIT}"
required = true
timeout_s = 1200

[[plugin.step]]
name = "fmt"
job = "lint"
cmd = ["cargo", "fmt", "--check"]
timeout_s = 300

[[plugin.step]]
name = "tests"
job = "test"
cmd = ["cargo", "nextest", "run", "--workspace", "--no-tests=fail"]
timeout_s = 1500

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "oci:sha256:{HEX64}"
required = false

[[plugin]]
name = "custom-lint"
adapter = "exec"
exec = ["./scripts/lint.sh", "--json"]
job = "lint"
required = false
"#
    )
}

/// A minimal valid file with one plugin built from `plugin` (the lines after `[[plugin]]`).
fn with_plugin(plugin: &str) -> String {
    format!("[assay]\ndigest = \"sha256:{HEX64}\"\n\n[[plugin]]\n{plugin}\n")
}

fn parse(text: &str) -> Result<PluginsConfig, ConfigError> {
    text.parse::<PluginsConfig>()
}

#[test]
fn parses_the_spec_example_into_its_plugins() -> Result<(), Box<dyn Error>> {
    let config = parse(&spec_example())?;

    let names: Vec<&str> = config.plugins().iter().map(|p| p.name().as_str()).collect();
    assert_eq!(names, ["temper", "weigh-in", "custom-lint"]);
    let [temper, weigh_in, custom] = config.plugins() else {
        return Err("three plugins expected".into());
    };
    assert!(temper.required());
    assert!(!weigh_in.required());
    assert_eq!(temper.timeout(), Duration::from_mins(20));
    let Adapter::Temper { pin, steps } = temper.adapter() else {
        return Err("temper adapter expected".into());
    };
    assert_eq!(pin.to_string(), format!("git:{GIT}"));
    let steps: Vec<(&str, BuildJob, Vec<&str>, Duration)> = steps
        .iter()
        .map(|s| {
            (
                s.name().as_str(),
                s.job(),
                s.command().iter().collect(),
                s.timeout(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            (
                "fmt",
                BuildJob::Lint,
                vec!["cargo", "fmt", "--check"],
                Duration::from_secs(300)
            ),
            (
                "tests",
                BuildJob::Test,
                vec!["cargo", "nextest", "run", "--workspace", "--no-tests=fail"],
                Duration::from_mins(25)
            ),
        ]
    );
    let Adapter::WeighIn { pin } = weigh_in.adapter() else {
        return Err("weigh-in adapter expected".into());
    };
    assert!(matches!(pin, Pin::Oci(_)), "{pin}");
    let Adapter::Exec { command, job } = custom.adapter() else {
        return Err("exec adapter expected".into());
    };
    assert_eq!(command.program(), "./scripts/lint.sh");
    assert_eq!(command.args(), ["--json"]);
    assert_eq!(*job, BuildJob::Lint);
    Ok(())
}

#[test]
fn parses_the_assay_table_of_the_spec_example() -> Result<(), Box<dyn Error>> {
    let config = parse(&spec_example())?;

    let assay = config.assay();
    assert_eq!(assay.digest.to_string(), format!("sha256:{HEX64}"));
    assert_eq!(
        assay.warm_target.as_ref().map(ToString::to_string),
        Some(format!("sha256:{HEX64}"))
    );
    assert_eq!(
        assay.claude_cli.as_ref().map(ExactVersion::as_str),
        Some("2.1.296")
    );
    assert_eq!(assay.runner, Some(RunnerKind::ClaudeOauth));
    assert_eq!(assay.protected_bases, ["main"]);
    assert_eq!(assay.llm_blockers, Promotion::Advisory);
    assert_eq!(assay.llm_majors, Promotion::Block);
    assert_eq!(assay.on_infra_failure, InfraFailurePolicy::Block);
    assert_eq!(assay.block_on_infra_for, [PrClass::ExecSurface]);
    assert_eq!(
        assay.ledger_repo.as_deref(),
        Some("Wet-Ink-Corporation/assay-ledger")
    );
    assert_eq!(config.binary_globs(), ["assets/**"]);
    Ok(())
}

#[test]
fn applies_the_documented_defaults_to_a_minimal_file() -> Result<(), Box<dyn Error>> {
    let config = parse(&with_plugin(&format!(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:{GIT}\"\nrequired = true"
    )))?;

    let assay = config.assay();
    assert_eq!(assay.llm_blockers, Promotion::Advisory);
    assert_eq!(assay.llm_majors, Promotion::Advisory);
    assert_eq!(assay.on_infra_failure, InfraFailurePolicy::Warn);
    assert_eq!(
        assay.block_on_infra_for,
        [PrClass::TrustPath, PrClass::ExecSurface]
    );
    assert!(
        assay.protected_bases.is_empty(),
        "no base is protected by default"
    );
    assert!(config.binary_globs().is_empty(), "no glob by default");
    let [temper] = config.plugins() else {
        return Err("one plugin expected".into());
    };
    assert_eq!(
        temper.timeout(),
        Duration::from_secs(assay_app::DEFAULT_TIMEOUT_SECS)
    );
    Ok(())
}

#[test]
fn rejects_a_git_tag_pin() {
    let text = with_plugin(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:v1.2.3\"\nrequired = true",
    );

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Pin { ref plugin, source: ParsePinError::Tag }) if plugin == "temper"
        ),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_oci_tag_pin() {
    let text = with_plugin(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"oci:latest\"\nrequired = true",
    );

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Pin {
                source: ParsePinError::Tag,
                ..
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_abbreviated_commit_pin_as_a_tag() {
    let text = with_plugin(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:0123456\"\nrequired = true",
    );

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Pin {
                source: ParsePinError::Tag,
                ..
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_pin_without_a_known_scheme() {
    let text = with_plugin(&format!(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"{GIT}\"\nrequired = true"
    ));

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Pin {
                source: ParsePinError::UnknownScheme,
                ..
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_assay_digest_that_is_a_tag() {
    let text = "[assay]\ndigest = \"latest\"\n";

    let parsed = parse(text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Digest {
                field: "assay.digest",
                source: ParseDigestError
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_warm_target_that_is_not_a_sha256_digest() {
    let upper = format!("sha256:{}", HEX64.to_uppercase());
    let short = format!("sha256:{}", HEX64.get(..63).unwrap_or_default());
    let prefixed = format!("oci:sha256:{HEX64}");
    let refused = ["latest", "main", HEX64, &upper, &short, &prefixed]
        .into_iter()
        .map(|warm| {
            let text = format!("[assay]\ndigest = \"sha256:{HEX64}\"\nwarm_target = \"{warm}\"\n");
            parse(&text)
        })
        .filter(|parsed| {
            matches!(
                parsed,
                Err(ConfigError::Digest {
                    field: "assay.warm_target",
                    source: ParseDigestError
                })
            )
        })
        .count();

    assert_eq!(refused, 6, "every malformed warm_target is a digest error");
}

#[test]
fn a_file_without_warm_target_has_none() -> Result<(), Box<dyn Error>> {
    let config = parse(&format!("[assay]\ndigest = \"sha256:{HEX64}\"\n"))?;

    assert_eq!(config.assay().warm_target, None);
    Ok(())
}

#[test]
fn rejects_applies_when_because_required_gates_always_run() {
    let text = with_plugin(&format!(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:{GIT}\"\nrequired = true\napplies_when = \"rust\""
    ));

    let parsed = parse(&text);

    assert!(matches!(parsed, Err(ConfigError::Syntax(_))), "{parsed:?}");
}

#[test]
fn rejects_a_builtin_adapter_without_a_pin() {
    let text = with_plugin("name = \"temper\"\nadapter = \"builtin:temper\"\nrequired = true");

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::MissingKey { key: "pin", .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_pin_on_an_exec_adapter() {
    let text = with_plugin(&format!(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\npin = \"git:{GIT}\"\nrequired = false"
    ));

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::UnexpectedKey { key: "pin", .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_exec_adapter_outside_a_build_job() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"static\"\nrequired = false",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::Job { ref job, .. }) if job == "static"),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_exec_adapter_without_a_job() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\nrequired = false",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::MissingKey { key: "job", .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_an_empty_exec_command() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = []\njob = \"lint\"\nrequired = false",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::Command { .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_steps_on_an_adapter_that_does_not_run_them() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false\n\n[[plugin.step]]\nname = \"fmt\"\njob = \"lint\"\ncmd = [\"cargo\", \"fmt\"]\ntimeout_s = 30",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::UnexpectedKey { key: "step", .. })),
        "{parsed:?}"
    );
}

/// A temper plugin with one step `tests` of `job` running `cmd` (a TOML array).
fn temper_step(job: &str, cmd: &str) -> String {
    with_plugin(&format!(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:{GIT}\"\nrequired = true\n\n[[plugin.step]]\nname = \"tests\"\njob = \"{job}\"\ncmd = {cmd}\ntimeout_s = 1500"
    ))
}

#[test]
fn rejects_a_step_that_lets_zero_tests_pass() {
    let weakened = [
        "--no-tests=pass",
        "--no-tests=warn",
        "--no-tests",
        "--no-tests=",
        "--no-tests=FAIL",
    ];
    for job in ["test", "lint"] {
        for arg in weakened {
            let cmd = format!("[\"cargo\", \"nextest\", \"run\", \"--no-tests=fail\", \"{arg}\"]");

            let parsed = parse(&temper_step(job, &cmd));

            assert!(
                matches!(&parsed, Err(ConfigError::NoTests { gate, arg: given }) if gate == "temper.tests" && given == arg),
                "{job} {arg}: {parsed:?}"
            );
        }
    }
}

#[test]
fn accepts_a_step_whose_zero_tests_fail() -> Result<(), Box<dyn Error>> {
    let cmd = "[\"cargo\", \"nextest\", \"run\", \"--workspace\", \"--no-tests=fail\"]";

    let config = parse(&temper_step("test", cmd))?;

    let Some(Adapter::Temper { steps, .. }) = config.plugins().first().map(Plugin::adapter) else {
        return Err("a temper plugin expected".into());
    };
    let commands: Vec<Vec<&str>> = steps
        .iter()
        .map(|step| step.command().iter().collect())
        .collect();
    assert_eq!(
        commands,
        [["cargo", "nextest", "run", "--workspace", "--no-tests=fail"]]
    );
    Ok(())
}

#[test]
fn rejects_an_unknown_adapter() {
    let text = with_plugin(&format!(
        "name = \"x\"\nadapter = \"builtin:other\"\npin = \"git:{GIT}\"\nrequired = true"
    ));

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::UnknownAdapter { ref adapter, .. }) if adapter == "builtin:other"),
        "{parsed:?}"
    );
}

#[test]
fn rejects_two_plugins_with_one_name() {
    let plugin = "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false";
    let text = format!("{}\n[[plugin]]\n{plugin}\n", with_plugin(plugin));

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::DuplicateName { ref name }) if name == "lint"),
        "{parsed:?}"
    );
}

#[test]
fn rejects_two_steps_of_one_plugin_with_one_name() {
    let step = "[[plugin.step]]\nname = \"fmt\"\njob = \"lint\"\ncmd = [\"cargo\", \"fmt\"]\ntimeout_s = 30";
    let text = with_plugin(&format!(
        "name = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:{GIT}\"\nrequired = true\n\n{step}\n\n{step}"
    ));

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::DuplicateName { ref name }) if name == "temper.fmt"),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_claude_cli_range_instead_of_an_exact_version() {
    let text = format!("[assay]\ndigest = \"sha256:{HEX64}\"\nclaude_cli = \"^2.1\"\n");

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::Version { ref version }) if version == "^2.1"),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_name_that_is_not_a_plain_identifier() {
    let text = with_plugin(
        "name = \"../lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::Name { .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_zero_timeout() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false\ntimeout_s = 0",
    );

    let parsed = parse(&text);

    assert!(
        matches!(parsed, Err(ConfigError::Timeout { seconds: 0, .. })),
        "{parsed:?}"
    );
}

#[test]
fn rejects_a_timeout_longer_than_a_job_can_run() {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false\ntimeout_s = 21601",
    );

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Timeout {
                seconds: 21_601,
                ..
            })
        ),
        "{parsed:?}"
    );
}

#[test]
fn accepts_the_longest_timeout_a_job_can_run() -> Result<(), Box<dyn Error>> {
    let text = with_plugin(
        "name = \"lint\"\nadapter = \"exec\"\nexec = [\"./lint.sh\"]\njob = \"lint\"\nrequired = false\ntimeout_s = 21600",
    );

    let config = parse(&text)?;

    let timeouts: Vec<Duration> = config.plugins().iter().map(Plugin::timeout).collect();
    assert_eq!(timeouts, [Duration::from_secs(21_600)]);
    Ok(())
}

#[test]
fn pins_round_trip_through_display() -> Result<(), Box<dyn Error>> {
    for text in [format!("git:{GIT}"), format!("oci:sha256:{HEX64}")] {
        let pin: Pin = text.parse()?;

        assert_eq!(pin.to_string(), text);
    }
    Ok(())
}

#[test]
fn rejects_an_uppercase_commit_pin() {
    let parsed = format!("git:{}", GIT.to_uppercase()).parse::<Pin>();

    assert_eq!(parsed, Err(ParsePinError::Tag));
}

#[test]
fn a_commit_id_round_trips_through_display() -> Result<(), Box<dyn Error>> {
    let commit: CommitId = GIT.parse()?;

    assert_eq!(commit.as_str(), GIT);
    Ok(())
}

#[test]
fn a_commit_id_is_exactly_forty_lowercase_hex_digits() {
    let short = GIT.get(..39).unwrap_or_default().to_owned();
    for text in [
        short,
        format!("{GIT}0"),
        GIT.to_uppercase(),
        format!("g{}", GIT.get(1..).unwrap_or_default()),
        format!("git:{GIT}"),
        String::new(),
    ] {
        assert_eq!(
            text.parse::<CommitId>(),
            Err(ParseCommitIdError),
            "{text:?} is not a full commit id"
        );
    }
}
