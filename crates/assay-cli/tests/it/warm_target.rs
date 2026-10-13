//! `.github/actions/warm-target/warm-target.sh` (ADR-21, `06 §3`, `05` T6): the warm `target/`
//! is packed as `target/` plus the cargo registry index and cache, unpacked only when every entry
//! is a regular file or directory under that allowlist (otherwise nothing is written), and the
//! workspace crates are cleaned with one `cargo clean -p`. Fakes of `zstd` (a plain tar passes
//! through) and `cargo` (a recorded `cargo metadata`) are put on `PATH`, so nothing compiles and
//! nothing touches the network (TST-10).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `zstd` stand-in: any argument not starting with `-` is the input file, else stdin; the
/// bytes are copied to stdout unchanged in both directions.
const FAKE_ZSTD: &str = r#"#!/usr/bin/env bash
set -euo pipefail
file=
for arg in "$@"; do
  case $arg in -*) ;; *) file=$arg ;; esac
done
if [ -n "$file" ]; then cat -- "$file"; else cat; fi
"#;

/// `cargo` stand-in: logs `<cwd>|<argv>` and answers `metadata` from `$FAKE_CARGO_METADATA`.
const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s|%s\n' "$PWD" "$*" >> "$FAKE_CARGO_LOG"
case $1 in
  metadata) cat -- "$FAKE_CARGO_METADATA" ;;
  clean) ;;
  *) exit 64 ;;
esac
"#;

/// Writes a plain tar at `argv[1]` from `kind:name:payload` specs: `file` (payload is the
/// content), `dir`, `symlink` and `hardlink` (payload is the link target), `fifo`.
const MAKE_TAR: &str = r#"
import io, sys, tarfile
kinds = {"file": tarfile.REGTYPE, "dir": tarfile.DIRTYPE, "symlink": tarfile.SYMTYPE,
         "hardlink": tarfile.LNKTYPE, "fifo": tarfile.FIFOTYPE}
with tarfile.open(sys.argv[1], "w", format=tarfile.PAX_FORMAT) as tar:
    for spec in sys.argv[2:]:
        kind, name, payload = spec.split(":", 2)
        info = tarfile.TarInfo(name)
        info.type = kinds[kind]
        info.mode = 0o755 if kind == "dir" else 0o644
        data = payload.encode() if kind == "file" else b""
        if kind in ("symlink", "hardlink"):
            info.linkname = payload
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
"#;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/actions/warm-target/warm-target.sh")
}

/// A per-test directory: `bin/` with the fakes, `ws/` (the workspace), `cargo-home/`.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(test: &str) -> io::Result<Self> {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("warm-target")
            .join(test);
        match fs::remove_dir_all(&root) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        for dir in ["bin", "ws", "cargo-home"] {
            fs::create_dir_all(root.join(dir))?;
        }
        for (name, text) in [("zstd", FAKE_ZSTD), ("cargo", FAKE_CARGO)] {
            let fake = root.join("bin").join(name);
            fs::write(&fake, text)?;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))?;
        }
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write(&self, name: &str, text: &str) -> io::Result<()> {
        let path = self.path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)
    }

    fn run(&self, args: &[&Path]) -> io::Result<Output> {
        self.run_with(args, &[])
    }

    /// Runs the script with `env` added; the test-only cap variables are otherwise unset.
    fn run_with(&self, args: &[&Path], env: &[(&str, &str)]) -> io::Result<Output> {
        let mut path = vec![self.path("bin")];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path: OsString =
            std::env::join_paths(path).map_err(|error| io::Error::other(error.to_string()))?;
        Command::new("bash")
            .arg(script())
            .args(args)
            .env("PATH", path)
            .env("FAKE_CARGO_LOG", self.path("cargo.log"))
            .env("FAKE_CARGO_METADATA", self.path("metadata.json"))
            .env_remove("WARM_TARGET_TEST_MAX_ENTRIES")
            .env_remove("WARM_TARGET_TEST_MAX_BYTES")
            .envs(env.iter().copied())
            .output()
    }

    fn unpack(&self, archive: &Path) -> io::Result<Output> {
        self.unpack_with(archive, &[])
    }

    fn unpack_with(&self, archive: &Path, env: &[(&str, &str)]) -> io::Result<Output> {
        self.run_with(
            &[
                Path::new("unpack"),
                archive,
                &self.path("ws"),
                &self.path("cargo-home"),
            ],
            env,
        )
    }

    fn nothing_written(&self) -> io::Result<bool> {
        Ok(files(&self.path("ws"))?.is_empty() && files(&self.path("cargo-home"))?.is_empty())
    }

    /// A plain tar (the fake `zstd` passes it through) built from `MAKE_TAR` specs.
    fn archive(&self, specs: &[&str]) -> io::Result<PathBuf> {
        let archive = self.path("crafted.tar.zst");
        let output = Command::new("python3")
            .args(["-I", "-c", MAKE_TAR])
            .arg(&archive)
            .args(specs)
            .output()?;
        if output.status.success() {
            Ok(archive)
        } else {
            Err(io::Error::other(stderr(&output)))
        }
    }

    fn cargo_log(&self) -> io::Result<String> {
        match fs::read_to_string(self.path("cargo.log")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            read => read,
        }
    }
}

