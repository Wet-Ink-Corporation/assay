//! The reviewer prompt bundle (`docs/architecture/04-pipeline.md` §3, issue #90): the v1 files
//! in `plugin/prompts/`, the user prompt each role is sent, the roles' limits and the bundle
//! hash the evidence key takes.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use assay_app::{BundleFile, BundleFileError, BundleFiles, PromptBundle, Role};
use assay_domain::{ContentHash, RequiredRole, Severity, VerifierVerdict};

fn prompts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/prompts")
}

fn read(file: BundleFile) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(prompts_dir().join(file.path()))?)
}

/// The bundle files as checked in.
fn v1_files() -> Result<BundleFiles, Box<dyn Error>> {
    Ok(BundleFiles {
        system: read(BundleFile::System)?,
        bugs: read(BundleFile::Instruction(Role::Bugs))?,
        security: read(BundleFile::Instruction(Role::Security))?,
        design: read(BundleFile::Instruction(Role::Design))?,
        meta: read(BundleFile::Instruction(Role::Meta))?,
        combined: read(BundleFile::Instruction(Role::Combined))?,
        combined_micro: read(BundleFile::Instruction(Role::CombinedMicro))?,
        verifier: read(BundleFile::Verifier)?,
    })
}

fn small_files() -> BundleFiles {
    BundleFiles {
        system: "SYSTEM".to_owned(),
        bugs: "BUGS".to_owned(),
        security: "SECURITY".to_owned(),
        design: "DESIGN".to_owned(),
        meta: "META".to_owned(),
        combined: "COMBINED".to_owned(),
        combined_micro: "MICRO".to_owned(),
        verifier: "VERIFIER".to_owned(),
    }
}

/// `files` with the character at `char_index` of `file` replaced by a different one.
fn with_char_changed(mut files: BundleFiles, file: BundleFile, char_index: usize) -> BundleFiles {
    let text = match file {
        BundleFile::System => &mut files.system,
        BundleFile::Instruction(Role::Bugs) => &mut files.bugs,
        BundleFile::Instruction(Role::Security) => &mut files.security,
        BundleFile::Instruction(Role::Design) => &mut files.design,
        BundleFile::Instruction(Role::Meta) => &mut files.meta,
        BundleFile::Instruction(Role::Combined) => &mut files.combined,
        BundleFile::Instruction(Role::CombinedMicro) => &mut files.combined_micro,
        BundleFile::Verifier => &mut files.verifier,
    };
    *text = text
        .chars()
        .enumerate()
        .map(|(index, c)| match (index == char_index, c) {
            (false, c) => c,
            (true, 'x') => 'y',
            (true, _) => 'x',
        })
        .collect();
    files
}

/// The line of 04 §3's roster table for `role`: `(max turns, cap in micro-AEU)`.
fn roster_row(role: Role) -> Result<(u8, u64), Box<dyn Error>> {
    let spec = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/architecture/04-pipeline.md"),
    )?;
    let prefix = format!("| `{}` |", role.code());
    let line = spec
        .lines()
        .find(|line| line.starts_with(&prefix))
        .ok_or_else(|| format!("04 §3 has no roster row for {}", role.code()))?;
    let cells: Vec<&str> = line.split('|').map(str::trim).collect();
    let [_, _, _, _, turns, cap, ..] = cells.as_slice() else {
        return Err(format!("short roster row: {line}").into());
    };
    let (dollars, cents) = cap
        .strip_prefix('$')
        .and_then(|cap| cap.split_once('.'))
        .ok_or_else(|| format!("cap {cap} is not $d.cc"))?;
    let micros = dollars.parse::<u64>()? * 1_000_000 + cents.parse::<u64>()? * 10_000;
    Ok((turns.parse()?, micros))
}

#[test]
fn each_role_has_the_turns_and_cap_of_the_04_roster() -> Result<(), Box<dyn Error>> {
    for role in Role::ALL {
        let expected = roster_row(role)?;

        let actual = (role.max_turns().get(), role.cap().micros());

        assert_eq!(actual, expected, "{} limits, 04 §3", role.code());
    }
    Ok(())
}

