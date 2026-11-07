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

use tempfile::TempDir;

use crate::support::write_executable;

const REPOSITORY: &str = "ghcr.io/wet-ink-corporation/assay";
/// The single layer of the fixture artifact, and its sha256.
const LAYER: &[u8] = b"#!/bin/sh\necho assay-fixture\n";
const LAYER_HEX: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
/// sha256 of `manifest("assay")`.
const MANIFEST_HEX: &str = "75cb4a025c9733e918a795d1b498ac44974c23f63c5c0127a92f05540fae8b08";
/// sha256 of `manifest("../assay")`: a well-formed manifest whose layer title escapes `out/`.
const ESCAPING_MANIFEST_HEX: &str =
    "96a69ccf159c4f93ffe1b780274e357ccd738c52845827f524f35371f990d934";
/// sha256 of `manifest_with(INDEX_TYPE, "assay", "29")`: an image index, not a manifest.
const INDEX_HEX: &str = "9ff2f631e1285c344ee3d82b50d4b96eb2515ad2ee9720725589b2ac26a33ea5";
/// sha256 of `manifest_with(MANIFEST_TYPE, "assay", "\"29\"")`: the layer size is a string.
const STRING_SIZE_HEX: &str = "7fd7106ccc0e23b13f872ce75846e382388780c002a933f00410e3607f8c07bd";
/// A second layer, as a signature beside the binary would be, and its sha256.
const SIGNATURE: &[u8] = b"assay-fixture-signature\n";
const SIGNATURE_HEX: &str = "bb3a78a8c8fda7dc4b05cb24ed249a584c6c2aa216c4f024e61bb9c325cd401c";
/// sha256 of `manifest_of(MANIFEST_TYPE, &[layer "assay", signature "assay.sig"])`.
const TWO_LAYER_HEX: &str = "46e9ca214a29d3587e2575c1b26b81690097cb2f1c62f66976e508e20dc38bc5";
/// sha256 of `manifest_of(MANIFEST_TYPE, &[layer "assay", signature "assay"])`: two layers, one
/// title.
const DUPLICATE_TITLE_HEX: &str =
    "175371f55162dddca64c9a2d25a814a49884f65de464dc79a6bd13aac689bd66";
const GIT_PIN: &str = "0123456789abcdef0123456789abcdef01234567";
const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const PASSWORD: &str = "s3cret";

/// The fake `oras`: `oras {manifest|blob} fetch [-u U --password-stdin] --output F REF@sha256:H`
/// copies `$FAKE_ORAS_STORE/H` to `F`, appends its argv to `$FAKE_ORAS_LOG` and writes what it
/// read on stdin for `--password-stdin` to `$FAKE_ORAS_STDIN`. Without `--password-stdin` it
/// still drains stdin, as a real CLI may: a caller must not feed it anything it needs later.
const FAKE_ORAS: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_ORAS_LOG"
shift 2
out= ref= password=
while [ $# -gt 0 ]; do
  case "$1" in
    --output|-o) out=$2; shift 2 ;;
    -u|--username) shift 2 ;;
    --password-stdin) cat > "$FAKE_ORAS_STDIN"; password=1; shift ;;
    *) ref=$1; shift ;;
  esac
done
[ -n "$password" ] || [ -t 0 ] || cat > /dev/null
cp "$FAKE_ORAS_STORE/${ref##*@sha256:}" "$out"
"#;

