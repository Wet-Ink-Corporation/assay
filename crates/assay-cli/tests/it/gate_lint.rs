//! `assay gate gates-lint` (#60 ST-8, Z2b) as the binary: `ASSAY_MERGE_BASE` becomes the
//! ratchet base, the pinned base and head are fetched with `ASSAY_RUN_TOKEN` only, and temper's
//! declared `lint` steps, the pinned `baseline --check` and every `exec` gate of `job = "lint"`
//! run with the gate environment; the documents and the `gates` output follow ST-6. The scene is
//! `gate_static_scene`: no network, no real temper. What the job refuses of the checkout (#348)
//! is `gate_lint_checkout`'s.

use std::error::Error;
use std::fs;
use std::process::Output;

use assay_app::{Contract, ContractValidator, Sha256Digest};

use crate::gate::temper;
use crate::gate_static_scene::{RUN_KEY, RUN_TOKEN, Scene};

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `<out>/<plugin>.json`, validated as `assay.findings/1`: its source name, run key and
/// conclusion, and the sha256 of its bytes.
fn document(scene: &Scene, plugin: &str) -> Result<([String; 3], String), Box<dyn Error>> {
    let bytes = fs::read(scene.path("out").join(format!("{plugin}.json")))?;
    let document = ContractValidator::new()?.read(Contract::Findings, &bytes)?;
    let field = |pointer: &str| {
        document
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{plugin}.json has no {pointer}"))
    };
    Ok((
        [
            field("/source/name")?,
            field("/source/run_key")?,
            field("/conclusion")?,
        ],
        Sha256Digest::of(&bytes).to_string(),
    ))
}

/// An `exec` gate `probe` of `job` that writes a `neutral` document.
fn probe(job: &str) -> String {
    let document = format!(
        r#"{{"schema":"assay.findings/1","source":{{"kind":"gate","name":"probe","version":"v","run_key":"{RUN_KEY}"}},"conclusion":"neutral","findings":[]}}"#
    );
    let script = format!("printf '%s' '{document}' > \"$ASSAY_FINDINGS_OUT\"");
    format!(
        "\n[[plugin]]\nname = \"probe\"\nadapter = \"exec\"\n\
         exec = [\"/bin/sh\", \"-c\", {script:?}]\njob = \"{job}\"\nrequired = false\n"
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_lint_steps_the_ratchet_and_the_exec_lint_gate_write_documents_and_the_gates_output()
-> Result<(), Box<dyn Error>> {
    let plugins = format!(
        "{}{}",
        temper(&[("fmt", "lint", "touch fmt-ran")]),
        probe("lint")
    );
    let scene = Scene::lint(
        &[("src/lib.rs", "pub fn two() -> u8 {\n    2\n}\n")],
        &plugins,
    )?;

    let output = scene.run(|_| {})?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\nprobe neutral\n");
    let (temper, temper_digest) = document(&scene, "temper")?;
    let (probe, probe_digest) = document(&scene, "probe")?;
    assert_eq!(temper, ["temper", RUN_KEY, "pass"]);
    assert_eq!(probe, ["probe", RUN_KEY, "neutral"]);
    assert_eq!(
        scene.recorded("output")?,
        format!(
            "gates=[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{temper_digest}\"}},\
             {{\"plugin\":\"probe\",\"conclusion\":\"neutral\",\"sha256\":\"{probe_digest}\"}}]\n"
        )
    );
    assert!(scene.head.join("fmt-ran").exists(), "the lint step ran");
    let script = scene.path("pinned/temper.mjs");
    assert_eq!(
        scene.recorded("bin/node-args")?,
        format!(
            "{} baseline --check --locked --base {} --findings-out target/temper/findings.json\n",
            script.display(),
            scene.base_sha
        ),
        "only the pinned baseline --check ran, against the base"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_ratchet_is_believed_only_against_assay_merge_base() -> Result<(), Box<dyn Error>> {
    let scene = Scene::lint(
        &[("src/lib.rs", "pub fn two() -> u8 {\n    2\n}\n")],
        &temper(&[]),
    )?;

    // temper reports the base as its merge base; plan said the head is.
    let output = scene.run(|command| {
        command.env("ASSAY_MERGE_BASE", &scene.head_sha);
    })?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper gate-error:ratchet-degraded\n");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_or_malformed_merge_base_exits_2_before_anything_runs() -> Result<(), Box<dyn Error>> {
    let scene = Scene::lint(&[], &temper(&[("fmt", "lint", "touch fmt-ran")]))?;
    let upper = scene.base_sha.to_uppercase();
    let short = scene.base_sha.get(..12).ok_or("a 40-digit sha")?.to_owned();
    let cases: [(Option<&str>, &str); 4] = [
        (None, "assay: ASSAY_MERGE_BASE is not set"),
        (Some(""), "assay: ASSAY_MERGE_BASE is not"),
        (Some(&upper), "assay: ASSAY_MERGE_BASE is not"),
        (Some(&short), "assay: ASSAY_MERGE_BASE is not"),
    ];

    for (value, starts) in cases {
        let output = scene.run(|command| match value {
            None => {
                command.env_remove("ASSAY_MERGE_BASE");
            }
            Some(value) => {
                command.env("ASSAY_MERGE_BASE", value);
            }
        })?;

        assert_eq!(output.status.code(), Some(2), "{value:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with(starts), "{value:?}: {stderr}");
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            assert!(
                !stderr.contains(value),
                "the value is never echoed: {stderr}"
            );
        }
        assert!(scene.untouched()?, "{value:?}: nothing runs");
        assert!(!scene.head.join("fmt-ran").exists(), "{value:?}");
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn no_token_reaches_a_lint_step_an_exec_gate_or_the_pinned_temper() -> Result<(), Box<dyn Error>> {
    // Both run from the head checkout, which is `<root>/work`.
    let plugins = format!(
        "{}\n[[plugin]]\nname = \"env-probe\"\nadapter = \"exec\"\n\
         exec = [\"/bin/sh\", \"-c\", \"/usr/bin/env > ../exec-env\"]\njob = \"lint\"\n\
         required = false\n",
        temper(&[("fmt", "lint", "/usr/bin/env > ../step-env")]),
    );
    let scene = Scene::lint(&[], &plugins)?;
    let tokens = [
        ("GITHUB_TOKEN", "ghs_githubtoken"),
        ("GH_TOKEN", "ghs_ghtoken"),
        ("ASSAY_GITHUB_TOKEN", "ghs_assaytoken"),
        ("ASSAY_LEDGER_TOKEN", "ghs_ledgertoken"),
        ("ACTIONS_RUNTIME_TOKEN", "actions_runtime"),
    ];

    let output = scene.run(|command| {
        command.envs(tokens);
    })?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    for seen in ["step-env", "exec-env", "bin/node-env"] {
        let env = scene.recorded(seen)?;
        assert!(
            env.contains(&format!("ASSAY_RUN_KEY={RUN_KEY}")),
            "{seen} ran with the gate environment: {env}"
        );
        for secret in tokens.iter().map(|(_, value)| *value).chain([RUN_TOKEN]) {
            assert!(!env.contains(secret), "{seen} sees no token: {env}");
        }
        assert!(
            !env.contains("ASSAY_MERGE_BASE=") && !env.contains("GITHUB_REPOSITORY="),
            "{seen} sees only the allowlist: {env}"
        );
    }
    Ok(())
}
