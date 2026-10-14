//! The `plan` use case (S1, `docs/architecture/04-pipeline.md` §2, 03 §4): `run.json` from
//! base config at the pinned `base_sha` and the PR's objects, against fakes of its ports.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    ArtifactJob, BaseConfigError, ChangedFiles, Contract, ContractValidator, EntryKind,
    FileListError, IncludeTargets, PlanError, PlannedRun, RestFile, plan,
};
use assay_domain::{FileChange, Lines, PrClass, Tier};
use serde_json::{Value, json};

use crate::plan_fakes::{
    BASE, CountingNonces, FakeError, FakePr, HEAD, PIN, plugins_toml, request, run_uid,
};

type Planned = Result<PlannedRun, PlanError<FakeError, FakeError>>;

fn plan_for(pr: &FakePr) -> Result<Planned, Box<dyn Error>> {
    let request = request(pr)?;
    Ok(plan(
        &request,
        pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    ))
}

fn document(run: &PlannedRun) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(run.json())?)
}

fn changes(files: Vec<FileChange>) -> ChangedFiles {
    ChangedFiles::Listed(files)
}

fn counted(path: &str, lines: u64) -> FileChange {
    FileChange::new(
        path,
        Lines::Counted {
            added: lines,
            removed: 0,
        },
    )
}

#[test]
fn a_small_change_plans_a_schema_valid_run() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();

    let run = plan_for(&pr)??;

    let validator = ContractValidator::new()?;
    assert_eq!(
        validator.read(Contract::Run, run.json())?,
        document(&run)?,
        "the document is valid assay.run/1 as written"
    );
    Ok(())
}

#[test]
fn the_run_pins_both_shas_and_carries_run_uid_and_base_ref() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();

    let run = plan_for(&pr)??;

    let doc = document(&run)?;
    assert_eq!(
        (
            &doc["base_sha"],
            &doc["head_sha"],
            &doc["base_ref"],
            &doc["repo"],
            &doc["pr"],
            &doc["run_uid"],
        ),
        (
            &json!(BASE),
            &json!(HEAD),
            &json!("main"),
            &json!("Wet-Ink-Corporation/assay"),
            &json!(42),
            &json!({"repo_id": 840_213_377, "run_id": 7, "run_attempt": 1}),
        )
    );
    Ok(())
}

#[test]
fn a_one_line_change_is_micro_with_its_reviewer_and_one_shard() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();

    let run = plan_for(&pr)??;

    let doc = document(&run)?;
    assert_eq!(
        (
            &doc["tier"],
            &doc["tier_reasons"],
            &doc["roles"],
            &doc["required_roles"],
            &doc["shards"],
            &doc["filtered_stats"],
            &doc["draft"],
        ),
        (
            &json!("micro"),
            &json!(["micro-size"]),
            &json!(["combined-micro"]),
            &json!(["combined-micro"]),
            &json!([{"files": ["src/lib.rs"]}]),
            &json!({"files": 1, "lines": 2, "excluded": []}),
            &json!(false),
        )
    );
    Ok(())
}

#[test]
fn plugins_come_from_the_base_config_not_the_head() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    // The PR rewrites the config to drop the required gate; plan must not believe it.
    let head_config = plugins_toml().replace("required = true", "required = false");
    pr.write(HEAD, ".assay/plugins.toml", head_config.as_bytes());

    let run = plan_for(&pr)??;

    assert_eq!(
        document(&run)?["plugins"],
        json!([
            {
                "name": "temper",
                "required": true,
                "pin": format!("git:{PIN}"),
                "steps": [{"name": "fmt", "job": "gates-lint", "timeout_s": 300}],
            },
            {"name": "custom-lint", "required": false, "job": "gates-test", "steps": []},
        ])
    );
    Ok(())
}

#[test]
fn a_base_without_plugins_config_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.remove(BASE, ".assay/plugins.toml");

    let planned = plan_for(&pr)?;

    assert_matches!(
        planned,
        Err(PlanError::BaseConfig(BaseConfigError::Missing))
    );
    Ok(())
}