#[test]
fn only_combined_micro_findings_are_capped_at_major() {
    let ceilings = Role::ALL.map(|role| (role, role.severity_ceiling()));

    assert_eq!(
        ceilings,
        [
            (Role::Bugs, Severity::Blocker),
            (Role::Security, Severity::Blocker),
            (Role::Design, Severity::Blocker),
            (Role::Meta, Severity::Blocker),
            (Role::Combined, Severity::Blocker),
            (Role::CombinedMicro, Severity::Major)
        ]
    );
}

#[test]
fn every_required_role_has_a_bundle_role_with_its_code_and_back() {
    let round_trip = RequiredRole::ALL.map(|required| {
        let role = Role::from(required);
        (required.code(), role.code(), RequiredRole::from(role))
    });

    assert_eq!(
        round_trip,
        RequiredRole::ALL.map(|required| (required.code(), required.code(), required))
    );
}

#[test]
fn only_design_and_meta_see_the_pull_requests_intent() {
    let seeing: Vec<Role> = Role::ALL
        .into_iter()
        .filter(|role| role.sees_intent())
        .collect();

    assert_eq!(seeing, [Role::Design, Role::Meta]);
}

#[test]
fn the_user_prompt_is_the_context_pack_then_the_role_instruction() -> Result<(), Box<dyn Error>> {
    let bundle = PromptBundle::new(small_files())?;

    let prompt = bundle.user_prompt(Role::CombinedMicro, "PACK")?;

    assert_eq!(prompt.as_str(), "PACK\n\nMICRO");
    Ok(())
}

#[test]
fn every_role_is_sent_the_same_system_prompt_and_its_own_instruction() -> Result<(), Box<dyn Error>>
{
    let bundle = PromptBundle::new(small_files())?;

    let parts = Role::ALL.map(|role| (bundle.system_prompt(), bundle.instruction(role)));

    assert_eq!(
        parts,
        [
            ("SYSTEM", "BUGS"),
            ("SYSTEM", "SECURITY"),
            ("SYSTEM", "DESIGN"),
            ("SYSTEM", "META"),
            ("SYSTEM", "COMBINED"),
            ("SYSTEM", "MICRO")
        ]
    );
    Ok(())
}

#[test]
fn an_empty_file_is_refused_by_name_and_the_files_are_handed_back() -> Result<(), Box<dyn Error>> {
    let files = BundleFiles {
        combined: String::new(),
        ..small_files()
    };

    // A copy, to compare with what the error hands back.
    let refused = PromptBundle::new(files.clone());

    let error = refused
        .err()
        .ok_or("an empty role instruction was accepted")?;
    assert_eq!(
        (error.file(), error.reason()),
        (
            BundleFile::Instruction(Role::Combined),
            BundleFileError::Empty
        )
    );
    assert_eq!(error.into_files(), files);
    Ok(())
}

#[test]
fn a_whitespace_only_file_is_refused_as_empty() -> Result<(), Box<dyn Error>> {
    let files = BundleFiles {
        combined_micro: " \n\t\n".to_owned(),
        ..small_files()
    };

    let refused = PromptBundle::new(files);

    let error = refused.err().ok_or("a whitespace-only file was accepted")?;
    assert_eq!(
        (error.file(), error.reason()),
        (
            BundleFile::Instruction(Role::CombinedMicro),
            BundleFileError::Empty
        )
    );
    Ok(())
}

#[test]
fn an_empty_verifier_instruction_is_refused_by_name() -> Result<(), Box<dyn Error>> {
    let files = BundleFiles {
        verifier: String::new(),
        ..small_files()
    };

    let refused = PromptBundle::new(files);

    let error = refused
        .err()
        .ok_or("an empty verifier instruction was accepted")?;
    assert_eq!(
        (error.file(), error.reason()),
        (BundleFile::Verifier, BundleFileError::Empty)
    );
    Ok(())
}

#[test]
fn the_verifier_is_sent_its_own_instruction() -> Result<(), Box<dyn Error>> {
    let bundle = PromptBundle::new(small_files())?;

    let instruction = bundle.verifier_instruction();

    assert_eq!(instruction, "VERIFIER");
    Ok(())
}

