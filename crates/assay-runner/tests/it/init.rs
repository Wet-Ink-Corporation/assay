//! `read_session_init`: the `system/init` message read out of a stream-json transcript and
//! judged by the clean-init rule against the pinned allowlist (03 §8, #121).

use std::error::Error;
use std::fs;

use assay_app::{
    BuiltinKind, ExactVersion, InitPlugin, InitReadError, InitUnclean, PluginsConfig, check_init,
};
use assay_runner::read_session_init;
use serde_json::{Value, json};

use crate::support::{fixtures_dir, stream};

/// The base config pinning 2.1.296 and its observed built-ins.
fn pinned() -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(fs::read_to_string(fixtures_dir().join("plugins-2.1.296.toml"))?.parse()?)
}

/// The clean-init verdict on the recorded stream `name`.
fn verdict(name: &str) -> Result<Result<(), InitUnclean>, Box<dyn Error>> {
    let config = pinned()?;
    let assay = config.assay();
    let version: &ExactVersion = assay.claude_cli.as_ref().ok_or("no CLI pin")?;
    let init = read_session_init(&stream(name)?)?;
    Ok(check_init(&init, version, &assay.claude_cli_builtins))
}

/// The init message of the 2.1.296 recording, as JSON to edit.
fn init_message() -> Result<Value, Box<dyn Error>> {
    let transcript = stream("init_2_1_296")?;
    let first = transcript
        .split(|&byte| byte == b'\n')
        .next()
        .ok_or("empty recording")?;
    Ok(serde_json::from_slice(first)?)
}

/// A transcript of `lines`, one JSON message each.
fn transcript(lines: &[&Value]) -> Vec<u8> {
    lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes()
}

/// The init message with `field` replaced by `value`.
fn with_field(field: &str, value: Value) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut init = init_message()?;
    let object = init.as_object_mut().ok_or("init is not an object")?;
    object.insert(field.to_owned(), value);
    Ok(transcript(&[&init]))
}

/// The init message without `field`.
fn without_field(field: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut init = init_message()?;
    let object = init.as_object_mut().ok_or("init is not an object")?;
    object.remove(field);
    Ok(transcript(&[&init]))
}

#[test]
fn the_2_1_296_init_is_clean_with_the_pinned_allowlist() -> Result<(), Box<dyn Error>> {
    assert_eq!(verdict("init_2_1_296")?, Ok(()));
    Ok(())
}

#[test]
fn an_extra_built_in_is_init_unclean() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        verdict("init_extra_builtin")?,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Plugin,
            name: "cc-plugin-new".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_non_built_in_plugin_is_init_unclean() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        verdict("init_non_builtin_plugin")?,
        Err(InitUnclean::NotBuiltin {
            name: "planted".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_skill_not_on_the_list_is_init_unclean() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        verdict("init_unlisted_skill")?,
        Err(InitUnclean::Unlisted {
            kind: BuiltinKind::Skill,
            name: "planted-skill".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_extra_tool_is_init_unclean() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        verdict("init_extra_tool")?,
        Err(InitUnclean::Tools {
            found: ["Glob", "Grep", "Read", "Bash"].map(str::to_owned).to_vec()
        })
    );
    Ok(())
}

#[test]
fn reads_every_field_the_rule_needs() -> Result<(), Box<dyn Error>> {
    let mut init = init_message()?;
    let object = init.as_object_mut().ok_or("init is not an object")?;
    object.insert(
        "mcp_servers".to_owned(),
        json!([{ "name": "github", "status": "connected" }]),
    );

    let read = read_session_init(&transcript(&[&init]))?;

    assert_eq!(read.cli_version(), "2.1.296");
    assert_eq!(read.tools(), ["Glob", "Grep", "Read"]);
    assert_eq!(read.mcp_servers(), ["github"]);
    assert_eq!(
        read.plugins().first(),
        Some(&InitPlugin {
            name: "cc-plugin-agents-md".to_owned(),
            path: "builtin".to_owned(),
            source: "cc-plugin-agents-md@builtin".to_owned(),
        })
    );
    assert_eq!(read.plugins().len(), 3);
    assert_eq!(read.skills().len(), 19);
    assert_eq!(
        read.agents(),
        ["claude", "Explore", "general-purpose", "Plan"]
    );
    Ok(())
}

#[test]
fn an_empty_transcript_has_no_init() {
    assert_eq!(read_session_init(b"\n\n"), Err(InitReadError::Missing));
}

#[test]
fn an_init_after_another_message_is_refused() -> Result<(), Box<dyn Error>> {
    let rate_limit =
        json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "allowed" } });
    let init = init_message()?;

    let read = read_session_init(&transcript(&[&rate_limit, &init]));

    assert_eq!(read, Err(InitReadError::Missing));
    Ok(())
}