#[test]
fn a_symlink_in_the_base_config_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.symlink(BASE, ".assay/owners", b"../owners");

    let planned = plan_for(&pr)?;

    assert_matches!(
        planned,
        Err(PlanError::BaseConfig(BaseConfigError::NotARegularFile {
            ref path,
            kind: EntryKind::Symlink,
        })) if path == ".assay/owners"
    );
    Ok(())
}

#[test]
fn a_base_plugins_config_that_is_not_utf8_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(BASE, ".assay/plugins.toml", b"[assay]\ndigest = \"\xff\"\n");

    let planned = plan_for(&pr)?;

    assert_matches!(
        planned,
        Err(PlanError::BaseConfig(BaseConfigError::NotUtf8 { ref path }))
            if path == ".assay/plugins.toml"
    );
    Ok(())
}

#[test]
fn an_invalid_base_plugins_config_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(
        BASE,
        ".assay/plugins.toml",
        b"[assay]\ndigest = \"latest\"\n",
    );

    let planned = plan_for(&pr)?;

    assert_matches!(
        planned,
        Err(PlanError::BaseConfig(BaseConfigError::Plugins(_)))
    );
    Ok(())
}

#[test]
fn an_invalid_base_globs_config_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(BASE, ".assay/globs.toml", b"lower = [\"**\"]\n");

    let planned = plan_for(&pr)?;

    assert_matches!(
        planned,
        Err(PlanError::BaseConfig(BaseConfigError::Globs(_)))
    );
    Ok(())
}

#[test]
fn a_rest_list_that_disagrees_with_git_fails_loudly() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    request.rest_files.files = vec![RestFile {
        path: "src/other.rs".to_owned(),
        previous_path: None,
    }];

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_matches!(
        planned,
        Err(PlanError::FileList(FileListError::Mismatch {
            only_in_git: 1,
            only_in_rest: 1,
            ..
        }))
    );
    Ok(())
}

#[test]
fn a_rest_list_shorter_than_the_prs_file_count_is_refused() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    request.rest_files.changed_files = 2;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_matches!(
        planned,
        Err(PlanError::FileList(FileListError::Incomplete {
            listed: 1,
            changed_files: 2,
        }))
    );
    Ok(())
}

#[test]
fn a_rest_list_over_its_cap_is_refused() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    request.rest_files.files = (0..3_001)
        .map(|index| RestFile {
            path: format!("f{index}"),
            previous_path: None,
        })
        .collect();
    request.rest_files.changed_files = 3_001;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_matches!(
        planned,
        Err(PlanError::FileList(FileListError::OverCap {
            listed: 3_001
        }))
    );
    Ok(())
}

#[test]
fn a_rest_list_naming_a_file_twice_is_refused() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    let twice = RestFile {
        path: "src/lib.rs".to_owned(),
        previous_path: None,
    };
    request.rest_files.files = vec![twice.clone(), twice];
    request.rest_files.changed_files = 2;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_matches!(planned, Err(PlanError::FileList(FileListError::Repeated)));
    Ok(())
}

/// 3,000 files listed, more changed: the REST list is truncated (04 §2).
fn truncated_rest(pr: &mut FakePr, rest_extra: Option<&str>) -> Vec<RestFile> {
    let files: Vec<FileChange> = (0..3_100)
        .map(|index| counted(&format!("docs/f{index}.md"), 1))
        .collect();
    let mut rest: Vec<RestFile> = files
        .iter()
        .take(3_000)
        .map(|file| RestFile {
            path: file.path.clone(),
            previous_path: None,
        })
        .collect();
    if let (Some(extra), Some(last)) = (rest_extra, rest.last_mut()) {
        extra.clone_into(&mut last.path);
    }
    pr.changes = changes(files);
    rest
}

#[test]
fn a_truncated_rest_list_is_accepted_when_git_lists_every_file_in_it() -> Result<(), Box<dyn Error>>
{
    let mut pr = FakePr::new();
    let rest = truncated_rest(&mut pr, None);
    let mut request = request(&pr)?;
    request.rest_files.files = rest;
    request.rest_files.changed_files = 3_100;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_eq!(
        planned.map(|run| run.tiering().filtered.files)?,
        3_100,
        "every git file is planned, not only the 3,000 REST lists"
    );
    Ok(())
}

