//! The consumer side of ADR-14 and `05` T14: `.github/actions/fetch-by-digest/fetch-by-digest.sh`
//! installs an OCI artifact only when its manifest and every layer match the pinned digest
//! (`02 §4`). A fake `oras` on `PATH` serves blobs from a directory, so no test touches the
//! network (TST-10).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REPOSITORY: &str = "ghcr.io/wet-ink-corporation/assay";
/// The single layer of the fixture artifact, and its sha256.
const LAYER: &[u8] = b"#!/bin/sh\necho assay-fixture\n";
const LAYER_HEX: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
/// sha256 of `manifest("assay")`.
const MANIFEST_HEX: &str = "75cb4a025c9733e918a795d1b498ac44974c23f63c5c0127a92f05540fae8b08";
/// sha256 of `manifest("../assay")`: a well-formed manifest whose layer title escapes `out/`.
const ESCAPING_MANIFEST_HEX: &str =
    "96a69ccf159c4f93ffe1b780274e357ccd738c52845827f524f35371f990d934";
const GIT_PIN: &str = "0123456789abcdef0123456789abcdef01234567";

/// The fake `oras`: `oras {manifest|blob} fetch [-u U --password-stdin] --output F REF@sha256:H`
/// copies `$FAKE_ORAS_STORE/H` to `F` and appends the command to `$FAKE_ORAS_LOG`.
const FAKE_ORAS: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_ORAS_LOG"
shift 2
out= ref=
while [ $# -gt 0 ]; do
  case "$1" in
    --output|-o) out=$2; shift 2 ;;
    -u|--username) shift 2 ;;
    --password-stdin) cat > /dev/null; shift ;;
    *) ref=$1; shift ;;
  esac
done
cp "$FAKE_ORAS_STORE/${ref##*@sha256:}" "$out"
"#;

/// An OCI image manifest as `oras push` writes it for the assay binary, with one titled layer.
fn manifest(title: &str) -> String {
    format!(
        concat!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","#,
            r#""artifactType":"application/vnd.wet-ink.assay.binary.v1","#,
            r#""config":{{"mediaType":"application/vnd.oci.empty.v1+json","#,
            r#""digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","#,
            r#""size":2,"data":"e30="}},"#,
            r#""layers":[{{"mediaType":"application/vnd.wet-ink.assay.binary.layer.v1","#,
            r#""digest":"sha256:{layer}","size":29,"#,
            r#""annotations":{{"org.opencontainers.image.title":"{title}"}}}}]}}"#,
        ),
        layer = LAYER_HEX,
        title = title,
    )
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/actions/fetch-by-digest/fetch-by-digest.sh")
}

