//! The clean-init rule of 03 §8: no MCP server, exactly the three read-only tools, and only
//! allowlisted built-ins, on the pinned CLI version.

use std::error::Error;

use assay_app::{
    AllowlistDrift, BuiltinAllowlist, BuiltinKind, ExactVersion, InitParts, InitPlugin,
    InitReadError, InitUnclean, PluginsConfig, SessionInit, allowlist_drift, check_init,
};
use assay_domain::BuiltinSet;

const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

const PLUGINS: [&str; 3] = [
    "cc-plugin-agents-md",
    "cc-plugin-telemetry",
    "cc-plugin-plugin-authoring",
];
const SKILLS: [&str; 19] = [
    "deep-research",
    "design",
    "design-sync",
    "dataviz",
    "update-config",
    "verify",
    "debug",
    "code-review",
    "simplify",
    "batch",
    "fewer-permission-prompts",
    "doctor",
    "loop",
    "schedule",
    "claude-api",
    "workflow-authoring",
    "run",
    "run-skill-generator",
    "plugin-authoring",
];
const AGENTS: [&str; 4] = ["claude", "Explore", "general-purpose", "Plan"];

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn toml_list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

/// The base config pinning 2.1.296 with its built-ins as observed on 2026-10-12.
fn pinned() -> Result<(ExactVersion, BuiltinAllowlist), Box<dyn Error>> {
    let text = format!(
        "[assay]\ndigest = \"sha256:{HEX64}\"\nclaude_cli = \"2.1.296\"\n\
         claude_cli_builtins = {{ plugins = {}, skills = {}, agents = {} }}\n",
        toml_list(&PLUGINS),
        toml_list(&SKILLS),
        toml_list(&AGENTS),
    );
    let config: PluginsConfig = text.parse()?;
    let assay = config.assay();
    let version = assay.claude_cli.clone().ok_or("no CLI pin")?;
    Ok((version, assay.claude_cli_builtins.clone()))
}

fn builtin(name: &str) -> InitPlugin {
    InitPlugin {
        name: name.to_owned(),
        path: "builtin".to_owned(),
        source: format!("{name}@builtin"),
    }
}

/// The parts of the 2.1.296 `system/init` captured under the 03 §8 profile.
fn observed() -> InitParts {
    InitParts {
        cli_version: "2.1.296".to_owned(),
        tools: strings(&["Glob", "Grep", "Read"]),
        mcp_servers: Vec::new(),
        plugins: PLUGINS.iter().map(|name| builtin(name)).collect(),
        skills: strings(&SKILLS),
        agents: strings(&AGENTS),
    }
}

fn check(parts: InitParts) -> Result<Result<(), InitUnclean>, Box<dyn Error>> {
    let (version, allowlist) = pinned()?;
    let init = SessionInit::new(parts)?;
    Ok(check_init(&init, &version, &allowlist))
}

#[test]
fn the_observed_2_1_296_init_is_clean_with_the_pinned_allowlist() -> Result<(), Box<dyn Error>> {
    assert_eq!(check(observed())?, Ok(()));
    Ok(())
}

#[test]
fn tools_are_compared_whatever_their_order() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        tools: strings(&["Read", "Grep", "Glob"]),
        ..observed()
    };

    assert_eq!(check(parts)?, Ok(()));
    Ok(())
}

