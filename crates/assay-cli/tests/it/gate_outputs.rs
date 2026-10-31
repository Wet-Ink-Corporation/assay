//! `assay gate gates-test`'s `gates` output (#60 ST-6, 03 §4a): one entry per scheduled plugin
//! in file order, appended to `GITHUB_OUTPUT` as one line, with the sha256 of the exact bytes
//! of each concluded gate's document; nothing is appended when the job cannot run.

use std::error::Error;
use std::fs;
use std::process::Command;

use assay_app::Sha256Digest;

use crate::gate::{Job, RUN_KEY, temper};

/// The sha256 of `<out>/<plugin>.json` as written.
fn digest(job: &Job, plugin: &str) -> Result<String, Box<dyn Error>> {
    let bytes = fs::read(job.out.join(format!("{plugin}.json")))?;
    Ok(Sha256Digest::of(&bytes).to_string())
}

/// An `exec` gate of the test job that writes a `neutral` document.
fn probe() -> String {
    let document = format!(
        r#"{{"schema":"assay.findings/1","source":{{"kind":"gate","name":"probe","version":"v","run_key":"{RUN_KEY}"}},"conclusion":"neutral","findings":[]}}"#
    );
    let script = format!("printf '%s' '{document}' > \"$ASSAY_FINDINGS_OUT\"");
    format!(
        "\n[[plugin]]\nname = \"probe\"\nadapter = \"exec\"\n\
         exec = [\"/bin/sh\", \"-c\", {script:?}]\njob = \"test\"\nrequired = false\n"
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn concluded_gates_report_the_sha256_of_their_documents_in_file_order() -> Result<(), Box<dyn Error>>
{
    let job = Job::new(&format!(
        "{}{}",
        temper(&[("tests", "test", "exit 0")]),
        probe()
    ))?;
    fs::write(&job.github_output, "earlier=1\n")?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        job.outputs()?,
        format!(
            "earlier=1\ngates=[{{\"plugin\":\"temper\",\"conclusion\":\"pass\",\"sha256\":\"{}\"}},\
             {{\"plugin\":\"probe\",\"conclusion\":\"neutral\",\"sha256\":\"{}\"}}]\n",
            digest(&job, "temper")?,
            digest(&job, "probe")?
        )
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_gate_error_is_reported_as_its_reason() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&format!(
        "{}{}",
        temper(&[("tests", "test", "kill -KILL $$")]),
        probe()
    ))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        job.outputs()?,
        format!(
            "gates=[{{\"plugin\":\"temper\",\"error\":\"gate-error:signal\"}},\
             {{\"plugin\":\"probe\",\"conclusion\":\"neutral\",\"sha256\":\"{}\"}}]\n",
            digest(&job, "probe")?
        )
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_job_with_nothing_scheduled_reports_an_empty_list() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("fmt", "lint", "exit 0")]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(job.outputs()?, "gates=[]\n");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_github_output_is_refused_before_any_step_runs() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;

    let output = job
        .command("gates-test", &job.config, Some(RUN_KEY))
        .output()?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("GITHUB_OUTPUT"), "{stderr}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_github_output_that_is_not_a_regular_file_is_refused_before_any_step_runs()
-> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    let fifo = job.dir.path().join("fifo-output");
    let made = Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo: {made}");
    let directory = job.dir.path().join("dir-output");
    fs::create_dir(&directory)?;
    let missing = job.dir.path().join("missing-output");

    // Opening the FIFO with no reader on its other end would block forever.
    for path in [&fifo, &directory, &missing] {
        let output = job
            .command("gates-test", &job.config, Some(RUN_KEY))
            .env("GITHUB_OUTPUT", path)
            .output()?;

        assert_eq!(
            output.status.code(),
            Some(2),
            "{}: {output:?}",
            path.display()
        );
    }
    assert!(!missing.exists(), "GITHUB_OUTPUT is never created");
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_job_refused_before_it_runs_appends_nothing() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    fs::create_dir(&job.out)?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(job.outputs()?, "");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_job_whose_documents_cannot_be_written_appends_nothing() -> Result<(), Box<dyn Error>> {
    // The step makes `--out` first, so creating it after the gates fails (`OutExists`).
    let job = Job::new(&temper(&[("tests", "test", "mkdir ../out")]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(job.outputs()?, "");
    Ok(())
}