/// A per-test directory: `store/` (the fake registry), `bin/oras`, `out/` and `oras.log`.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(test: &str) -> io::Result<Self> {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("fetch-by-digest")
            .join(test);
        match fs::remove_dir_all(&root) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        fs::create_dir_all(root.join("store"))?;
        fs::create_dir_all(root.join("bin"))?;
        let oras = root.join("bin/oras");
        fs::write(&oras, FAKE_ORAS)?;
        fs::set_permissions(&oras, fs::Permissions::from_mode(0o755))?;
        Ok(Self { root })
    }

    /// Makes the fake registry answer a request for `sha256:<hex>` with `bytes`.
    fn serve(&self, hex: &str, bytes: &[u8]) -> io::Result<()> {
        fs::write(self.root.join("store").join(hex), bytes)
    }

    /// Serves the fixture artifact honestly: the manifest and its layer under their own digests.
    fn serve_fixture(&self) -> io::Result<()> {
        self.serve(MANIFEST_HEX, manifest("assay").as_bytes())?;
        self.serve(LAYER_HEX, LAYER)
    }

    fn out(&self) -> PathBuf {
        self.root.join("out")
    }

    fn run(&self, args: &[&str]) -> io::Result<Output> {
        let mut path = vec![self.root.join("bin")];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path: OsString =
            std::env::join_paths(path).map_err(|error| io::Error::other(error.to_string()))?;
        Command::new("bash")
            .arg(script())
            .args(args)
            .env("PATH", path)
            .env("FAKE_ORAS_STORE", self.root.join("store"))
            .env("FAKE_ORAS_LOG", self.root.join("oras.log"))
            .env_remove("GITHUB_OUTPUT")
            .output()
    }

    fn fetch(&self, digest: &str) -> io::Result<Output> {
        let out = self.out();
        let out = out
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 path"))?;
        self.run(&["fetch", REPOSITORY, digest, out])
    }

    /// Whether `out/` holds anything; a missing directory holds nothing.
    fn out_is_empty(&self) -> io::Result<bool> {
        match fs::read_dir(self.out()) {
            Ok(mut entries) => Ok(entries.next().is_none()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn oras_was_called(&self) -> bool {
        self.root.join("oras.log").exists()
    }

    fn write_config(&self, text: &str) -> io::Result<String> {
        let config = self.root.join("plugins.toml");
        fs::write(&config, text)?;
        config
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("non-UTF-8 path"))
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn installs_the_artifact_executable_when_every_digest_matches()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("installs")?;
    sandbox.serve_fixture()?;

    let output = sandbox.fetch(&format!("sha256:{MANIFEST_HEX}"))?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let installed = sandbox.out().join("assay");
    assert_eq!(fs::read(&installed)?, LAYER, "installed bytes differ");
    assert_eq!(
        fs::metadata(&installed)?.permissions().mode() & 0o777,
        0o755,
        "the installed binary must be executable"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_the_manifest_does_not_match_the_pinned_digest()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("manifest-mismatch")?;
    // The registry answers the pinned digest with a different (well-formed) manifest.
    sandbox.serve(MANIFEST_HEX, manifest("assay-other").as_bytes())?;
    sandbox.serve(LAYER_HEX, LAYER)?;

    let output = sandbox.fetch(&format!("sha256:{MANIFEST_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("digest mismatch"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_a_layer_does_not_match_its_descriptor()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("layer-mismatch")?;
    sandbox.serve(MANIFEST_HEX, manifest("assay").as_bytes())?;
    sandbox.serve(LAYER_HEX, b"#!/bin/sh\necho tampered-fixt\n")?;

    let output = sandbox.fetch(&format!("sha256:{MANIFEST_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("digest mismatch"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn rejects_a_pin_that_is_not_a_sha256_digest_before_contacting_the_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("not-a-digest")?;
    sandbox.serve_fixture()?;
    let uppercase = format!("sha256:{}", MANIFEST_HEX.to_uppercase());
    let short = format!("sha256:{}", MANIFEST_HEX.get(..63).unwrap_or_default());
    let pins = ["v0.1.0", "latest", MANIFEST_HEX, &uppercase, &short];

    let accepted = pins
        .into_iter()
        .map(|pin| Ok((pin, sandbox.fetch(pin)?.status.code())))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|&(_, code)| code != Some(2))
        .collect::<Vec<_>>();

    assert_eq!(accepted, Vec::new(), "pins not rejected as usage errors");
    assert!(!sandbox.oras_was_called(), "oras ran for an invalid pin");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn rejects_a_layer_title_that_escapes_the_output_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("escaping-title")?;
    sandbox.serve(ESCAPING_MANIFEST_HEX, manifest("../assay").as_bytes())?;
    sandbox.serve(LAYER_HEX, LAYER)?;

    let output = sandbox.fetch(&format!("sha256:{ESCAPING_MANIFEST_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    assert!(
        !sandbox.root.join("assay").exists(),
        "a layer was written outside the output directory"
    );
    Ok(())
}

fn plugins_toml() -> String {
    format!(
        r#"[assay]
digest = "sha256:{MANIFEST_HEX}"
warm_target = "sha256:{LAYER_HEX}"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "oci:sha256:{LAYER_HEX}"
required = true

[[plugin]]
name = "weigh-in"
adapter = "builtin:weigh-in"
pin = "git:{GIT_PIN}"
required = false
"#
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn reads_the_assay_digest_from_plugins_toml() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-assay")?;
    let config = sandbox.write_config(&plugins_toml())?;

    let output = sandbox.run(&["digest-from-config", &config, "assay.digest"])?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("sha256:{MANIFEST_HEX}\n")
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn reads_an_oci_plugin_pin_from_plugins_toml() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-plugin")?;
    let config = sandbox.write_config(&plugins_toml())?;

    let output = sandbox.run(&["digest-from-config", &config, "plugin.temper"])?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("sha256:{LAYER_HEX}\n")
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn refuses_a_git_plugin_pin_because_it_is_not_an_oci_digest()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-git-pin")?;
    let config = sandbox.write_config(&plugins_toml())?;

    let output = sandbox.run(&["digest-from-config", &config, "plugin.weigh-in"])?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "no digest may be printed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn refuses_an_assay_digest_that_is_a_tag() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-tag")?;
    let config = sandbox.write_config("[assay]\ndigest = \"v0.1.0\"\n")?;

    let output = sandbox.run(&["digest-from-config", &config, "assay.digest"])?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "no digest may be printed");
    Ok(())
}