/// An OCI manifest shaped like the one `oras push` writes for the assay binary. Each layer is
/// `(sha256 hex, size as JSON text, title)`.
fn manifest_of(media_type: &str, layers: &[(&str, &str, &str)]) -> String {
    let layers = layers
        .iter()
        .map(|(hex, size, title)| {
            format!(
                concat!(
                    r#"{{"mediaType":"application/vnd.wet-ink.assay.binary.layer.v1","#,
                    r#""digest":"sha256:{hex}","size":{size},"#,
                    r#""annotations":{{"org.opencontainers.image.title":"{title}"}}}}"#,
                ),
                hex = hex,
                size = size,
                title = title,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            r#"{{"schemaVersion":2,"mediaType":"{media_type}","#,
            r#""artifactType":"application/vnd.wet-ink.assay.binary.v1","#,
            r#""config":{{"mediaType":"application/vnd.oci.empty.v1+json","#,
            r#""digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","#,
            r#""size":2,"data":"e30="}},"#,
            r#""layers":[{layers}]}}"#,
        ),
        media_type = media_type,
        layers = layers,
    )
}

/// The fixture manifest with its one layer titled `title`, of type `media_type` and with the
/// layer `size` given as JSON text.
fn manifest_with(media_type: &str, title: &str, size: &str) -> String {
    manifest_of(media_type, &[(LAYER_HEX, size, title)])
}

/// The OCI image manifest `oras push` writes for the assay binary, with one titled layer.
fn manifest(title: &str) -> String {
    manifest_with(MANIFEST_TYPE, title, "29")
}

/// The fixture manifest with the binary titled `assay` and the signature titled `signature_title`.
fn two_layer_manifest(signature_title: &str) -> String {
    manifest_of(
        MANIFEST_TYPE,
        &[
            (LAYER_HEX, "29", "assay"),
            (SIGNATURE_HEX, "24", signature_title),
        ],
    )
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/actions/fetch-by-digest/fetch-by-digest.sh")
}

/// A per-test directory: `store/` (the fake registry), `bin/oras`, `out/` and `oras.log`.
struct Sandbox {
    /// The sandbox's root, unique to it, so concurrent runs never share it (#166); removed on
    /// drop.
    dir: TempDir,
}

impl Sandbox {
    fn new(test: &str) -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("fetch-by-digest-{test}-"))
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
        let root = dir.path();
        fs::create_dir_all(root.join("store"))?;
        fs::create_dir_all(root.join("bin"))?;
        write_executable(&root.join("bin/oras"), FAKE_ORAS)?;
        Ok(Self { dir })
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Makes the fake registry answer a request for `sha256:<hex>` with `bytes`.
    fn serve(&self, hex: &str, bytes: &[u8]) -> io::Result<()> {
        fs::write(self.root().join("store").join(hex), bytes)
    }

    /// Serves the fixture artifact honestly: the manifest and its layer under their own digests.
    fn serve_fixture(&self) -> io::Result<()> {
        self.serve(MANIFEST_HEX, manifest("assay").as_bytes())?;
        self.serve(LAYER_HEX, LAYER)
    }

    fn out(&self) -> PathBuf {
        self.root().join("out")
    }

    fn path(&self, name: &str) -> io::Result<String> {
        self.root()
            .join(name)
            .into_os_string()
            .into_string()
            .map_err(|path| io::Error::other(format!("non-UTF-8 path {}", path.display())))
    }

    /// Runs the script with `env` added to (or overriding) the sandbox environment, in which
    /// `GITHUB_OUTPUT` and every `FETCH_BY_DIGEST_*` variable are unset.
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> io::Result<Output> {
        let mut path = vec![self.root().join("bin")];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path: OsString =
            std::env::join_paths(path).map_err(|error| io::Error::other(error.to_string()))?;
        let mut command = Command::new("bash");
        command
            .arg(script())
            .args(args)
            .env("PATH", path)
            .env("FAKE_ORAS_STORE", self.root().join("store"))
            .env("FAKE_ORAS_LOG", self.root().join("oras.log"))
            .env("FAKE_ORAS_STDIN", self.root().join("oras.stdin"))
            .env_remove("GITHUB_OUTPUT");
        for variable in [
            "FETCH_BY_DIGEST_USERNAME",
            "FETCH_BY_DIGEST_PASSWORD",
            "FETCH_BY_DIGEST_ARTIFACT_TYPE",
            "FETCH_BY_DIGEST_MODE",
        ] {
            command.env_remove(variable);
        }
        command.envs(env.iter().copied()).output()
    }

    fn run(&self, args: &[&str]) -> io::Result<Output> {
        self.run_with(args, &[])
    }

    fn fetch(&self, digest: &str) -> io::Result<Output> {
        self.run(&["fetch", REPOSITORY, digest, &self.path("out")?])
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
        self.root().join("oras.log").exists()
    }

    fn write_config(&self, text: &str) -> io::Result<String> {
        let config = self.root().join("plugins.toml");
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

/// Asserts the script gave `reason` on stderr, so a test fails for the refusal it is about.
fn assert_reason(output: &Output, reason: &str) {
    assert!(
        stderr(output).contains(reason),
        "expected `{reason}` on stderr: {}",
        stderr(output)
    );
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
    assert_reason(&output, "is not a plain file name");
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    assert!(
        !sandbox.root().join("assay").exists(),
        "a layer was written outside the output directory"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_the_artifact_type_differs()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("artifact-type")?;
    sandbox.serve_fixture()?;

    let digest = format!("sha256:{MANIFEST_HEX}");

    let output = sandbox.run_with(
        &["fetch", REPOSITORY, &digest, &sandbox.path("out")?],
        &[(
            "FETCH_BY_DIGEST_ARTIFACT_TYPE",
            "application/vnd.wet-ink.temper.v1",
        )],
    )?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_reason(&output, "is not of artifact type");
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_the_pin_names_an_image_index()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("image-index")?;
    sandbox.serve(
        INDEX_HEX,
        manifest_with(INDEX_TYPE, "assay", "29").as_bytes(),
    )?;
    sandbox.serve(LAYER_HEX, LAYER)?;

    let output = sandbox.fetch(&format!("sha256:{INDEX_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_reason(&output, "is not an OCI image manifest");
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_a_layer_size_is_not_an_integer()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("string-size")?;
    sandbox.serve(
        STRING_SIZE_HEX,
        manifest_with(MANIFEST_TYPE, "assay", "\"29\"").as_bytes(),
    )?;
    sandbox.serve(LAYER_HEX, LAYER)?;

    let output = sandbox.fetch(&format!("sha256:{STRING_SIZE_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_reason(&output, "integer size");
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn rejects_a_repository_that_is_not_a_plain_registry_path_before_contacting_the_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("bad-repository")?;
    sandbox.serve_fixture()?;
    let digest = format!("sha256:{MANIFEST_HEX}");
    let pinned = format!("ghcr.io/x/assay@{digest}");
    let out = sandbox.path("out")?;
    let repositories = [
        "ghcr.io/x/assay:v1",
        pinned.as_str(),
        "--plain-http",
        "-ghcr.io/x/assay",
    ];

    let accepted = repositories
        .into_iter()
        .map(|repository| {
            let code = sandbox
                .run(&["fetch", repository, &digest, &out])?
                .status
                .code();
            Ok((repository, code))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|&(_, code)| code != Some(2))
        .collect::<Vec<_>>();

    assert_eq!(
        accepted,
        Vec::new(),
        "repositories not rejected as usage errors"
    );
    assert!(
        !sandbox.oras_was_called(),
        "oras ran for an invalid repository"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn rejects_an_empty_output_directory_or_one_containing_a_newline()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("bad-out")?;
    sandbox.serve_fixture()?;
    let newline = format!("{}\ninjected=1", sandbox.path("out")?);
    let digest = format!("sha256:{MANIFEST_HEX}");

    let accepted = ["", newline.as_str()]
        .into_iter()
        .map(|out| {
            let code = sandbox
                .run(&["fetch", REPOSITORY, &digest, out])?
                .status
                .code();
            Ok((out, code))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|&(_, code)| code != Some(2))
        .collect::<Vec<_>>();

    assert_eq!(
        accepted,
        Vec::new(),
        "out-dirs not rejected as usage errors"
    );
    assert!(
        !sandbox.oras_was_called(),
        "oras ran for an invalid out-dir"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn installs_every_layer_of_a_multi_layer_artifact_with_its_exact_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("two-layers")?;
    sandbox.serve(TWO_LAYER_HEX, two_layer_manifest("assay.sig").as_bytes())?;
    sandbox.serve(LAYER_HEX, LAYER)?;
    sandbox.serve(SIGNATURE_HEX, SIGNATURE)?;

    let output = sandbox.fetch(&format!("sha256:{TWO_LAYER_HEX}"))?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        (
            fs::read(sandbox.out().join("assay"))?,
            fs::read(sandbox.out().join("assay.sig"))?
        ),
        (LAYER.to_vec(), SIGNATURE.to_vec()),
        "installed layers differ"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn aborts_and_installs_nothing_when_two_layers_share_a_title()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("duplicate-title")?;
    sandbox.serve(DUPLICATE_TITLE_HEX, two_layer_manifest("assay").as_bytes())?;
    sandbox.serve(LAYER_HEX, LAYER)?;
    sandbox.serve(SIGNATURE_HEX, SIGNATURE)?;

    let output = sandbox.fetch(&format!("sha256:{DUPLICATE_TITLE_HEX}"))?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_reason(&output, "two layers are titled");
    assert!(sandbox.out_is_empty()?, "nothing may be installed");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn passes_the_registry_password_on_stdin_and_never_in_argv()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("password-stdin")?;
    sandbox.serve_fixture()?;
    let digest = format!("sha256:{MANIFEST_HEX}");

    let output = sandbox.run_with(
        &["fetch", REPOSITORY, &digest, &sandbox.path("out")?],
        &[
            ("FETCH_BY_DIGEST_USERNAME", "robot"),
            ("FETCH_BY_DIGEST_PASSWORD", PASSWORD),
        ],
    )?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let argv = fs::read_to_string(sandbox.root().join("oras.log"))?;
    assert!(!argv.contains(PASSWORD), "password in oras argv: {argv}");
    assert_eq!(
        fs::read_to_string(sandbox.root().join("oras.stdin"))?,
        PASSWORD,
        "oras must read the password on stdin"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn writes_the_verified_digest_and_directory_to_github_output()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("github-output")?;
    sandbox.serve_fixture()?;
    let out = sandbox.path("out")?;
    let github_output = sandbox.path("github-output")?;
    let digest = format!("sha256:{MANIFEST_HEX}");

    let output = sandbox.run_with(
        &["fetch", REPOSITORY, &digest, &out],
        &[("GITHUB_OUTPUT", &github_output)],
    )?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(&github_output)?,
        format!("digest=sha256:{MANIFEST_HEX}\ndir={out}\n")
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

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn reads_the_warm_target_digest_from_plugins_toml() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-warm-target")?;
    let config = sandbox.write_config(&plugins_toml())?;

    let output = sandbox.run(&["digest-from-config", &config, "assay.warm_target"])?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("sha256:{LAYER_HEX}\n")
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn refuses_a_warm_target_that_is_a_tag() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("config-warm-target-tag")?;
    let config = sandbox.write_config(&format!(
        "[assay]\ndigest = \"sha256:{MANIFEST_HEX}\"\nwarm_target = \"main\"\n"
    ))?;

    let output = sandbox.run(&["digest-from-config", &config, "assay.warm_target"])?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "no digest may be printed");
    Ok(())
}
