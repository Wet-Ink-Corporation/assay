//! The preflight's view of the CLI (06 §1, #83): `read_cli_status` on recorded
//! `claude --version` and `claude auth status` outputs (`tests/fixtures/status/`), and
//! `ClaudeHeadless`'s `CliProbe` against the stand-in `claude`.

use std::assert_matches;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{CliProbe, CliStatus, StatusParts, StatusReadError};
use assay_runner::{
    ClaudeHeadless, HeadlessEnv, HeadlessError, StatusCommand, StatusError, Workdir, WorkdirError,
    read_cli_status,
};
use tempfile::TempDir;

use crate::support::{Asked, RecordingKill, fixtures_dir};

const TOKEN: &str = "sk-ant-oat01-test-token";

/// The most bytes of either status output that are read: 64 KiB.
const STATUS_CAP: usize = 64 * 1024;

fn fixture(name: &str) -> PathBuf {
    fixtures_dir().join("status").join(name)
}

/// `read_cli_status` on the 2.1.296 version line and the auth status fixture `auth`.
fn read_with_auth(auth: &str) -> Result<Result<CliStatus, StatusReadError>, Box<dyn Error>> {
    let version = fs::read(fixture("version_2_1_296.txt"))?;
    Ok(read_cli_status(&version, &fs::read(fixture(auth))?))
}

fn status(
    version: &str,
    logged_in: bool,
    method: &str,
    provider: &str,
) -> Result<CliStatus, StatusReadError> {
    CliStatus::new(StatusParts {
        version: version.to_owned(),
        logged_in,
        auth_method: method.to_owned(),
        api_provider: provider.to_owned(),
    })
}

#[test]
fn the_2_1_296_oauth_status_is_read() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        read_with_auth("auth_oauth_2_1_296.json")?,
        status("2.1.296", true, "oauth_token", "firstParty")
    );
    Ok(())
}

#[test]
fn an_api_key_status_is_read_as_it_is_for_the_preflight_to_refuse() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        read_with_auth("auth_api_key_2_1_296.json")?,
        status("2.1.296", true, "api_key", "firstParty")
    );
    Ok(())
}

#[test]
fn a_bedrock_status_is_read_as_it_is_for_the_preflight_to_refuse() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        read_with_auth("auth_bedrock_2_1_296.json")?,
        status("2.1.296", true, "third_party", "bedrock")
    );
    Ok(())
}

#[test]
fn a_logged_out_status_is_read_as_it_is_for_the_preflight_to_refuse() -> Result<(), Box<dyn Error>>
{
    assert_eq!(
        read_with_auth("auth_logged_out.json")?,
        status("2.1.296", false, "none", "firstParty")
    );
    Ok(())
}

#[test]
fn each_unreadable_auth_status_is_refused() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("auth_not_json.txt", StatusReadError::NotJson),
        ("auth_trailing_text.json", StatusReadError::NotJson),
        ("auth_not_object.json", StatusReadError::NotJson),
        ("auth_repeated_key.json", StatusReadError::RepeatedKey),
        (
            "auth_missing_method.json",
            StatusReadError::Field {
                field: "authMethod",
            },
        ),
        (
            "auth_mistyped_logged_in.json",
            StatusReadError::Field { field: "loggedIn" },
        ),
        (
            "auth_control_character.json",
            StatusReadError::Name {
                field: "authMethod",
            },
        ),
    ];
    for (name, expected) in cases {
        assert_eq!(read_with_auth(name)?, Err(expected), "{name}");
    }
    Ok(())
}

#[test]
fn each_unreadable_version_line_is_refused() -> Result<(), Box<dyn Error>> {
    let auth = fs::read(fixture("auth_oauth_2_1_296.json"))?;
    let lines: [&[u8]; 6] = [
        &fs::read(fixture("version_no_suffix.txt"))?,
        &fs::read(fixture("version_extra_line.txt"))?,
        b"",
        b" (Claude Code)\n",
        b"2.1.296 beta (Claude Code)\n",
        b"2.1.296 (Claude Code)\r\n",
    ];
    for line in lines {
        assert_eq!(
            read_cli_status(line, &auth),
            Err(StatusReadError::Version),
            "{}",
            String::from_utf8_lossy(line)
        );
    }
    Ok(())
}

#[test]
fn a_version_line_without_a_newline_is_read() -> Result<(), Box<dyn Error>> {
    let auth = fs::read(fixture("auth_oauth_2_1_296.json"))?;

    let read = read_cli_status(b"2.1.296 (Claude Code)", &auth);

    assert_eq!(read, status("2.1.296", true, "oauth_token", "firstParty"));
    Ok(())
}