#[test]
fn a_truncated_rest_list_naming_a_file_git_did_not_list_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    let rest = truncated_rest(&mut pr, Some("src/secret.rs"));
    let mut request = request(&pr)?;
    request.rest_files.files = rest;
    request.rest_files.changed_files = 3_100;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_matches!(
        planned,
        Err(PlanError::FileList(FileListError::Mismatch {
            only_in_git: 0,
            only_in_rest: 1,
            ..
        }))
    );
    Ok(())
}

#[test]
fn a_rename_only_one_side_detected_still_matches() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.changes = changes(vec![counted("src/new.rs", 0).renamed_from("src/old.rs")]);
    let mut request = request(&pr)?;
    // GitHub saw a deletion and an addition where git saw a rename.
    request.rest_files.files = ["src/old.rs", "src/new.rs"]
        .map(|path| RestFile {
            path: path.to_owned(),
            previous_path: None,
        })
        .into();
    request.rest_files.changed_files = 2;

    let planned = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    );

    assert_eq!(
        planned.map(|run| run.tiering().filtered.files)?,
        1,
        "the same paths on both sides are the same change"
    );
    Ok(())
}

#[test]
fn a_diff_past_its_cap_is_refused_as_needing_a_split() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.changes = ChangedFiles::TooSlow;

    let planned = plan_for(&pr)?;

    assert_matches!(planned, Err(PlanError::DiffTooSlow));
    Ok(())
}

#[test]
fn a_read_that_returns_fewer_blobs_than_asked_is_refused() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.short_reads = true;

    let planned = plan_for(&pr)?;

    assert_matches!(planned, Err(PlanError::ShortRead { .. }));
    Ok(())
}

#[test]
fn a_draft_schedules_no_reviewer_but_keeps_its_required_roles() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    request.pull_request.draft = true;

    let run = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    )?;

    let doc = document(&run)?;
    assert_eq!(
        (
            &doc["draft"],
            &doc["roles"],
            &doc["required_roles"],
            &doc["shards"]
        ),
        (
            &json!(true),
            &json!([]),
            &json!(["combined-micro"]),
            &json!([])
        ),
        "a draft runs gates only (04 §2.4); decide still knows what review it lacks"
    );
    Ok(())
}

#[test]
fn each_job_gets_its_own_nonce_artifact_name() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();

    let run = plan_for(&pr)??;

    let names = document(&run)?["artifact_names"].clone();
    let expected: serde_json::Map<String, Value> = ArtifactJob::ALL
        .into_iter()
        .zip(0_u32..)
        .map(|(job, index)| {
            (
                job.name().to_owned(),
                json!(format!("assay-{}-{index:032x}", job.name())),
            )
        })
        .collect();
    assert_eq!(names, Value::Object(expected));
    Ok(())
}

#[test]
fn a_nonce_source_that_repeats_itself_is_refused() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut stuck = CountingNonces {
        stuck: true,
        ..CountingNonces::default()
    };

    let planned = plan(&request(&pr)?, &pr, &mut stuck, &ContractValidator::new()?);

    assert_matches!(planned, Err(PlanError::RepeatedNonce { .. }));
    Ok(())
}

#[test]
fn attempts_of_the_same_evidence_share_the_run_key_but_not_the_run_uid()
-> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let first = request(&pr)?;
    let mut second = request(&pr)?;
    second.run_uid = run_uid(2)?;
    let validator = ContractValidator::new()?;

    let one = plan(&first, &pr, &mut CountingNonces::default(), &validator)?;
    let two = plan(&second, &pr, &mut CountingNonces::default(), &validator)?;

    assert_eq!(
        (
            one.run_key() == two.run_key(),
            document(&one)?["run_uid"] == document(&two)?["run_uid"]
        ),
        (true, false),
        "run_key is for reuse only; run_uid keys leases and ledger records (R3-16)"
    );
    Ok(())
}

