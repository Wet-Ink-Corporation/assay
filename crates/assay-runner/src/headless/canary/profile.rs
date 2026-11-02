//! The profile canary of `assay doctor --profile` (03 §8, A-10, #84): one call with the
//! production argv and `--include-hook-events` in a throwaway checkout planted with hooks and
//! word files, which shows each flag honoured or not by observation.
//!
//! The sandbox, made fresh for the call and removed after it, holds:
//! - `home/`, the call's `HOME`, `cwd/`, the working directory, and `pr/`, the `--add-dir` tree,
//!   each with a `.claude/settings.json` planting a canary hook (user, project and in-tree
//!   scope) on every session, prompt, tool and stop event, writing `markers/hook-<scope>`;
//! - `pr/TREE.md` with the tree word, and `outside/OUTSIDE.md`, outside both working
//!   directories, with the outside word;
//! - `prompt/system.md`, the appended system prompt, with the system word.
//!
//! The stdin prompt gives the prompt word and asks for three reads: [`FENCE_TARGET`], the tree
//! file and the outside file.

use std::fs;
use std::io;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{FENCE_TARGET, PROFILE_SCHEMA, ProfileProbe, ProfileRun, ProfileWords};
use assay_domain::Aeu;
use serde_json::{Value, json};

use super::super::supervise::{self, Bounds};
use super::super::{DEFAULT_MAX_OUTPUT_BYTES, GroupKill, HeadlessError, Invocation};
use super::{CanaryError, ClaudeCanary};
use crate::stream;

/// The scopes a canary hook is planted at, in report order.
const SCOPES: [&str; 3] = ["user", "project", "tree"];

/// The hook events planted, every one a session without hooks must not run.
const EVERY_EVENT: [&str; 4] = ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"];

/// The tool events planted, matching every tool.
const TOOL_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];

/// Turns the profile canary may take: its reads, then its answer, with room for one retry.
const TURNS: NonZeroU8 = NonZeroU8::MIN.saturating_add(3);

/// The profile canary's cap: 0.25 AEU, passed as `--max-budget-usd`.
const BUDGET: Aeu = Aeu::from_micros(250_000);

/// The profile canary's wall-clock limit.
const TIMEOUT: Duration = Duration::from_secs(300);

impl<K: GroupKill> ProfileProbe for ClaudeCanary<K> {
    type Error = CanaryError;

    /// Plants a fresh sandbox, runs one profile call in it, reports what it showed and removes
    /// it.
    ///
    /// # Errors
    /// [`CanaryError`], as for the mod canary: the sandbox could not be made or planted, the
    /// CLI could not be started, supervised or killed, the markers could not be inspected, or
    /// the sandbox could not be removed ([`CanaryError::Abandoned`] when the canary failed too).
    fn probe_profile(&self, words: &ProfileWords) -> Result<ProfileRun, CanaryError> {
        self.in_sandbox(|root| self.profile_in(root, words))
    }
}

impl<K: GroupKill> ClaudeCanary<K> {
    /// Plants the sandbox at `root`, runs the profile call in it and reports what it showed.
    fn profile_in(&self, root: &Path, words: &ProfileWords) -> Result<ProfileRun, CanaryError> {
        let layout = Layout::of(root)?;
        layout.plant(words).map_err(CanaryError::Sandbox)?;
        let invocation = Invocation {
            json_schema: PROFILE_SCHEMA,
            system_prompt_file: &layout.system_prompt,
            add_dir: &layout.tree,
            max_turns: TURNS,
            budget: BUDGET,
        };
        let cli = self
            .parent_death
            .command(&self.program)
            .map_err(HeadlessError::Spawn)?;
        let mut command = invocation.command(cli, &layout.cwd, &self.env);
        command
            .arg("--include-hook-events")
            .env("HOME", &layout.home);
        let bounds = Bounds {
            timeout: TIMEOUT,
            cap: DEFAULT_MAX_OUTPUT_BYTES,
        };
        let prompt = layout.prompt(words);
        let captured = supervise::run(command, prompt.as_bytes(), bounds, &self.killer)?;
        let fired_hooks = layout.fired().map_err(CanaryError::Inspect)?;
        let result = stream::classify(&captured.transcript, captured.ending, BUDGET);
        let observed = stream::observe(&captured.transcript);
        Ok(ProfileRun {
            init: result.init,
            outcome: result.outcome,
            transcript: captured.transcript,
            permission_mode: observed.permission_mode,
            denials: observed.denials,
            hook_events: observed.hook_events,
            planted_hooks: SCOPES.map(str::to_owned).to_vec(),
            fired_hooks,
            tree_file: layout.tree_file,
            outside_file: layout.outside_file,
        })
    }
}