#[test]
fn a_nul_byte_is_refused_by_name() -> Result<(), Box<dyn Error>> {
    let files = BundleFiles {
        system: "SYS\0TEM".to_owned(),
        ..small_files()
    };

    let refused = PromptBundle::new(files);

    let error = refused.err().ok_or("a NUL byte was accepted")?;
    assert_eq!(
        (error.file(), error.reason()),
        (BundleFile::System, BundleFileError::Nul)
    );
    Ok(())
}

#[test]
fn the_bundle_hash_is_the_content_hash_of_its_files_under_their_paths() -> Result<(), Box<dyn Error>>
{
    let files = v1_files()?;
    let expected = ContentHash::of_files([
        ("v1/system.md", files.system.as_bytes()),
        ("v1/bugs.md", files.bugs.as_bytes()),
        ("v1/security.md", files.security.as_bytes()),
        ("v1/design.md", files.design.as_bytes()),
        ("v1/meta.md", files.meta.as_bytes()),
        ("v1/combined.md", files.combined.as_bytes()),
        ("v1/combined-micro.md", files.combined_micro.as_bytes()),
        ("v1/verifier.md", files.verifier.as_bytes()),
    ]);

    let bundle = PromptBundle::new(files)?;

    assert_eq!(bundle.hash(), expected);
    Ok(())
}

#[test]
fn changing_any_one_character_of_any_file_changes_the_bundle_hash() -> Result<(), Box<dyn Error>> {
    let files = v1_files()?;
    // A copy: `files` stays the base every edit below starts from.
    let original = PromptBundle::new(files.clone())?.hash();

    for file in BundleFile::ALL {
        let len = read(file)?.chars().count();
        for char_index in [0, len / 2, len - 1] {
            // Each edit starts from its own copy of the unedited files.
            let edited = with_char_changed(files.clone(), file, char_index);

            let hash = PromptBundle::new(edited)?.hash();

            assert_ne!(hash, original, "{} char {char_index}", file.path());
        }
    }
    Ok(())
}

#[test]
fn swapping_two_role_instructions_changes_the_bundle_hash() -> Result<(), Box<dyn Error>> {
    let files = small_files();
    // A copy: `files` is the unswapped original the hash is compared with.
    let swapped = BundleFiles {
        combined: files.combined_micro.clone(),
        combined_micro: files.combined.clone(),
        ..files.clone()
    };

    let (original, edited) = (
        PromptBundle::new(files)?.hash(),
        PromptBundle::new(swapped)?.hash(),
    );

    assert_ne!(original, edited);
    Ok(())
}

/// `(name, definition)` of each severity in 04 §3's "Severity, operationally" paragraph.
fn spec_severity_definitions() -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let spec = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/architecture/04-pipeline.md"),
    )?;
    let line = spec
        .lines()
        .find(|line| line.starts_with("**Severity, operationally**"))
        .ok_or("04 §3 has no severity paragraph")?;
    let (_, list) = line
        .split_once("): ")
        .ok_or("severity paragraph has no list")?;
    let (list, _) = list
        .split_once(". From")
        .ok_or("severity list has no end")?;
    list.split("; ")
        .map(|entry| {
            let (name, definition) = entry
                .split_once(" = ")
                .ok_or_else(|| format!("no definition in {entry}"))?;
            Ok((name.trim_matches('`').to_owned(), definition.to_owned()))
        })
        .collect()
}

#[test]
fn the_v1_system_prompt_defines_every_severity_as_04_does() -> Result<(), Box<dyn Error>> {
    let definitions = spec_severity_definitions()?;
    let names: BTreeSet<&str> = definitions.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(Severity::ALL.map(Severity::code)));

    let bundle = PromptBundle::new(v1_files()?)?;

    for (name, definition) in &definitions {
        let line = format!("- **{name}**: {definition}.");
        assert!(
            bundle.system_prompt().lines().any(|text| text == line),
            "system prompt lacks {line:?}"
        );
    }
    Ok(())
}

#[test]
fn the_v1_system_prompt_lists_what_not_to_flag_and_no_instruction_repeats_it()
-> Result<(), Box<dyn Error>> {
    let heading = "## What not to flag";

    let bundle = PromptBundle::new(v1_files()?)?;

    assert!(
        bundle.system_prompt().lines().any(|line| line == heading),
        "no {heading:?} section"
    );
    for role in Role::ALL {
        assert!(
            !bundle.instruction(role).contains(heading),
            "{} repeats the shared list",
            role.code()
        );
    }
    Ok(())
}

