//! Exec roots of one manifest (04 §2.2): where build scripts and proc-macro crates live, so
//! an in-place edit there is exec surface.

use assay_domain::ExecRoot;
use toml::{Table, Value};

/// The exec roots the manifest at `manifest_path`, whose text is `manifest`, declares.
///
/// They feed the domain's `ChangeSet::exec_roots`; across a PR, use
/// [`manifest_exec_roots`]. The package is `[package]`, or the legacy `[project]`.
///
/// - Its build script: no `build` key, `build = true` and `build = "build.rs"` all give
///   [`ExecRoot::BuildSources`] for the crate, the files a default `build.rs` can reach. A
///   custom `build = "<path>"` gives the script's directory, resolved against the manifest's,
///   since it may pull in siblings through `mod`, `#[path]` or `include!`. `build = false`
///   gives nothing, and any other value (an array, a table, a number) the crate's directory.
/// - A proc-macro crate (`[lib] proc-macro = true`, `proc_macro = true`, or a `crate-type`
///   holding `"proc-macro"`): the crate's directory, and its `[lib] path` with that file's
///   directory.
///
/// Fails closed: a manifest that does not parse gives its whole directory, and a path that is
/// absolute, holds a `\` or climbs out of the repository, or a directory that is the whole
/// repository (a root manifest's custom build script or proc-macro), gives
/// [`ExecRoot::Everything`]. A virtual workspace manifest declares nothing.
///
/// # Examples
///
/// ```
/// use assay_domain::ExecRoot;
///
/// let manifest = "[package]\nname = \"x\"\nbuild = \"tools/gen.rs\"\n";
/// assert_eq!(
///     assay_app::exec_roots("crates/x/Cargo.toml", manifest),
///     [ExecRoot::Path("crates/x/tools".to_owned())]
/// );
/// ```
#[must_use]
pub fn exec_roots(manifest_path: &str, manifest: &str) -> Vec<ExecRoot> {
    let directory = manifest_path
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let Ok(table) = manifest.parse::<Table>() else {
        return vec![placed(Some(directory.to_owned()))];
    };
    let package = table.get("package").or_else(|| table.get("project"));
    let build = package.and_then(|package| build_root(directory, package.get("build")));
    let proc_macro = table
        .get("lib")
        .filter(|lib| is_proc_macro(lib))
        .map(|lib| proc_macro_roots(directory, lib));
    let mut roots = Vec::new();
    for root in build.into_iter().chain(proc_macro.into_iter().flatten()) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// The exec roots of the manifest at `manifest_path` across a PR.
///
/// From its `base` and `head` text (`None` where the manifest does not exist): the roots of
/// both sides, and, when they differ, the manifest itself, so a change that only turns existing
/// code into build-time code (adding `build = "tools/gen.rs"`, setting `proc-macro = true`) is
/// exec surface.
///
/// # Examples
///
/// ```
/// use assay_domain::ExecRoot;
///
/// let base = "[package]\nname = \"x\"\nbuild = false\n";
/// let head = "[package]\nname = \"x\"\nbuild = false\n[lib]\nproc-macro = true\n";
/// let roots = assay_app::manifest_exec_roots("crates/x/Cargo.toml", Some(base), Some(head));
/// assert!(roots.contains(&ExecRoot::Path("crates/x/Cargo.toml".to_owned())));
/// ```
#[must_use]
pub fn manifest_exec_roots(
    manifest_path: &str,
    base: Option<&str>,
    head: Option<&str>,
) -> Vec<ExecRoot> {
    // A manifest that does not exist declares no build-time code.
    let roots_of =
        |text: Option<&str>| text.map_or_else(Vec::new, |text| exec_roots(manifest_path, text));
    let base_roots = roots_of(base);
    let head_roots = roots_of(head);
    let changed = base_roots != head_roots;
    let mut roots = base_roots;
    for root in head_roots {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    if changed {
        roots.push(placed(Some(manifest_path.to_owned())));
    }
    roots
}

/// The root of a package's build script, from its `build` value.
fn build_root(directory: &str, build: Option<&Value>) -> Option<ExecRoot> {
    let sources = || ExecRoot::BuildSources {
        crate_dir: directory.to_owned(),
    };
    match build {
        Some(Value::Boolean(false)) => None,
        None | Some(Value::Boolean(true)) => Some(sources()),
        Some(Value::String(script)) => {
            let resolved = join(directory, script);
            if resolved == join(directory, "build.rs") {
                Some(sources())
            } else {
                Some(placed(parent(resolved.as_deref())))
            }
        }
        Some(_) => Some(placed(Some(directory.to_owned()))),
    }
}

/// A proc-macro crate's directory, then its `[lib] path` and that file's directory.
fn proc_macro_roots(directory: &str, lib: &Value) -> Vec<ExecRoot> {
    let lib_path = lib
        .get("path")
        .and_then(Value::as_str)
        .map(|path| join(directory, path));
    let lib_directory = lib_path.as_ref().map(|path| parent(path.as_deref()));
    [Some(Some(directory.to_owned())), lib_path, lib_directory]
        .into_iter()
        .flatten()
        .map(placed)
        .collect()
}

/// Whether `[lib]` declares a proc-macro crate, in any of cargo's spellings.
fn is_proc_macro(lib: &Value) -> bool {
    let flag = ["proc-macro", "proc_macro"]
        .iter()
        .any(|key| lib.get(*key).and_then(Value::as_bool) == Some(true));
    let crate_type = ["crate-type", "crate_type"].iter().any(|key| {
        lib.get(*key)
            .and_then(Value::as_array)
            .is_some_and(|types| types.iter().any(|t| t.as_str() == Some("proc-macro")))
    });
    flag || crate_type
}

/// A resolved path as a root: `None` (outside the repository) and `""` (its top) hold
/// everything.
fn placed(path: Option<String>) -> ExecRoot {
    match path {
        Some(path) if !path.is_empty() => ExecRoot::Path(path),
        Some(_) | None => ExecRoot::Everything,
    }
}

/// The directory holding `path`; `""` for a top-level file. `None` stays `None`.
fn parent(path: Option<&str>) -> Option<String> {
    path.map(|path| {
        path.rsplit_once('/')
            .map_or_else(String::new, |(directory, _)| directory.to_owned())
    })
}

/// `relative` resolved against `directory`, with `.` and `..` segments removed; `None` when it
/// is absolute, holds a `\`, or climbs out of the repository.
fn join(directory: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') || relative.contains('\\') {
        return None;
    }
    let mut segments: Vec<&str> = directory.split('/').filter(|s| !s.is_empty()).collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            name => segments.push(name),
        }
    }
    Some(segments.join("/"))
}
