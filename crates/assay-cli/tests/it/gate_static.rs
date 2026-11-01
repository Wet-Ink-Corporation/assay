//! `assay gate gates-static` (#60 ST-7, Z2s) as the binary: the pinned base and head are
//! fetched into an object-only repository with `ASSAY_RUN_TOKEN` and no other token, the temper
//! Z2s checks and Weigh-In run through `run_gates` with `--node` and `--temper`, and the
//! documents and the `gates` output follow ST-6. The scene is `gate_static_scene`: no network,
//! no real temper. The refusals are `gate_static_refusals`.

use std::error::Error;
use std::fs;
use std::process::Output;

use assay_app::{Contract, ContractValidator, Sha256Digest};

use crate::gate_static_scene::{
    BASELINE, REPO, RUN_KEY, RUN_TOKEN, RUN_TOKEN_HEADER, Scene, baseline,
};

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `<out>/<plugin>.json`, validated as `assay.findings/1`: its source name, run key and
/// conclusion.
fn document(scene: &Scene, plugin: &str) -> Result<[String; 3], Box<dyn Error>> {
    let bytes = fs::read(scene.path("out").join(format!("{plugin}.json")))?;
    let document = ContractValidator::new()?.read(Contract::Findings, &bytes)?;
    let field = |pointer: &str| {
        document
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{plugin}.json has no {pointer}"))
    };
    Ok([
        field("/source/name")?,
        field("/source/run_key")?,
        field("/conclusion")?,
    ])
}

/// The sha256 of `<out>/<plugin>.json` as written.
fn digest(scene: &Scene, plugin: &str) -> Result<String, Box<dyn Error>> {
    let bytes = fs::read(scene.path("out").join(format!("{plugin}.json")))?;
    Ok(Sha256Digest::of(&bytes).to_string())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_clean_pr_writes_passing_documents_and_the_gates_output() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[("src/lib.rs", "pub fn two() -> u8 {\n    2\n}\n")])?;

    let output = scene.run(|_| {})?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\nweigh-in pass\n");
    assert_eq!(document(&scene, "temper")?, ["temper", RUN_KEY, "pass"]);
    assert_eq!(document(&scene, "weigh-in")?, ["weigh-in", RUN_KEY, "pass"]);
    assert_eq!(
        scene.recorded("output")?,
        format!(
            "gates=[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{}\"}},\
             {{\"plugin\":\"weigh-in\",\"conclusion\":\"pass\",\"sha256\":\"{}\"}}]\n",
            digest(&scene, "temper")?,
            digest(&scene, "weigh-in")?
        )
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn base_and_head_are_fetched_by_sha_with_the_run_token_only() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    let output = scene.run(|_| {})?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let remote = format!("https://github.com/{REPO}.git");
    // The remote and the refspec are a fetch's last two arguments.
    let fetched: Vec<String> = scene
        .recorded("git-bin/fetches")?
        .lines()
        .map(|line| line.rsplitn(3, ' ').take(2).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        fetched,
        [&scene.base_sha, &scene.head_sha]
            .map(|sha| format!("{sha}:refs/assay/fetched/{sha} {remote}"))
    );
    let header = format!("http.{remote}.extraHeader={RUN_TOKEN_HEADER}\n");
    assert_eq!(scene.recorded("git-bin/headers")?, header.repeat(2));
    let node = scene.recorded("bin/node-env")?;
    assert!(
        node.contains("ASSAY_RUN_KEY="),
        "the pinned scan ran: {node}"
    );
    assert!(!node.contains(RUN_TOKEN), "no gate sees the token: {node}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_pr_that_raises_the_baseline_writes_a_failing_temper_document() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[(BASELINE, &baseline(3))])?;

    let output = scene.run(|_| {})?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "the document speaks: {output:?}"
    );
    assert_eq!(stdout(&output), "temper fail\nweigh-in pass\n");
    assert_eq!(document(&scene, "temper")?, ["temper", RUN_KEY, "fail"]);
    Ok(())
}