#[test]
fn the_v1_what_not_to_flag_list_has_one_bullet_per_04_item() -> Result<(), Box<dyn Error>> {
    let spec = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/architecture/04-pipeline.md"),
    )?;
    let paragraph = spec
        .lines()
        .find(|line| line.starts_with("**\"What not to flag\"**"))
        .ok_or("04 §3 has no what-not-to-flag paragraph")?;
    let (_, list) = paragraph
        .split_once("): ")
        .ok_or("the paragraph has no list")?;
    let (list, _) = list.split_once(". ").ok_or("the list has no end")?;
    let items = list.split("; ").count();

    let bundle = PromptBundle::new(v1_files()?)?;

    let bullets = bundle
        .system_prompt()
        .lines()
        .skip_while(|line| *line != "## What not to flag")
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter(|line| line.starts_with("- "))
        .count();
    assert_eq!(bullets, items, "bullets of the shared list, 04 §3");
    Ok(())
}

#[test]
fn the_v1_verifier_instruction_names_every_verdict() -> Result<(), Box<dyn Error>> {
    let bundle = PromptBundle::new(v1_files()?)?;

    let instruction = bundle.verifier_instruction();

    for verdict in VerifierVerdict::ALL {
        let code = format!("`{}`", verdict.code());
        assert!(instruction.contains(&code), "verifier.md lacks {code}");
    }
    Ok(())
}

#[test]
fn the_v1_combined_micro_instruction_states_its_ceiling() -> Result<(), Box<dyn Error>> {
    let ceiling = format!(
        "capped at `{}`",
        Role::CombinedMicro.severity_ceiling().code()
    );

    let bundle = PromptBundle::new(v1_files()?)?;

    assert!(
        bundle.instruction(Role::CombinedMicro).contains(&ceiling),
        "combined-micro does not say {ceiling:?}"
    );
    Ok(())
}

#[test]
fn every_v1_instruction_opens_with_its_role_heading_and_states_its_turns()
-> Result<(), Box<dyn Error>> {
    let bundle = PromptBundle::new(v1_files()?)?;

    for role in Role::ALL {
        let instruction = bundle.instruction(role);

        let heading = format!("## Your role: {}", role.code());
        assert_eq!(instruction.lines().next(), Some(heading.as_str()));
        let turns = format!("{} turns", role.max_turns());
        assert!(instruction.contains(&turns), "{heading} lacks {turns:?}");
    }
    Ok(())
}

#[test]
fn every_v1_instruction_states_whether_the_role_sees_the_intent() -> Result<(), Box<dyn Error>> {
    let blind = "not given the pull request's title, description or commit messages";
    let envelope = "untrusted envelope";

    let bundle = PromptBundle::new(v1_files()?)?;

    for role in Role::ALL {
        let instruction = bundle.instruction(role);
        let says = (instruction.contains(blind), instruction.contains(envelope));
        assert_eq!(
            says,
            (!role.sees_intent(), role.sees_intent()),
            "{}: (blind, envelope)",
            role.code()
        );
    }
    Ok(())
}

#[test]
fn the_v1_full_tier_instructions_carry_their_roster_focus() -> Result<(), Box<dyn Error>> {
    let focus = [
        (Role::Bugs, "callers of every changed symbol"),
        (Role::Security, "hostile"),
        (Role::Design, "rule ID"),
        (Role::Meta, "base to head"),
    ];

    let bundle = PromptBundle::new(v1_files()?)?;

    for (role, phrase) in focus {
        assert!(
            bundle.instruction(role).contains(phrase),
            "{} lacks {phrase:?}",
            role.code()
        );
    }
    Ok(())
}

#[test]
fn the_v1_system_prompt_says_the_role_instruction_is_the_last_section() -> Result<(), Box<dyn Error>>
{
    let bundle = PromptBundle::new(v1_files()?)?;

    assert!(
        bundle
            .system_prompt()
            .contains("last section of the user message, under `## Your role:`"),
        "system.md does not say where the role instruction is"
    );
    Ok(())
}