#[test]
fn the_run_key_changes_with_base_config_prompt_bundle_and_head() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let validator = ContractValidator::new()?;
    let key = |pr: &FakePr, request: &assay_app::PlanRequest| {
        plan(request, pr, &mut CountingNonces::default(), &validator).map(|run| run.run_key())
    };
    let original = key(&pr, &request(&pr)?)?;

    let mut owners = pr.clone();
    owners.write(BASE, ".assay/owners", b"owners = []\n");
    let mut bundle = request(&pr)?;
    bundle.prompt_bundle = assay_domain::ContentHash::of_files([("prompt.md", b"v2".as_slice())]);
    let mut head = request(&pr)?;
    head.pull_request.head_sha = "cccccccccccccccccccccccccccccccccccccccc".parse()?;

    let keys = [
        key(&owners, &request(&owners)?)?,
        key(&pr, &bundle)?,
        key_for_head(&pr, &head, &validator)?,
    ];

    assert!(
        keys.iter().all(|changed| *changed != original),
        "{original} must change with any evidence input, got {keys:?}"
    );
    Ok(())
}

/// The run key of `request`, whose head is another commit holding the fake's head tree.
fn key_for_head(
    pr: &FakePr,
    request: &assay_app::PlanRequest,
    validator: &ContractValidator,
) -> Result<assay_app::EvidenceKey, Box<dyn Error>> {
    let moved = pr.at_head(request.pull_request.head_sha.as_str());
    Ok(plan(request, &moved, &mut CountingNonces::default(), validator)?.run_key())
}

#[test]
fn more_than_five_thousand_lines_plan_a_needs_split_run() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.changes = changes(vec![counted("src/lib.rs", 5_001)]);

    let run = plan_for(&pr)??;

    assert_eq!(
        (&document(&run)?["tier"], &document(&run)?["roles"]),
        (&json!("needs-split"), &json!([])),
        "the schema accepts every tier the domain computes"
    );
    Ok(())
}

#[test]
fn a_manifest_trust_key_change_is_a_trust_path() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(
        HEAD,
        "Cargo.toml",
        b"[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    pr.changes = changes(vec![counted("Cargo.toml", 1)]);
    let request = request(&pr)?;

    let run = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    )?;

    assert_eq!(
        (
            run.tiering().tier,
            run.tiering().classes.clone(),
            document(&run)?["trust_paths_changed"].clone(),
        ),
        (
            Tier::Full,
            vec![PrClass::TrustPath],
            json!(["Cargo.toml (edition)"]),
        )
    );
    Ok(())
}

#[test]
fn a_manifest_that_is_a_symlink_reads_as_unparseable() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    // A symlink's blob is its target's text, which here happens to be valid TOML.
    pr.symlink(HEAD, "Cargo.toml", b"a = 1");
    pr.changes = changes(vec![
        counted("Cargo.toml", 1).with_kind(assay_domain::FileKind::Symlink),
    ]);
    let request = request(&pr)?;

    let run = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    )?;

    assert!(
        run.tiering()
            .trust_paths_changed
            .contains(&"Cargo.toml (unparseable)".to_owned()),
        "a symlinked manifest is never read as TOML, got {:?}",
        run.tiering().trust_paths_changed
    );
    Ok(())
}

#[test]
fn a_docs_only_change_is_not_trivial_until_includes_are_scanned() -> Result<(), Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(HEAD, "README.md", b"# x, documented\n");
    pr.changes = changes(vec![counted("README.md", 1)]);
    let mut request = request(&pr)?;
    let validator = ContractValidator::new()?;
    let scanned = plan(&request, &pr, &mut CountingNonces::default(), &validator)?;
    request.include_targets = IncludeTargets::NotScanned;

    let unscanned = plan(&request, &pr, &mut CountingNonces::default(), &validator)?;

    assert_eq!(
        (scanned.tiering().tier, unscanned.tiering().tier),
        (Tier::Trivial, Tier::Micro),
        "without the include scan any file may be an include target (fail closed)"
    );
    Ok(())
}

#[test]
fn a_side_without_a_package_set_is_exec_surface() -> Result<(), Box<dyn Error>> {
    let pr = FakePr::new();
    let mut request = request(&pr)?;
    request.head_packages = None;

    let run = plan(
        &request,
        &pr,
        &mut CountingNonces::default(),
        &ContractValidator::new()?,
    )?;

    assert_eq!(
        (run.tiering().tier, run.tiering().classes.clone()),
        (Tier::Full, vec![PrClass::ExecSurface])
    );
    Ok(())
}
