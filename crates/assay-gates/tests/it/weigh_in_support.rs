//! Fixtures for the Weigh-In adapter: a head tree of journals and atoms in memory, journal
//! events shaped like the ones in this repository's `.kb/_ledger/journal/`, and the adapter run
//! through the gate use case so its document is schema-checked.

use std::error::Error;

use assay_app::{
    ContractValidator, GateJob, GateJobOutcome, GateRun, GateRunner, Plugin, PluginsConfig,
    run_gates,
};
use assay_domain::{Finding, FindingsDocument, Severity};
use assay_gates::{EntryKind, Revision, WeighInAdapter, WeighInError};
use serde_json::{Value, json};

use crate::temper_support::{FakeError, FakeRevisions};

pub(crate) const JOURNALS: &str = ".kb/_ledger/journal";
pub(crate) const DECISIONS: &str = ".kb/_intake/decisions";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
const GIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Base config with a non-required weigh-in (`required` as given) and a temper.
pub(crate) fn config(required: bool) -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(format!(
        r#"
[assay]
digest = "sha256:{HEX64}"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{GIT}"
required = true

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "git:{GIT}"
required = {required}
"#
    )
    .parse()?)
}

/// The plugin named `name` in `config`.
pub(crate) fn plugin(config: &PluginsConfig, name: &str) -> Result<Plugin, Box<dyn Error>> {
    config
        .plugins()
        .iter()
        .find(|plugin| plugin.name().as_str() == name)
        .cloned()
        .ok_or_else(|| format!("no plugin {name}").into())
}

/// One journal event of `kind` for `item` at `ts`, with the fields every event carries.
pub(crate) fn event(kind: &str, item: &str, ts: &str) -> Value {
    json!({
        "type": kind,
        "item_id": item,
        "actor": "cli",
        "session_id": "0af9bc42-376c-52dd-8e5c-98e787854a2c",
        "worktree": "assay",
        "branch": "claude/assay-109-weigh-in-adapter",
        "head": GIT,
        "ts": ts,
    })
}

/// `item_raised` for `item` at `ts` with `blocks`.
pub(crate) fn raised(item: &str, ts: &str, blocks: Option<&str>) -> Value {
    let mut raised = event("item_raised", item, ts);
    if let Some(object) = raised.as_object_mut() {
        object.insert("kind".to_owned(), "question".into());
        object.insert("question".to_owned(), "Ship it?".into());
        object.insert("blocks".to_owned(), blocks.map_or(Value::Null, Value::from));
        object.insert("door_hint".to_owned(), "two-way".into());
        object.insert("authority_hint".to_owned(), "judgment".into());
    }
    raised
}

/// `item_triaged` for `item` at `ts` with `door` and `authority`.
pub(crate) fn triaged(item: &str, ts: &str, door: Option<&str>, authority: Option<&str>) -> Value {
    let mut triaged = event("item_triaged", item, ts);
    if let Some(object) = triaged.as_object_mut() {
        object.insert("door".to_owned(), door.map_or(Value::Null, Value::from));
        object.insert(
            "authority".to_owned(),
            authority.map_or(Value::Null, Value::from),
        );
        object.insert("route".to_owned(), "present".into());
    }
    triaged
}

/// `events` as journal text: one compact JSON object per line, each ending in `\n`.
pub(crate) fn lines(events: &[Value]) -> String {
    let mut text = String::new();
    for event in events {
        text.push_str(&event.to_string());
        text.push('\n');
    }
    text
}

/// A decision atom for `item` as Weigh-In writes it, front matter first.
pub(crate) fn atom(item: &str) -> String {
    format!(
        "---\nid: \"kb-decision-{item}\"\ntitle: \"Ship it\"\nkind: decision\nstatus: accepted\n\
         weighin_item: \"{item}\"\nquestion: \"Ship it?\"\ndoor: two-way\n---\n\n# Ship it\n"
    )
}

/// The adapter's result with the fake reader.
pub(crate) type Ran = Result<GateRun, WeighInError<FakeError>>;

/// What the tests compare of each finding: rule, file, line and severity.
pub(crate) type Seen = (String, String, u64, Severity);

/// One PR head for the Weigh-In adapter.
#[derive(Debug, Default)]
pub(crate) struct Head {
    pub(crate) revisions: FakeRevisions,
}

impl Head {
    /// Sets `path` to the regular file `text` at head.
    pub(crate) fn file(&mut self, path: &str, text: &str) {
        self.revisions
            .insert((Revision::Head, path), EntryKind::File, text.as_bytes());
    }

    /// Sets `path` at head to an entry of `kind`.
    pub(crate) fn entry(&mut self, path: &str, kind: EntryKind, text: &str) {
        self.revisions
            .insert((Revision::Head, path), kind, text.as_bytes());
    }

    /// Writes `events` as the journal `<name>.jsonl`.
    pub(crate) fn journal(&mut self, name: &str, events: &[Value]) {
        self.file(&format!("{JOURNALS}/{name}.jsonl"), &lines(events));
    }

    /// Runs `plugin` of `config(required)` in `job`, directly.
    pub(crate) fn run(
        &self,
        required: bool,
        name: &str,
        job: GateJob,
    ) -> Result<Ran, Box<dyn Error>> {
        let config = config(required)?;
        let plugin = plugin(&config, name)?;
        let mut adapter = WeighInAdapter::new(&self.revisions, "run-1")?;
        Ok(adapter.run(&plugin, job))
    }

    /// Runs `gates-static` through the gate use case (temper, which this adapter refuses, is
    /// dropped): the weigh-in outcome and the validated document.
    pub(crate) fn document(
        &self,
    ) -> Result<(GateJobOutcome, Option<FindingsDocument>), Box<dyn Error>> {
        let config = config(false)?;
        let mut adapter = WeighInAdapter::new(&self.revisions, "run-1")?;
        let validator = ContractValidator::new()?;
        let reports = run_gates(&config, GateJob::Static, &mut adapter, &validator);
        let report = reports
            .into_iter()
            .find(|report| report.plugin.as_str() == "weigh-in")
            .ok_or("no weigh-in report")?;
        let document = report.document.map(serde_json::from_value).transpose()?;
        Ok((report.outcome, document))
    }

    /// The outcome and the findings of a `gates-static` run.
    pub(crate) fn findings(&self) -> Result<(GateJobOutcome, Vec<Finding>), Box<dyn Error>> {
        let (outcome, document) = self.document()?;
        Ok((outcome, document.map(|d| d.findings).unwrap_or_default()))
    }

    /// The outcome and the rule, file, line and severity of each finding.
    pub(crate) fn seen(&self) -> Result<(GateJobOutcome, Vec<Seen>), Box<dyn Error>> {
        let (outcome, findings) = self.findings()?;
        let seen = findings
            .into_iter()
            .map(|finding| {
                let rule = finding.rule.unwrap_or_default();
                (rule, finding.file, finding.line.get(), finding.severity)
            })
            .collect();
        Ok((outcome, seen))
    }
}
