//! `assay gate`'s argument parsing and per-job wiring (unit).

use super::*;

fn parsed(args: &[&str]) -> Result<Args, GateCliError> {
    parse(args.iter().map(OsString::from))
}

#[test]
fn the_job_and_the_flags_parse_in_any_order() {
    let args = parsed(&["gates-test", "--out", "o", "--config", "c", "--head", "h"]);

    assert!(
        matches!(
            args,
            Ok(Args { job: TEST, ref config, ref head, ref out, node: None, temper: None })
                if config == Path::new("c") && head == Path::new("h") && out == Path::new("o")
        ),
        "{args:?}"
    );
}

#[test]
fn every_gate_job_is_named() {
    let jobs: Vec<Option<GateJob>> = ["gates-static", "gates-lint", "gates-test"]
        .into_iter()
        .map(|job| {
            parsed(&[job, "--config", "c", "--head", "h", "--out", "o"])
                .ok()
                .map(|args| args.job)
        })
        .collect();

    assert_eq!(
        jobs,
        [
            Some(GateJob::Static),
            Some(GateJob::Build(BuildJob::Lint)),
            Some(TEST)
        ]
    );
}

#[test]
fn anything_else_is_a_usage_error() {
    let cases: [&[&str]; 7] = [
        &[],
        &["test", "--config", "c", "--head", "h", "--out", "o"],
        &["gates-test", "--config", "c", "--head", "h"],
        &["gates-test", "--config", "c", "--head", "h", "--out"],
        &[
            "gates-test",
            "--config",
            "c",
            "--config",
            "c",
            "--head",
            "h",
            "--out",
            "o",
        ],
        &[
            "gates-test",
            "--config",
            "c",
            "--head",
            "h",
            "--out",
            "o",
            "--base",
            "b",
        ],
        &["gates-test", "-c", "c", "--head", "h", "--out", "o"],
    ];

    for args in cases {
        assert!(matches!(parsed(args), Err(GateCliError::Usage)), "{args:?}");
    }
}

#[test]
fn the_program_flags_parse_and_repeating_one_is_a_usage_error() {
    let flags = ["--config", "c", "--head", "h", "--out", "o"];
    let programs = ["--temper", "/t", "--node", "/n"];

    let args = parsed(&[&["gates-static"][..], &flags, &programs].concat());
    let repeated = parsed(&[&["gates-static"][..], &flags, &programs, &["--node", "/m"]].concat());

    assert!(
        matches!(
            args,
            Ok(Args { job: GateJob::Static, node: Some(ref node), temper: Some(ref temper), .. })
                if node == Path::new("/n") && temper == Path::new("/t")
        ),
        "{args:?}"
    );
    assert!(matches!(repeated, Err(GateCliError::Usage)), "{repeated:?}");
}

fn wired(job: GateJob, node: Option<&str>, temper: Option<&str>) -> Result<Wired, GateCliError> {
    wire(job, node.map(PathBuf::from), temper.map(PathBuf::from))
}

#[test]
fn gates_static_takes_both_programs_by_absolute_path() {
    let wired = wired(GateJob::Static, Some("/bin/node"), Some("/opt/temper"));

    assert!(
        matches!(
            wired,
            Ok(Wired::Static(Programs { ref node, ref temper }))
                if node == Path::new("/bin/node") && temper == Path::new("/opt/temper")
        ),
        "{wired:?}"
    );
}

#[test]
fn a_program_flag_another_job_does_not_take_or_one_missing_is_a_usage_error() {
    let cases = [
        (GateJob::Static, None, Some("/opt/temper")),
        (GateJob::Static, Some("/bin/node"), None),
        (GateJob::Static, None, None),
        (TEST, Some("/bin/node"), None),
        (TEST, None, Some("/opt/temper")),
    ];

    for (job, node, temper) in cases {
        assert!(
            matches!(wired(job, node, temper), Err(GateCliError::Usage)),
            "{job} {node:?} {temper:?}"
        );
    }
}

#[test]
fn a_relative_program_is_refused_by_its_flag() {
    let refused = [
        wired(GateJob::Static, Some("node"), Some("/opt/temper")),
        wired(GateJob::Static, Some("/bin/node"), Some("../temper")),
    ];

    assert!(
        matches!(
            refused,
            [
                Err(GateCliError::Static(StaticError::RelativeProgram {
                    program: Program::Node,
                    path: ref node
                })),
                Err(GateCliError::Static(StaticError::RelativeProgram {
                    program: Program::Temper,
                    path: ref temper
                })),
            ] if node == Path::new("node") && temper == Path::new("../temper")
        ),
        "{refused:?}"
    );
}

#[test]
fn gates_lint_is_not_wired_whatever_its_flags() {
    for (node, temper) in [(None, None), (Some("/bin/node"), Some("/opt/temper"))] {
        assert!(
            matches!(
                wired(GateJob::Build(BuildJob::Lint), node, temper),
                Err(GateCliError::NotWired(GateJob::Build(BuildJob::Lint)))
            ),
            "{node:?} {temper:?}"
        );
    }
}