/// Every file under `dir`, relative to it, sorted; directories are walked, not listed.
fn files(dir: &Path) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(dir).map_err(io::Error::other)?;
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn pack_then_unpack_restores_target_and_the_registry_cache_but_not_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new("round-trip")?;
    sandbox.write("ws/target/debug/deps/libdep.rlib", "rlib")?;
    sandbox.write("ws/src/lib.rs", "pub fn f() {}")?;
    sandbox.write("cargo-home/registry/index/idx/config.json", "{}")?;
    sandbox.write("cargo-home/registry/cache/idx/dep-1.0.0.crate", "crate")?;
    sandbox.write("cargo-home/registry/src/idx/dep-1.0.0/lib.rs", "src")?;
    sandbox.write("cargo-home/bin/cargo-tool", "tool")?;
    let archive = sandbox.path("warm-target.tar.zst");
    let packed = sandbox.run(&[
        Path::new("pack"),
        &sandbox.path("ws"),
        &sandbox.path("cargo-home"),
        &archive,
    ])?;
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
    fs::remove_dir_all(sandbox.path("ws"))?;
    fs::remove_dir_all(sandbox.path("cargo-home"))?;
    fs::create_dir(sandbox.path("ws"))?;

    let unpacked = sandbox.unpack(&archive)?;

    assert_eq!(unpacked.status.code(), Some(0), "{}", stderr(&unpacked));
    assert_eq!(
        files(&sandbox.path("ws"))?,
        ["target/debug/deps/libdep.rlib"]
    );
    assert_eq!(
        files(&sandbox.path("cargo-home"))?,
        [
            "registry/cache/idx/dep-1.0.0.crate",
            "registry/index/idx/config.json"
        ]
    );
    assert_eq!(
        fs::read_to_string(sandbox.path("ws/target/debug/deps/libdep.rlib"))?,
        "rlib"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn pack_turns_cargo_hardlinks_into_regular_files_and_keeps_the_exec_bit()
-> Result<(), Box<dyn std::error::Error>> {
    // cargo hardlinks a build script's binary to its uplift; tar would store the second name as
    // a hardlink entry, which unpack refuses.
    let sandbox = Sandbox::new("hardlinks")?;
    let build = "ws/target/debug/build/x";
    sandbox.write(&format!("{build}/build_script_build-0123"), "script")?;
    fs::set_permissions(
        sandbox.path(&format!("{build}/build_script_build-0123")),
        fs::Permissions::from_mode(0o755),
    )?;
    fs::hard_link(
        sandbox.path(&format!("{build}/build_script_build-0123")),
        sandbox.path(&format!("{build}/build-script-build")),
    )?;
    sandbox.write("cargo-home/registry/index/idx/config.json", "{}")?;
    sandbox.write("cargo-home/registry/cache/idx/dep-1.0.0.crate", "crate")?;
    let archive = sandbox.path("warm-target.tar.zst");
    let packed = sandbox.run(&[
        Path::new("pack"),
        &sandbox.path("ws"),
        &sandbox.path("cargo-home"),
        &archive,
    ])?;
    assert_eq!(packed.status.code(), Some(0), "{}", stderr(&packed));
    fs::remove_dir_all(sandbox.path("ws"))?;
    fs::create_dir(sandbox.path("ws"))?;

    let unpacked = sandbox.unpack(&archive)?;

    assert_eq!(unpacked.status.code(), Some(0), "{}", stderr(&unpacked));
    for name in ["build_script_build-0123", "build-script-build"] {
        let path = sandbox.path(&format!("{build}/{name}"));
        let metadata = fs::symlink_metadata(&path)?;
        assert!(metadata.is_file(), "{name} is not a regular file");
        assert_eq!(fs::read_to_string(&path)?, "script", "{name} bytes");
        assert_eq!(
            metadata.permissions().mode() & 0o111,
            0o111,
            "{name} exec bit"
        );
    }
    Ok(())
}

/// Unpacks an archive of `specs` with the test-only cap `variable` lowered to `cap`, and
/// returns `(exit code, nothing written)`.
fn unpack_under_cap(
    test: &str,
    specs: &[&str],
    variable: &str,
    cap: &str,
) -> io::Result<(Option<i32>, bool)> {
    let sandbox = Sandbox::new(test)?;
    let archive = sandbox.archive(specs)?;

    let output = sandbox.unpack_with(&archive, &[(variable, cap)])?;

    Ok((output.status.code(), sandbox.nothing_written()?))
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn accepts_an_archive_at_the_entry_cap_and_refuses_one_more_entry_writing_nothing() -> io::Result<()>
{
    let at_cap = ["dir:target:", "file:target/a:a", "file:target/b:b"];
    let over_cap = [at_cap.as_slice(), &["file:target/c:c"]].concat();

    let accepted = unpack_under_cap(
        "entries-at-cap",
        &at_cap,
        "WARM_TARGET_TEST_MAX_ENTRIES",
        "3",
    )?;
    let refused = unpack_under_cap(
        "entries-over-cap",
        &over_cap,
        "WARM_TARGET_TEST_MAX_ENTRIES",
        "3",
    )?;

    assert_eq!(
        accepted,
        (Some(0), false),
        "three entries under a cap of three"
    );
    assert_eq!(
        refused,
        (Some(1), true),
        "four entries under a cap of three"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn accepts_an_archive_at_the_byte_cap_and_refuses_one_more_byte_writing_nothing() -> io::Result<()>
{
    let accepted = unpack_under_cap(
        "bytes-at-cap",
        &["file:target/ok:x", "file:target/a:123456789"],
        "WARM_TARGET_TEST_MAX_BYTES",
        "10",
    )?;
    let refused = unpack_under_cap(
        "bytes-over-cap",
        &["file:target/ok:x", "file:target/a:1234567890"],
        "WARM_TARGET_TEST_MAX_BYTES",
        "10",
    )?;

    assert_eq!(accepted, (Some(0), false), "ten bytes under a cap of ten");
    assert_eq!(refused, (Some(1), true), "eleven bytes under a cap of ten");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn refuses_an_empty_archive() -> io::Result<()> {
    let sandbox = Sandbox::new("empty")?;
    let archive = sandbox.archive(&[])?;

    let output = sandbox.unpack(&archive)?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(sandbox.nothing_written()?, "nothing may be written");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn refuses_a_cap_variable_that_is_not_a_count() -> io::Result<()> {
    let sandbox = Sandbox::new("cap-not-a-number")?;
    let archive = sandbox.archive(&["file:target/ok:ok"])?;

    let output = sandbox.unpack_with(&archive, &[("WARM_TARGET_TEST_MAX_BYTES", "-1")])?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(sandbox.nothing_written()?, "nothing may be written");
    Ok(())
}

/// Unpacks an archive of a valid entry followed by `hostile`, and reports whether the script
/// refused it (exit 1) with nothing written.
fn refused_with_nothing_written(test: &str, hostile: &str) -> io::Result<bool> {
    let sandbox = Sandbox::new(test)?;
    let archive = sandbox.archive(&["file:target/ok:ok", hostile])?;

    let output = sandbox.unpack(&archive)?;

    Ok(output.status.code() == Some(1)
        && files(&sandbox.path("ws"))?.is_empty()
        && files(&sandbox.path("cargo-home"))?.is_empty())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn refuses_an_entry_outside_the_allowlist_and_writes_nothing() -> io::Result<()> {
    let hostile = [
        "file:registry/src/idx/dep/lib.rs:src",
        "file:.cargo/config.toml:[build]",
        "file:src/lib.rs:code",
        "file:registry:flat",
        "file:targets/x:x",
        "file:target/../escape:x",
        "file:./target/x:x",
        "file:target//x:x",
        "file:/tmp/absolute:x",
        "file:target/new\nline:x",
    ];

    let accepted = hostile
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            Ok((
                *spec,
                refused_with_nothing_written(&format!("path-{index}"), spec)?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|&(_, refused)| !refused)
        .map(|(spec, _)| spec)
        .collect::<Vec<_>>();

    assert_eq!(accepted, Vec::<&str>::new(), "entries not refused");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn refuses_links_and_special_files_and_writes_nothing() -> io::Result<()> {
    let hostile = [
        "symlink:target/link:/etc",
        "symlink:target/inside:ok",
        "hardlink:target/hard:target/ok",
        "fifo:target/pipe:",
    ];

    let accepted = hostile
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            Ok((
                *spec,
                refused_with_nothing_written(&format!("type-{index}"), spec)?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|&(_, refused)| !refused)
        .map(|(spec, _)| spec)
        .collect::<Vec<_>>();

    assert_eq!(accepted, Vec::<&str>::new(), "entries not refused");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash and python3; Miri cannot run processes")]
fn refuses_a_target_directory_that_links_out_of_the_workspace() -> io::Result<()> {
    let sandbox = Sandbox::new("target-symlink")?;
    fs::create_dir(sandbox.path("outside"))?;
    symlink(sandbox.path("outside"), sandbox.path("ws/target"))?;
    let archive = sandbox.archive(&["dir:target:", "file:target/ok:ok"])?;

    let output = sandbox.unpack(&archive)?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        files(&sandbox.path("outside"))?,
        Vec::<String>::new(),
        "nothing may be written through the link"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn clean_runs_one_cargo_clean_for_every_workspace_package_in_the_workspace() -> io::Result<()> {
    let sandbox = Sandbox::new("clean")?;
    sandbox.write(
        "metadata.json",
        r#"{"packages":[{"name":"assay-app"},{"name":"assay_cli"}],"version":1}"#,
    )?;
    let workspace = sandbox.path("ws");

    let output = sandbox.run(&[Path::new("clean"), &workspace])?;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let workspace = workspace.display();
    assert_eq!(
        sandbox.cargo_log()?,
        format!(
            "{workspace}|metadata --locked --no-deps --format-version 1\n\
             {workspace}|clean --locked -p assay-app -p assay_cli\n"
        )
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns bash; Miri cannot run processes")]
fn clean_refuses_a_package_name_that_is_not_a_cargo_name() -> io::Result<()> {
    let refused = ["--config=build.rustc='/tmp/x'", "", "a b", "-p"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let sandbox = Sandbox::new(&format!("clean-name-{index}"))?;
            let json = format!(
                r#"{{"packages":[{{"name":"ok"}},{{"name":{}}}]}}"#,
                serde_json::Value::from(*name)
            );
            sandbox.write("metadata.json", &json)?;
            let output = sandbox.run(&[Path::new("clean"), &sandbox.path("ws")])?;
            let cleaned = sandbox.cargo_log()?.contains("|clean");
            Ok(output.status.code() == Some(1) && !cleaned)
        })
        .collect::<io::Result<Vec<_>>>()?;

    assert_eq!(refused, [true; 4], "every malformed name refuses the clean");
    Ok(())
}