#[test]
fn an_init_after_a_stray_line_is_refused() -> Result<(), Box<dyn Error>> {
    let mut bytes = b"(node) Warning: printed to stdout\n".to_vec();
    bytes.extend(transcript(&[&init_message()?]));

    assert_eq!(read_session_init(&bytes), Err(InitReadError::Missing));
    Ok(())
}

#[test]
fn another_system_message_first_is_not_an_init() {
    let status = json!({ "type": "system", "subtype": "status" });

    assert_eq!(
        read_session_init(&transcript(&[&status])),
        Err(InitReadError::Missing)
    );
}

#[test]
fn a_second_init_is_refused() -> Result<(), Box<dyn Error>> {
    let init = init_message()?;
    let result = json!({ "type": "result", "subtype": "success" });

    let read = read_session_init(&transcript(&[&init, &result, &init]));

    assert_eq!(read, Err(InitReadError::Repeated));
    Ok(())
}

#[test]
fn an_unreadable_line_after_the_init_is_left_to_classification() -> Result<(), Box<dyn Error>> {
    let mut bytes = transcript(&[&init_message()?]);
    bytes.extend_from_slice(b"not json\n");

    let read = read_session_init(&bytes).map(|init| init.tools().len());

    assert_eq!(read, Ok(3));
    Ok(())
}

#[test]
fn each_missing_field_is_named() -> Result<(), Box<dyn Error>> {
    for field in [
        "claude_code_version",
        "tools",
        "mcp_servers",
        "plugins",
        "skills",
        "agents",
    ] {
        assert_eq!(
            read_session_init(&without_field(field)?).map(|_| ()),
            Err(InitReadError::Field { field }),
            "{field}"
        );
    }
    Ok(())
}

#[test]
fn each_field_of_the_wrong_type_is_named() -> Result<(), Box<dyn Error>> {
    for field in [
        "claude_code_version",
        "tools",
        "mcp_servers",
        "plugins",
        "skills",
        "agents",
    ] {
        let wrong = if field == "claude_code_version" {
            json!(2)
        } else {
            json!("Read")
        };
        assert_eq!(
            read_session_init(&with_field(field, wrong)?).map(|_| ()),
            Err(InitReadError::Field { field }),
            "{field}"
        );
    }
    Ok(())
}

#[test]
fn a_list_entry_that_is_not_a_string_is_refused() -> Result<(), Box<dyn Error>> {
    let read = read_session_init(&with_field("skills", json!(["debug", 7]))?);

    assert_eq!(
        read.map(|_| ()),
        Err(InitReadError::Entry {
            field: "skills",
            index: 1
        })
    );
    Ok(())
}

#[test]
fn a_plugin_entry_with_another_key_is_refused() -> Result<(), Box<dyn Error>> {
    let plugin = json!({ "name": "x", "path": "builtin", "source": "x@builtin", "hooks": "./h" });

    let read = read_session_init(&with_field("plugins", json!([plugin]))?);

    assert_eq!(
        read.map(|_| ()),
        Err(InitReadError::Entry {
            field: "plugins",
            index: 0
        })
    );
    Ok(())
}

#[test]
fn a_plugin_entry_missing_a_key_or_with_a_non_string_is_refused() -> Result<(), Box<dyn Error>> {
    for plugin in [
        json!({ "name": "x", "path": "builtin" }),
        json!({ "name": "x", "path": "builtin", "source": 1 }),
        json!("x@builtin"),
    ] {
        let read = read_session_init(&with_field("plugins", json!([plugin]))?);

        assert_eq!(
            read.map(|_| ()),
            Err(InitReadError::Entry {
                field: "plugins",
                index: 0
            }),
            "{plugin}"
        );
    }
    Ok(())
}

#[test]
fn an_mcp_server_entry_without_a_name_is_refused() -> Result<(), Box<dyn Error>> {
    for server in [json!({ "status": "connected" }), json!("github")] {
        let read = read_session_init(&with_field("mcp_servers", json!([server]))?);

        assert_eq!(
            read.map(|_| ()),
            Err(InitReadError::Entry {
                field: "mcp_servers",
                index: 0
            }),
            "{server}"
        );
    }
    Ok(())
}

#[test]
fn a_list_over_the_cap_is_refused() -> Result<(), Box<dyn Error>> {
    let agents: Vec<String> = (0..257).map(|n| format!("agent-{n}")).collect();

    let read = read_session_init(&with_field("agents", json!(agents))?);

    assert_eq!(
        read.map(|_| ()),
        Err(InitReadError::TooMany {
            field: "agents",
            count: 257
        })
    );
    Ok(())
}