/// The paths of one sandbox, all under a UTF-8 root.
struct Layout {
    home: PathBuf,
    cwd: PathBuf,
    tree: PathBuf,
    markers: PathBuf,
    system_prompt: PathBuf,
    tree_file: String,
    outside_file: String,
}

impl Layout {
    fn of(root: &Path) -> Result<Self, CanaryError> {
        let Some(text) = root.to_str() else {
            return Err(CanaryError::NotUtf8 {
                path: root.to_path_buf(),
            });
        };
        Ok(Self {
            home: root.join("home"),
            cwd: root.join("cwd"),
            tree: root.join("pr"),
            markers: root.join("markers"),
            system_prompt: root.join("prompt").join("system.md"),
            tree_file: format!("{text}/pr/TREE.md"),
            outside_file: format!("{text}/outside/OUTSIDE.md"),
        })
    }

    /// The scope directories, in [`SCOPES`] order.
    fn scopes(&self) -> [(&str, &Path); 3] {
        let [user, project, tree] = SCOPES;
        [(user, &self.home), (project, &self.cwd), (tree, &self.tree)]
    }

    /// Writes every planted file.
    fn plant(&self, words: &ProfileWords) -> io::Result<()> {
        fs::create_dir_all(&self.markers)?;
        for (scope, dir) in self.scopes() {
            let claude = dir.join(".claude");
            fs::create_dir_all(&claude)?;
            let hooks = hooks(&self.markers.join(format!("hook-{scope}")))?;
            fs::write(claude.join("settings.json"), hooks.to_string())?;
        }
        write_new(
            &self.system_prompt,
            &format!(
                "You are checking that a session starts and reads files as asked. \
                 The system word is `{}`.\n",
                words.system()
            ),
        )?;
        write_new(
            Path::new(&self.tree_file),
            &format!("The tree word is `{}`.\n", words.tree()),
        )?;
        write_new(
            Path::new(&self.outside_file),
            &format!("The outside word is `{}`.\n", words.outside()),
        )
    }

    /// The stdin prompt: the prompt word and the three reads.
    fn prompt(&self, words: &ProfileWords) -> String {
        format!(
            "This session checks a sandbox. Use the Read tool three times, making each call even \
             if you expect it to be refused:\n\
             1. Read {FENCE_TARGET}\n\
             2. Read {}\n\
             3. Read {}\n\
             Then answer: set prompt_word to `{}`, system_word to the system word your system \
             prompt gives, and tree_word to the word in the second file, or an empty string if \
             it could not be read. Never repeat what the first or third file holds.\n",
            self.tree_file,
            self.outside_file,
            words.prompt()
        )
    }

    /// The scopes whose marker exists.
    fn fired(&self) -> io::Result<Vec<String>> {
        let mut fired = Vec::new();
        for scope in SCOPES {
            if self.markers.join(format!("hook-{scope}")).try_exists()? {
                fired.push(scope.to_owned());
            }
        }
        Ok(fired)
    }
}

/// Creates `path`'s parent and writes `text` to it.
fn write_new(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

/// The settings planting one command hook on every event, writing `marker`.
fn hooks(marker: &Path) -> io::Result<Value> {
    let marker = marker
        .to_str()
        .ok_or_else(|| io::Error::other("the marker path is not UTF-8"))?;
    let command = json!({ "type": "command", "command": format!(": > {}", sh_quote(marker)) });
    let mut events = serde_json::Map::new();
    for event in EVERY_EVENT {
        events.insert(event.to_owned(), json!([{ "hooks": [&command] }]));
    }
    for event in TOOL_EVENTS {
        events.insert(
            event.to_owned(),
            json!([{ "matcher": "*", "hooks": [&command] }]),
        );
    }
    Ok(json!({ "hooks": events }))
}

/// `text` as one single-quoted shell word.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn a_quote_in_a_path_stays_inside_one_shell_word() {
        assert_eq!(sh_quote("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(sh_quote("/tmp/plain"), "'/tmp/plain'");
    }
}
