//! What the profile canary reads from its transcript beyond the outcome (03 §8, #84): the init's
//! `permissionMode`, the last `result`'s `permission_denials` and every hook message.
//!
//! Shapes are those of the CLI bundle (2.1.42 read; 2.1.296 is the pin): hook messages are
//! `{"type":"system","subtype":"hook_started"|"hook_progress"|"hook_response","hook_event":..}`
//! under `--include-hook-events`, and a denial is `{tool_name, tool_use_id, tool_input}`. The
//! readers fail closed: any message that looks like a hook's counts, and denials that are not
//! all readable are none.

use assay_app::PermissionDenial;
use serde_json::{Map, Value};

/// What the transcript showed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    /// The opening init's `permissionMode`, when it is a string.
    pub(crate) permission_mode: Option<String>,
    /// The last `result`'s `permission_denials`; `None` with no result or an unreadable list.
    pub(crate) denials: Option<Vec<PermissionDenial>>,
    /// Each hook message, named by its `hook_event`, else its `subtype`, else its `type`.
    pub(crate) hook_events: Vec<String>,
}

/// Reads `transcript` line by line; a line that is not a JSON object is skipped (the outcome
/// already counts it).
pub(crate) fn observe(transcript: &[u8]) -> Observed {
    let mut observed = Observed::default();
    let messages = transcript
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .map(|line| match serde_json::from_slice::<Value>(line) {
            Ok(Value::Object(message)) => Some(message),
            Ok(_) | Err(_) => None,
        });
    for (index, message) in messages.enumerate() {
        let Some(message) = message else { continue };
        if index == 0 && is_init(&message) {
            observed.permission_mode = text(&message, "permissionMode").map(str::to_owned);
        }
        if let Some(name) = hook_name(&message) {
            observed.hook_events.push(name.to_owned());
        }
        if text(&message, "type") == Some("result") {
            observed.denials = denials(message.get("permission_denials"));
        }
    }
    observed
}

fn text<'m>(message: &'m Map<String, Value>, key: &str) -> Option<&'m str> {
    message.get(key).and_then(Value::as_str)
}

fn is_init(message: &Map<String, Value>) -> bool {
    text(message, "type") == Some("system") && text(message, "subtype") == Some("init")
}

/// The name of a message that looks like a hook's: one with a `hook_event`, a `hook_*`
/// subtype or a `hook*` type.
fn hook_name(message: &Map<String, Value>) -> Option<&str> {
    let subtype = text(message, "subtype").filter(|subtype| subtype.starts_with("hook_"));
    let kind = text(message, "type").filter(|kind| kind.starts_with("hook"));
    match (message.get("hook_event"), subtype, kind) {
        (Some(event), _, _) => Some(event.as_str().unwrap_or("hook_event")),
        (None, Some(name), _) | (None, None, Some(name)) => Some(name),
        (None, None, None) => None,
    }
}

/// `permission_denials` when it is a list of objects each with a string `tool_name`.
fn denials(list: Option<&Value>) -> Option<Vec<PermissionDenial>> {
    list?
        .as_array()?
        .iter()
        .map(|entry| {
            let entry = entry.as_object()?;
            Some(PermissionDenial {
                tool_name: text(entry, "tool_name")?.to_owned(),
                file_path: entry
                    .get("tool_input")
                    .and_then(|input| input.get("file_path"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use assay_app::PermissionDenial;

    use super::observe;

    const INIT: &str =
        r#"{"type":"system","subtype":"init","permissionMode":"dontAsk","tools":["Read"]}"#;

    fn lines(lines: &[&str]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    fn read(path: &str) -> PermissionDenial {
        PermissionDenial {
            tool_name: "Read".to_owned(),
            file_path: Some(path.to_owned()),
        }
    }

    #[test]
    fn the_permission_mode_is_read_from_the_opening_init() {
        assert_eq!(
            observe(&lines(&[INIT])).permission_mode.as_deref(),
            Some("dontAsk")
        );
    }

    #[test]
    fn a_permission_mode_outside_the_opening_init_is_not_read() {
        let late = lines(&[r#"{"type":"assistant"}"#, INIT]);
        let mistyped = lines(&[r#"{"type":"system","subtype":"init","permissionMode":1}"#]);

        assert_eq!(observe(&late).permission_mode, None);
        assert_eq!(observe(&mistyped).permission_mode, None);
    }

    #[test]
    fn the_denials_are_the_last_results() {
        let transcript = lines(&[
            INIT,
            r#"{"type":"result","permission_denials":[]}"#,
            r#"{"type":"result","permission_denials":[{"tool_name":"Read","tool_use_id":"t1","tool_input":{"file_path":"/proc/self/environ"}},{"tool_name":"Grep","tool_use_id":"t2","tool_input":{"pattern":"x"}}]}"#,
        ]);

        assert_eq!(
            observe(&transcript).denials,
            Some(vec![
                read("/proc/self/environ"),
                PermissionDenial {
                    tool_name: "Grep".to_owned(),
                    file_path: None,
                },
            ])
        );
    }

    #[test]
    fn denials_that_cannot_be_read_are_none() {
        for result in [
            r#"{"type":"result"}"#,
            r#"{"type":"result","permission_denials":{}}"#,
            r#"{"type":"result","permission_denials":[{"tool_input":{"file_path":"/x"}}]}"#,
            r#"{"type":"result","permission_denials":[{"tool_name":7}]}"#,
            r#"{"type":"result","permission_denials":["Read"]}"#,
        ] {
            assert_eq!(observe(&lines(&[INIT, result])).denials, None, "{result}");
        }
        assert_eq!(observe(&lines(&[INIT])).denials, None, "no result");
    }

    #[test]
    fn a_later_result_without_denials_hides_an_earlier_one() {
        let transcript = lines(&[
            r#"{"type":"result","permission_denials":[{"tool_name":"Read","tool_input":{"file_path":"/x"}}]}"#,
            r#"{"type":"result"}"#,
        ]);

        assert_eq!(observe(&transcript).denials, None);
    }

    #[test]
    fn hook_messages_are_named_by_their_event() {
        let transcript = lines(&[
            INIT,
            r#"{"type":"system","subtype":"hook_started","hook_id":"h","hook_name":"SessionStart:startup","hook_event":"SessionStart"}"#,
            r#"{"type":"system","subtype":"hook_response","exit_code":0}"#,
            r#"{"type":"hook_event","name":"Stop"}"#,
            r#"{"type":"assistant","hook_event":"PreToolUse"}"#,
            r#"{"type":"assistant","message":{"content":"hook_started"}}"#,
        ]);

        assert_eq!(
            observe(&transcript).hook_events,
            ["SessionStart", "hook_response", "hook_event", "PreToolUse"]
        );
    }
}
