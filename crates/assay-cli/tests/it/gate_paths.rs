//! `assay gate`'s path refusals (#67 review): what it refuses of `--config`, `--head` and
//! `--out` before any step runs, and an `--out` a step made first. Every case is exit 2.

use std::error::Error;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

use crate::gate::{Job, temper};

/// A job whose one `test` step leaves `tests-ran` in the head tree.
fn marking() -> Result<Job, Box<dyn Error>> {
    Job::new(&temper(&[("tests", "test", "touch tests-ran")]))
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_out_a_step_made_first_is_refused_and_left_as_it_is() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[(
        "tests",
        "test",
        "mkdir ../out && echo x > ../out/forged.json",
    )]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        job.written()?,
        ["forged.json"],
        "nothing is written beside it"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_symlink_outside_the_head_tree_to_a_config_inside_it_is_refused() -> Result<(), Box<dyn Error>>
{
    let job = marking()?;
    fs::create_dir(job.head.join(".assay"))?;
    let inside = job.head.join(".assay/plugins.toml");
    fs::copy(&job.config, &inside)?;
    let link = job.dir.path().join("link.toml");
    symlink(&inside, &link)?;

    let output = job.run_with("gates-test", &link, Some(crate::gate::RUN_KEY))?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_fifo_or_a_directory_as_the_config_is_refused() -> Result<(), Box<dyn Error>> {
    let job = marking()?;
    let fifo = job.dir.path().join("fifo.toml");
    let made = Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo: {made}");
    let directory = job.dir.path().join("dir.toml");
    fs::create_dir(&directory)?;

    for config in [&fifo, &directory] {
        let output = job.run_with("gates-test", config, Some(crate::gate::RUN_KEY))?;

        assert_eq!(
            output.status.code(),
            Some(2),
            "{}: {output:?}",
            config.display()
        );
    }
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_file_as_the_head_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let mut job = marking()?;
    let file = job.dir.path().join("head.txt");
    fs::write(&file, "not a tree")?;
    job.head = file;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_dangling_symlink_at_out_is_refused() -> Result<(), Box<dyn Error>> {
    let job = marking()?;
    symlink(job.dir.path().join("nowhere"), &job.out)?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.dir.path().join("nowhere").exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_out_through_a_symlink_in_the_head_tree_is_refused() -> Result<(), Box<dyn Error>> {
    // `head/link` points outside the tree now; a test could point it back inside.
    let mut job = marking()?;
    let elsewhere = job.dir.path().join("elsewhere");
    fs::create_dir(&elsewhere)?;
    symlink(&elsewhere, job.head.join("link"))?;
    job.out = job.head.join("link/gates");

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!elsewhere.join("gates").exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_out_with_dot_dot_through_a_symlink_is_refused() -> Result<(), Box<dyn Error>> {
    // `hop/..` is `elsewhere` to the kernel and the job's directory to a lexical reading.
    let mut job = marking()?;
    let below = job.dir.path().join("elsewhere/below");
    fs::create_dir_all(&below)?;
    symlink(&below, job.dir.path().join("hop"))?;
    job.out = job.dir.path().join("hop/../gates");

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.dir.path().join("elsewhere/gates").exists());
    assert!(!job.dir.path().join("gates").exists());
    Ok(())
}