#[test]
fn another_cli_version_is_unclean() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        cli_version: "2.1.297".to_owned(),
        ..observed()
    };

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::CliVersion {
            found: "2.1.297".to_owned(),
            pinned: "2.1.296".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn an_mcp_server_is_unclean() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        mcp_servers: strings(&["github"]),
        ..observed()
    };

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::McpServer {
            name: "github".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_extra_tool_is_unclean() -> Result<(), Box<dyn Error>> {
    let tools = strings(&["Bash", "Glob", "Grep", "Read"]);
    let parts = InitParts {
        tools: tools.clone(),
        ..observed()
    };

    assert_eq!(check(parts)?, Err(InitUnclean::Tools { found: tools }));
    Ok(())
}

#[test]
fn a_missing_tool_is_unclean() -> Result<(), Box<dyn Error>> {
    let tools = strings(&["Glob", "Read"]);
    let parts = InitParts {
        tools: tools.clone(),
        ..observed()
    };

    assert_eq!(check(parts)?, Err(InitUnclean::Tools { found: tools }));
    Ok(())
}

#[test]
fn a_repeated_tool_is_unclean() -> Result<(), Box<dyn Error>> {
    let tools = strings(&["Glob", "Grep", "Read", "Read"]);
    let parts = InitParts {
        tools: tools.clone(),
        ..observed()
    };

    assert_eq!(check(parts)?, Err(InitUnclean::Tools { found: tools }));
    Ok(())
}

#[test]
fn an_allowlisted_plugin_loaded_from_a_folder_is_not_a_built_in() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(InitPlugin {
        path: "/home/runner/.claude/plugins/cc-plugin-telemetry".to_owned(),
        ..builtin("cc-plugin-telemetry")
    });

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::NotBuiltin {
            name: "cc-plugin-telemetry".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_plugin_from_a_marketplace_is_not_a_built_in() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(InitPlugin {
        source: "planted@evil-marketplace".to_owned(),
        ..builtin("planted")
    });

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::NotBuiltin {
            name: "planted".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_built_in_source_naming_another_plugin_is_not_a_built_in() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(InitPlugin {
        source: "cc-plugin-telemetry@builtin".to_owned(),
        ..builtin("planted")
    });

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::NotBuiltin {
            name: "planted".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_extra_built_in_plugin_is_unclean() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(builtin("cc-plugin-new"));

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Plugin,
            name: "cc-plugin-new".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_skill_not_on_the_list_is_unclean() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.skills.push("planted-skill".to_owned());

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Skill,
            name: "planted-skill".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_agent_not_on_the_list_is_unclean() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.agents.push("planted-agent".to_owned());

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Agent,
            name: "planted-agent".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_repeated_built_in_is_unclean() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.skills.push("debug".to_owned());

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::Repeated {
            kind: BuiltinKind::Skill,
            name: "debug".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_repeated_built_in_plugin_is_unclean() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(builtin("cc-plugin-telemetry"));

    assert_eq!(
        check(parts)?,
        Err(InitUnclean::Repeated {
            kind: BuiltinKind::Plugin,
            name: "cc-plugin-telemetry".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_empty_allowlist_makes_every_built_in_unclean() -> Result<(), Box<dyn Error>> {
    let (version, _) = pinned()?;
    let init = SessionInit::new(observed())?;

    let checked = check_init(&init, &version, &BuiltinAllowlist::default());

    assert_eq!(
        checked,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Plugin,
            name: "cc-plugin-agents-md".to_owned()
        })
    );
    Ok(())
}

#[test]
fn the_unclean_reason_quotes_names_escaped() {
    let unclean = InitUnclean::Unlisted {
        kind: BuiltinKind::Skill,
        name: "a\nb".to_owned(),
    };

    assert_eq!(
        unclean.to_string(),
        "skill \"a\\nb\" is not on the built-in allowlist"
    );
}

#[test]
fn the_builtin_set_lists_names_as_observed() -> Result<(), Box<dyn Error>> {
    let init = SessionInit::new(observed())?;

    assert_eq!(
        init.builtin_set(),
        BuiltinSet {
            plugins: strings(&PLUGINS),
            skills: strings(&SKILLS),
            agents: strings(&AGENTS),
        }
    );
    Ok(())
}

#[test]
fn no_drift_when_the_observed_built_ins_are_the_allowlist() -> Result<(), Box<dyn Error>> {
    let (_, allowlist) = pinned()?;
    let init = SessionInit::new(observed())?;

    assert_eq!(allowlist_drift(&init, &allowlist), None);
    Ok(())
}

#[test]
fn drift_reports_unlisted_and_unobserved_names_sorted() -> Result<(), Box<dyn Error>> {
    let (_, allowlist) = pinned()?;
    let mut parts = observed();
    parts
        .skills
        .retain(|skill| skill != "debug" && skill != "batch");
    parts.skills.push("zeta-new".to_owned());
    parts.skills.push("alpha-new".to_owned());
    parts.plugins.push(builtin("cc-plugin-new"));
    let init = SessionInit::new(parts)?;

    let drift = allowlist_drift(&init, &allowlist);

    assert_eq!(
        drift,
        Some(AllowlistDrift {
            unlisted: BuiltinSet {
                plugins: strings(&["cc-plugin-new"]),
                skills: strings(&["alpha-new", "zeta-new"]),
                agents: Vec::new(),
            },
            unobserved: BuiltinSet {
                plugins: Vec::new(),
                skills: strings(&["batch", "debug"]),
                agents: Vec::new(),
            },
        })
    );
    Ok(())
}

#[test]
fn drift_reads_as_a_diff() {
    let drift = AllowlistDrift {
        unlisted: BuiltinSet {
            plugins: strings(&["cc-plugin-new"]),
            skills: Vec::new(),
            agents: Vec::new(),
        },
        unobserved: BuiltinSet {
            plugins: Vec::new(),
            skills: strings(&["debug"]),
            agents: strings(&["Plan"]),
        },
    };

    assert_eq!(
        drift.to_string(),
        "+ plugin \"cc-plugin-new\", - skill \"debug\", - agent \"Plan\""
    );
}

fn names(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("skill-{n}")).collect()
}

#[test]
fn accepts_a_list_at_the_entry_cap_and_refuses_one_more() {
    let at_cap = SessionInit::new(InitParts {
        skills: names(256),
        ..observed()
    });
    let over = SessionInit::new(InitParts {
        skills: names(257),
        ..observed()
    });

    assert_eq!(at_cap.map(|init| init.skills().len()), Ok(256));
    assert_eq!(
        over.map(|init| init.skills().len()),
        Err(InitReadError::TooMany {
            field: "skills",
            count: 257
        })
    );
}

#[test]
fn refuses_too_many_entries_in_any_list() {
    let over = || names(257);
    let cases: [(&str, InitParts); 5] = [
        (
            "tools",
            InitParts {
                tools: over(),
                ..observed()
            },
        ),
        (
            "mcp_servers",
            InitParts {
                mcp_servers: over(),
                ..observed()
            },
        ),
        (
            "plugins",
            InitParts {
                plugins: over().iter().map(|name| builtin(name)).collect(),
                ..observed()
            },
        ),
        (
            "agents",
            InitParts {
                agents: over(),
                ..observed()
            },
        ),
        (
            "skills",
            InitParts {
                skills: over(),
                ..observed()
            },
        ),
    ];

    for (field, parts) in cases {
        assert_eq!(
            SessionInit::new(parts).map(|_| ()),
            Err(InitReadError::TooMany { field, count: 257 }),
            "{field}"
        );
    }
}

#[test]
fn accepts_a_name_at_the_length_cap_and_refuses_one_byte_more() {
    let at_cap = SessionInit::new(InitParts {
        skills: vec!["s".repeat(128)],
        ..observed()
    });
    let over = SessionInit::new(InitParts {
        skills: vec!["s".repeat(129)],
        ..observed()
    });

    assert_eq!(at_cap.map(|init| init.skills().len()), Ok(1));
    assert_eq!(
        over.map(|_| ()),
        Err(InitReadError::Name {
            field: "skills",
            index: 0
        })
    );
}

#[test]
fn refuses_an_empty_name_or_one_with_a_control_character() {
    let empty = SessionInit::new(InitParts {
        agents: strings(&["Explore", ""]),
        ..observed()
    });
    let control = SessionInit::new(InitParts {
        mcp_servers: strings(&["a\u{1b}[2Jb"]),
        ..observed()
    });

    assert_eq!(
        empty.map(|_| ()),
        Err(InitReadError::Name {
            field: "agents",
            index: 1
        })
    );
    assert_eq!(
        control.map(|_| ()),
        Err(InitReadError::Name {
            field: "mcp_servers",
            index: 0
        })
    );
}

#[test]
fn refuses_a_malformed_plugin_entry_or_version() {
    let mut plugins = observed().plugins;
    plugins.push(InitPlugin {
        path: String::new(),
        ..builtin("x")
    });
    let plugin = SessionInit::new(InitParts {
        plugins,
        ..observed()
    });
    let version = SessionInit::new(InitParts {
        cli_version: String::new(),
        ..observed()
    });
    let tool = SessionInit::new(InitParts {
        tools: strings(&["Glob", "Grep\n", "Read"]),
        ..observed()
    });

    assert_eq!(
        plugin.map(|_| ()),
        Err(InitReadError::Name {
            field: "plugins",
            index: 3
        })
    );
    assert_eq!(
        version.map(|_| ()),
        Err(InitReadError::Name {
            field: "claude_code_version",
            index: 0
        })
    );
    assert_eq!(
        tool.map(|_| ()),
        Err(InitReadError::Name {
            field: "tools",
            index: 1
        })
    );
}