/// A scratch directory holding an empty working directory, the fake's records and generated
/// outputs.
struct Scene {
    root: TempDir,
    killer: RecordingKill,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("empty"))?;
        fs::create_dir(root.path().join("record"))?;
        Ok(Self {
            root,
            killer: RecordingKill::default(),
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn record(&self, name: &str) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(self.path("record").join(name))?)
    }

    /// A runner whose fake prints `version` and `auth` for the two status commands.
    fn runner(
        &self,
        version: PathBuf,
        auth: PathBuf,
        hang: bool,
    ) -> Result<ClaudeHeadless<&RecordingKill>, Box<dyn Error>> {
        let mut env = HeadlessEnv::new(TOKEN);
        env.pass("FAKE_RECORD", self.path("record"))?;
        env.pass("FAKE_VERSION", version)?;
        env.pass("FAKE_AUTH", auth)?;
        if hang {
            env.pass("FAKE_HANG", "1")?;
        }
        Ok(ClaudeHeadless::new(
            fixtures_dir().join("fake-claude"),
            Workdir::new(self.path("empty"))?,
            env,
            &self.killer,
        ))
    }

    /// A runner over the 2.1.296 version line and the oauth status.
    fn oauth_runner(&self) -> Result<ClaudeHeadless<&RecordingKill>, Box<dyn Error>> {
        self.runner(
            fixture("version_2_1_296.txt"),
            fixture("auth_oauth_2_1_296.json"),
            false,
        )
    }

    /// The oauth status padded with spaces to `len` bytes, written to a file.
    fn padded_auth(&self, len: usize) -> Result<PathBuf, Box<dyn Error>> {
        let mut bytes = fs::read(fixture("auth_oauth_2_1_296.json"))?;
        bytes.resize(len, b' ');
        let path = self.path(&format!("auth-{len}.json"));
        fs::write(&path, bytes)?;
        Ok(path)
    }
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_probe_reads_the_status_of_the_cli_it_runs() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let read = scene.oauth_runner()?.status();

    assert_eq!(
        read.map_err(|error| error.to_string()),
        Ok(status("2.1.296", true, "oauth_token", "firstParty")?)
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_status_commands_run_in_the_workdir_with_the_headless_environment_only()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.oauth_runner()?.status()?;

    let argv = |mode| -> Result<Vec<String>, Box<dyn Error>> {
        Ok(scene
            .record(&format!("argv.{mode}"))?
            .split_terminator('\0')
            .map(str::to_owned)
            .collect())
    };
    let env: BTreeMap<String, String> = scene
        .record("env.auth")?
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .filter(|(name, _)| !matches!(name.as_str(), "PWD" | "SHLVL" | "_"))
        .collect();
    let workdir = fs::canonicalize(scene.path("empty"))?;
    assert_eq!(
        (argv("version")?, argv("auth")?),
        (
            vec!["--version".to_owned()],
            vec!["auth".to_owned(), "status".to_owned()]
        )
    );
    assert_eq!(
        scene.record("cwd.auth")?.trim_end(),
        workdir.to_string_lossy()
    );
    assert_eq!(
        env.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
            "FAKE_AUTH",
            "FAKE_RECORD",
            "FAKE_VERSION",
        ]
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn a_status_output_of_exactly_the_cap_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let auth = scene.padded_auth(STATUS_CAP)?;

    let read = scene
        .runner(fixture("version_2_1_296.txt"), auth, false)?
        .status();

    assert_eq!(
        read.map_err(|error| error.to_string()),
        Ok(status("2.1.296", true, "oauth_token", "firstParty")?)
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn a_status_output_one_byte_over_the_cap_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let auth = scene.padded_auth(STATUS_CAP + 1)?;

    let read = scene
        .runner(fixture("version_2_1_296.txt"), auth, false)?
        .status();

    assert_matches!(
        read,
        Err(StatusError::TooLarge {
            command: StatusCommand::AuthStatus
        })
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn a_status_command_past_its_timeout_is_killed_and_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = scene
        .runner(
            fixture("version_2_1_296.txt"),
            fixture("auth_oauth_2_1_296.json"),
            true,
        )?
        .with_status_timeout(Duration::from_millis(300));

    let read = runner.status();

    assert_matches!(
        read,
        Err(StatusError::TimedOut {
            command: StatusCommand::Version
        })
    );
    let pid: u32 = scene.record("pid.version")?.trim().parse()?;
    assert_eq!(
        scene.killer.take_asked(),
        [Asked::Led {
            group: pid,
            leader_unreaped: true
        }]
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn an_unreadable_status_from_the_cli_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let read = scene
        .runner(
            fixture("version_2_1_296.txt"),
            fixture("auth_repeated_key.json"),
            false,
        )?
        .status();

    assert_matches!(read, Err(StatusError::Read(StatusReadError::RepeatedKey)));
    Ok(())
}

#[test]
fn the_probe_refuses_a_workdir_that_is_no_longer_empty() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = scene.oauth_runner()?;
    fs::write(scene.path("empty").join("planted"), "x")?;

    let read = runner.status();

    assert_matches!(
        read,
        Err(StatusError::Run(HeadlessError::Workdir(
            WorkdirError::NotEmpty { .. }
        )))
    );
    Ok(())
}
