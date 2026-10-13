//! The built-in allowlist pinned next to `claude_cli` in `.assay/plugins.toml` (03 §1, §8).

use std::error::Error;

use assay_app::{BuiltinKind, BuiltinsError, ConfigError, MAX_BUILTINS, PluginsConfig};

const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// `[assay]` with the CLI pin and `allowlist` as the `claude_cli_builtins` value.
fn with_allowlist(allowlist: &str) -> String {
    format!(
        "[assay]\ndigest = \"sha256:{HEX64}\"\nclaude_cli = \"2.1.296\"\nclaude_cli_builtins = {allowlist}\n"
    )
}

/// An allowlist with `skills` as its skill list and one plugin and agent.
fn with_skills(skills: &str) -> String {
    with_allowlist(&format!(
        "{{ plugins = [\"cc-plugin-telemetry\"], skills = {skills}, agents = [\"Explore\"] }}"
    ))
}

fn parse(text: &str) -> Result<PluginsConfig, ConfigError> {
    text.parse()
}

/// A TOML array of `count` distinct skill names.
fn skill_list(count: usize) -> String {
    let names: Vec<String> = (0..count).map(|n| format!("\"skill-{n}\"")).collect();
    format!("[{}]", names.join(", "))
}

#[test]
fn parses_the_allowlist_next_to_the_cli_pin() -> Result<(), Box<dyn Error>> {
    let config = parse(&with_allowlist(
        "{ plugins = [\"cc-plugin-agents-md\", \"cc-plugin-telemetry\"], skills = [\"debug\"], agents = [\"Explore\", \"general-purpose\"] }",
    ))?;

    let allowlist = &config.assay().claude_cli_builtins;
    let names = |kind| allowlist.names(kind).collect::<Vec<_>>();
    assert_eq!(
        names(BuiltinKind::Plugin),
        ["cc-plugin-agents-md", "cc-plugin-telemetry"]
    );
    assert_eq!(names(BuiltinKind::Skill), ["debug"]);
    assert_eq!(names(BuiltinKind::Agent), ["Explore", "general-purpose"]);
    assert!(allowlist.contains(BuiltinKind::Skill, "debug"));
    assert!(!allowlist.contains(BuiltinKind::Plugin, "debug"));
    Ok(())
}

#[test]
fn an_absent_allowlist_admits_no_built_in() -> Result<(), Box<dyn Error>> {
    let text = format!("[assay]\ndigest = \"sha256:{HEX64}\"\nclaude_cli = \"2.1.296\"\n");

    let config = parse(&text)?;

    let allowlist = &config.assay().claude_cli_builtins;
    for kind in BuiltinKind::ALL {
        assert_eq!(allowlist.names(kind).count(), 0, "{kind}");
    }
    Ok(())
}

#[test]
fn refuses_an_allowlist_without_a_cli_pin() {
    let text = format!(
        "[assay]\ndigest = \"sha256:{HEX64}\"\nclaude_cli_builtins = {{ plugins = [], skills = [], agents = [] }}\n"
    );

    let parsed = parse(&text);

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Builtins(BuiltinsError::WithoutCli))
        ),
        "{parsed:?}"
    );
}

#[test]
fn refuses_a_name_outside_the_name_characters() {
    let parsed = parse(&with_skills("[\"debug\", \"../evil\"]"));

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Builtins(BuiltinsError::Name { kind: BuiltinKind::Skill, ref name }))
                if name == "../evil"
        ),
        "{parsed:?}"
    );
}

#[test]
fn refuses_an_empty_name() {
    let parsed = parse(&with_skills("[\"\"]"));

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Builtins(BuiltinsError::Name { kind: BuiltinKind::Skill, ref name }))
                if name.is_empty()
        ),
        "{parsed:?}"
    );
}

#[test]
fn accepts_a_name_at_the_length_cap_and_refuses_one_byte_more() -> Result<(), Box<dyn Error>> {
    let at_cap = "a".repeat(128);
    let over = "a".repeat(129);

    let accepted = parse(&with_skills(&format!("[\"{at_cap}\"]")))?;
    let refused = parse(&with_skills(&format!("[\"{over}\"]")));

    assert!(
        accepted
            .assay()
            .claude_cli_builtins
            .contains(BuiltinKind::Skill, &at_cap)
    );
    assert!(
        matches!(
            refused,
            Err(ConfigError::Builtins(BuiltinsError::Name { kind: BuiltinKind::Skill, ref name }))
                if *name == over
        ),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn refuses_a_repeated_name() {
    let parsed = parse(&with_skills("[\"debug\", \"loop\", \"debug\"]"));

    assert!(
        matches!(
            parsed,
            Err(ConfigError::Builtins(BuiltinsError::Repeated { kind: BuiltinKind::Skill, ref name }))
                if name == "debug"
        ),
        "{parsed:?}"
    );
}

#[test]
fn accepts_a_list_at_the_entry_cap_and_refuses_one_more() -> Result<(), Box<dyn Error>> {
    let accepted = parse(&with_skills(&skill_list(MAX_BUILTINS)))?;
    let refused = parse(&with_skills(&skill_list(MAX_BUILTINS + 1)));

    assert_eq!(
        accepted
            .assay()
            .claude_cli_builtins
            .names(BuiltinKind::Skill)
            .count(),
        256
    );
    assert!(
        matches!(
            refused,
            Err(ConfigError::Builtins(BuiltinsError::TooMany {
                kind: BuiltinKind::Skill,
                count: 257
            }))
        ),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn refuses_an_unknown_key_in_the_allowlist() {
    let parsed = parse(&with_allowlist(
        "{ plugins = [], skills = [], agents = [], mcp_servers = [] }",
    ));

    assert!(matches!(parsed, Err(ConfigError::Syntax(_))), "{parsed:?}");
}

#[test]
fn refuses_an_allowlist_missing_a_list() {
    let parsed = parse(&with_allowlist("{ plugins = [], skills = [] }"));

    assert!(matches!(parsed, Err(ConfigError::Syntax(_))), "{parsed:?}");
}
