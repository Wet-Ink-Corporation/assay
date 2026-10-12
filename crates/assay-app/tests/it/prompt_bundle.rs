//! The reviewer prompt bundle (`docs/architecture/04-pipeline.md` §3, issue #90): the v1 files
//! in `plugin/prompts/`, the user prompt each role is sent, the roles' limits and the bundle
//! hash the evidence key takes.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use assay_app::{BundleFile, BundleFileError, BundleFiles, PromptBundle, Role};
use assay_domain::{ContentHash, Severity};

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
        combined: read(BundleFile::Instruction(Role::Combined))?,
        combined_micro: read(BundleFile::Instruction(Role::CombinedMicro))?,
    })
}

fn small_files() -> BundleFiles {
    BundleFiles {
        system: "SYSTEM".to_owned(),
        combined: "COMBINED".to_owned(),
        combined_micro: "MICRO".to_owned(),
    }
}

/// `files` with the character at `char_index` of `file` replaced by a different one.
fn with_char_changed(mut files: BundleFiles, file: BundleFile, char_index: usize) -> BundleFiles {
    let text = match file {
        BundleFile::System => &mut files.system,
        BundleFile::Instruction(Role::Combined) => &mut files.combined,
        BundleFile::Instruction(Role::CombinedMicro) => &mut files.combined_micro,
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
            (Role::Combined, Severity::Blocker),
            (Role::CombinedMicro, Severity::Major)
        ]
    );
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

    assert_eq!(parts, [("SYSTEM", "COMBINED"), ("SYSTEM", "MICRO")]);
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
        ("v1/combined.md", files.combined.as_bytes()),
        ("v1/combined-micro.md", files.combined_micro.as_bytes()),
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
    let BundleFiles {
        system,
        combined,
        combined_micro,
    } = small_files();
    let swapped = BundleFiles {
        system,
        combined: combined_micro,
        combined_micro: combined,
    };

    let (original, edited) = (
        PromptBundle::new(small_files())?.hash(),
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
