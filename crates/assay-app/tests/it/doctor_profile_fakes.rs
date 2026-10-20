//! Fakes and builders for `verify_profile`'s tests (#84): a CLI probe with a fixed status, a
//! profile probe that answers with a run built from the words it is given, and a clean run.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;

use assay_app::{
    ArtifactNonce, BuiltinAllowlist, CliProbe, CliStatus, EgressToken, ExactVersion, ModelOutcome,
    NonceSource, ParseNonceError, PermissionDenial, PluginsConfig, ProfileContext, ProfileFailure,
    ProfilePorts, ProfileProbe, ProfileReport, ProfileRun, ProfileWords, SessionInit, StatusParts,
    Structured, verify_profile,
};
use serde_json::json;

use crate::session_init::{observed, pinned};

pub(crate) const NONCE: &str = "0123456789abcdef0123456789abcdef";
pub(crate) const PROMPT_WORD: &str = "ASSAYPROMPT0123456789abcdef0123456789abcdef";
pub(crate) const SYSTEM_WORD: &str = "ASSAYSYSTEM0123456789abcdef0123456789abcdef";
pub(crate) const TREE_WORD: &str = "ASSAYTREE0123456789abcdef0123456789abcdef";
pub(crate) const OUTSIDE_WORD: &str = "ASSAYOUTSIDE0123456789abcdef0123456789abcdef";
pub(crate) const TREE_FILE: &str = "/tmp/assay-canary-x/pr/TREE.md";
pub(crate) const OUTSIDE_FILE: &str = "/tmp/assay-canary-x/outside/OUTSIDE.md";
pub(crate) const TOKEN: &str = "sk-ant-oat01-Q2FuYXJ5VG9rZW4tZm9yLXRlc3Rz";
pub(crate) const HOOKS: [&str; 3] = ["user", "project", "tree"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Failed(pub(crate) &'static str);

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for Failed {}

/// A nonce source that gives the same nonce every time and counts the draws.
#[derive(Default)]
pub(crate) struct FixedNonce {
    pub(crate) drawn: usize,
}

impl NonceSource for FixedNonce {
    type Error = ParseNonceError;

    fn nonce(&mut self) -> Result<ArtifactNonce, ParseNonceError> {
        self.drawn = self.drawn.saturating_add(1);
        NONCE.parse()
    }
}

/// A CLI probe answering with a fixed status, or failing.
pub(crate) struct FakeCli(pub(crate) Result<CliStatus, Failed>);

impl CliProbe for FakeCli {
    type Error = Failed;

    fn status(&self) -> Result<CliStatus, Failed> {
        self.0.clone()
    }
}

/// An OAuth first-party session on `version`.
pub(crate) fn cli(version: &str) -> Result<FakeCli, Box<dyn Error>> {
    Ok(FakeCli(Ok(CliStatus::new(StatusParts {
        version: version.to_owned(),
        logged_in: true,
        auth_method: "oauth_token".to_owned(),
        api_provider: "firstParty".to_owned(),
    })?)))
}

/// A profile probe that builds its run from the words with `build`, and counts its calls.
pub(crate) struct ScriptedProfile<F> {
    build: F,
    pub(crate) calls: Cell<usize>,
    // Interior mutability: `ProfileProbe::probe_profile` takes `&self`.
    pub(crate) words: RefCell<Vec<String>>,
}

impl<F: Fn(&ProfileWords) -> ProfileRun> ScriptedProfile<F> {
    pub(crate) fn new(build: F) -> Self {
        Self {
            build,
            calls: Cell::new(0),
            words: RefCell::default(),
        }
    }
}

impl<F: Fn(&ProfileWords) -> ProfileRun> ProfileProbe for ScriptedProfile<F> {
    type Error = Failed;

    fn probe_profile(&self, words: &ProfileWords) -> Result<ProfileRun, Failed> {
        self.calls.set(self.calls.get().saturating_add(1));
        self.words.borrow_mut().extend(
            [
                words.prompt(),
                words.system(),
                words.tree(),
                words.outside(),
            ]
            .map(str::to_owned),
        );
        Ok((self.build)(words))
    }
}

/// A profile probe that cannot run.
pub(crate) struct FailingProfile;

impl ProfileProbe for FailingProfile {
    type Error = Failed;

    fn probe_profile(&self, _: &ProfileWords) -> Result<ProfileRun, Failed> {
        Err(Failed("the CLI could not start"))
    }
}

pub(crate) fn read(file_path: &str) -> PermissionDenial {
    PermissionDenial {
        tool_name: "Read".to_owned(),
        file_path: Some(file_path.to_owned()),
    }
}

/// A reply holding `prompt`, `system` and `tree` as its three words.
pub(crate) fn reply(prompt: &str, system: &str, tree: &str) -> ModelOutcome {
    ModelOutcome::Ok(Structured::new(json!({
        "prompt_word": prompt,
        "system_word": system,
        "tree_word": tree,
    })))
}

/// A run that shows every flag honoured: the words answered, the environ and outside reads
/// denied, nothing leaked, no hook ran.
pub(crate) fn clean_run(words: &ProfileWords) -> ProfileRun {
    ProfileRun {
        init: SessionInit::new(observed()),
        outcome: reply(words.prompt(), words.system(), words.tree()),
        transcript: format!(
            "{{\"type\":\"result\",\"structured_output\":{{\"tree_word\":\"{}\"}}}}\n",
            words.tree()
        )
        .into_bytes(),
        permission_mode: Some("dontAsk".to_owned()),
        denials: Some(vec![read("/proc/self/environ"), read(OUTSIDE_FILE)]),
        hook_events: Vec::new(),
        planted_hooks: HOOKS.map(str::to_owned).to_vec(),
        fired_hooks: Vec::new(),
        tree_file: TREE_FILE.to_owned(),
        outside_file: OUTSIDE_FILE.to_owned(),
    }
}

/// `version` as a pin.
pub(crate) fn version(version: &str) -> Result<ExactVersion, Box<dyn Error>> {
    let text = format!(
        "[assay]\ndigest = \"sha256:{}\"\nclaude_cli = \"{version}\"\n",
        "0".repeat(64)
    );
    let config: PluginsConfig = text.parse()?;
    Ok(config.assay().claude_cli.clone().ok_or("no CLI pin")?)
}

/// The variables of an ordinary canary job.
pub(crate) fn clean_env() -> Vec<OsString> {
    ["HOME", "PATH", "CLAUDE_CODE_OAUTH_TOKEN"]
        .map(OsString::from)
        .to_vec()
}

/// Everything `verify_profile` is judged against: the 2.1.296 pin and allowlist, and the token.
pub(crate) struct Pins {
    pub(crate) version: ExactVersion,
    pub(crate) allowlist: BuiltinAllowlist,
    pub(crate) token: EgressToken,
}

impl Pins {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        let (version, allowlist) = pinned()?;
        Ok(Self {
            version,
            allowlist,
            token: EgressToken::new(TOKEN)?,
        })
    }

    pub(crate) fn context(&self) -> ProfileContext<'_> {
        ProfileContext {
            pinned: &self.version,
            allowlist: &self.allowlist,
            token: &self.token,
        }
    }
}

/// Each check's failure, in report order.
pub(crate) type Failures = Vec<(assay_app::ProfileCheck, Option<ProfileFailure<Failed>>)>;

pub(crate) fn failures_of(report: &ProfileReport<Failed>) -> Failures {
    report
        .results()
        .iter()
        .map(|result| (result.check, result.failure.clone()))
        .collect()
}

/// The report for a run built from the words by `build`, on 2.1.296 in a clean environment.
pub(crate) fn report_for(
    build: impl Fn(&ProfileWords) -> ProfileRun,
) -> Result<ProfileReport<Failed>, Box<dyn Error>> {
    let pins = Pins::new()?;
    let probe = ScriptedProfile::new(build);
    let ports = ProfilePorts {
        cli: &cli("2.1.296")?,
        probe: &probe,
        nonces: &mut FixedNonce::default(),
    };
    Ok(verify_profile(clean_env(), ports, &pins.context())?)
}
