//! `assay review --placeholder` end to end (#60 ST-9, D11 A): the binary with a cleared
//! environment holding only `GITHUB_OUTPUT`, its bytes afterwards and its exit codes (03 §2:
//! 0 written, 2 cannot run).

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// How the binary ended.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stderr: String,
}

/// Runs `assay review` with `args` and, when given, `GITHUB_OUTPUT` set to `output`.
fn review(args: &[&str], output: Option<&Path>) -> Result<Ran, Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
    command.arg("review").args(args).env_clear();
    if let Some(output) = output {
        command.env("GITHUB_OUTPUT", output);
    }
    let ran = command.output()?;
    Ok(Ran {
        code: ran.status.code(),
        stderr: String::from_utf8(ran.stderr)?,
    })
}

/// A scratch directory holding `output`, a `GITHUB_OUTPUT` with one earlier line.
fn scene() -> Result<TempDir, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("output"), "earlier=1\n")?;
    Ok(dir)
}

#[test]
fn the_placeholder_writes_not_run_with_no_cost_and_no_documents() -> Result<(), Box<dyn Error>> {
    let dir = scene()?;
    let output = dir.path().join("output");

    let ran = review(&["--placeholder"], Some(&output))?;

    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert_eq!(
        fs::read_to_string(&output)?,
        "earlier=1\nstage=not-run\ncost_aeu=0\ndocuments=[]\n"
    );
    Ok(())
}

#[test]
fn any_other_arguments_are_a_usage_error_and_write_nothing() -> Result<(), Box<dyn Error>> {
    for args in [
        &[][..],
        &["--placeholder", "extra"],
        &["--placeholder", "--placeholder"],
        &["--real"],
        &["placeholder"],
    ] {
        let dir = scene()?;
        let output = dir.path().join("output");

        let ran = review(args, Some(&output))?;

        assert_eq!(ran.code, Some(2), "{args:?}: {ran:?}");
        assert!(
            ran.stderr.contains("review --placeholder"),
            "{args:?}: {ran:?}"
        );
        assert_eq!(fs::read_to_string(&output)?, "earlier=1\n", "{args:?}");
    }
    Ok(())
}

#[test]
fn an_unset_github_output_is_refused() -> Result<(), Box<dyn Error>> {
    let ran = review(&["--placeholder"], None)?;

    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(ran.stderr.contains("GITHUB_OUTPUT is not set"), "{ran:?}");
    Ok(())
}

#[test]
fn a_missing_github_output_is_refused_and_not_created() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let output = dir.path().join("missing");

    let ran = review(&["--placeholder"], Some(&output))?;

    assert_eq!((ran.code, output.exists()), (Some(2), false), "{ran:?}");
    assert!(ran.stderr.contains("GITHUB_OUTPUT"), "{ran:?}");
    Ok(())
}

#[test]
fn a_github_output_that_is_not_a_regular_file_is_refused_before_it_is_opened()
-> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let fifo = dir.path().join("fifo-output");
    let made = Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo: {made}");

    // Opening the FIFO with no reader on its other end would block forever.
    for output in [fifo.as_path(), dir.path()] {
        let ran = review(&["--placeholder"], Some(output))?;

        assert_eq!(ran.code, Some(2), "{}: {ran:?}", output.display());
        assert!(
            ran.stderr.contains("not a regular file"),
            "{}: {ran:?}",
            output.display()
        );
    }
    Ok(())
}
