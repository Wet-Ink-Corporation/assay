//! Exec roots of one manifest (04 §2.2): where a custom build script or a proc-macro crate
//! lives, so an in-place edit there is exec surface.

use assay_domain::ExecRoot;
use toml::{Table, Value};

/// The exec roots the manifest at `manifest_path`, whose text is `manifest`, declares.
///
/// They feed the domain's `ChangeSet::exec_roots`. Call it for every workspace manifest at
/// base and at head.
///
/// - `[package] build = "<path>"`: the script's directory, resolved against the manifest's,
///   since the script may pull in siblings through `mod`, `#[path]` or `include!`;
///   `build = true` gives the crate's directory (where its `build.rs` is).
/// - A proc-macro crate (`[lib] proc-macro = true`, `proc_macro = true`, or a `crate-type`
///   holding `"proc-macro"`): the crate's directory, and its `[lib] path` with that file's
///   directory.
///
/// A default `build.rs` (no `build` key) gives no root: the manifest cannot say whether it
/// exists, the `**/build.rs` glob covers the script itself, and #64's `syn` scan adds the
/// modules it reaches.
///
/// Fails closed: a manifest that does not parse gives its whole directory, and a path that is
/// absolute, holds a `\` or climbs out of the repository, or a root manifest's directory,
/// gives [`ExecRoot::Everything`].
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
        return vec![root(Some(directory.to_owned()))];
    };
    let build = match table
        .get("package")
        .and_then(|package| package.get("build"))
    {
        Some(Value::String(script)) => Some(parent(join(directory, script).as_deref())),
        Some(Value::Boolean(true)) => Some(Some(directory.to_owned())),
        _ => None,
    };
    let lib = table.get("lib");
    let proc_macro_roots = lib.filter(|lib| is_proc_macro(lib)).map(|lib| {
        let lib_path = lib
            .get("path")
            .and_then(Value::as_str)
            .map(|path| join(directory, path));
        let lib_directory = lib_path.as_ref().map(|path| parent(path.as_deref()));
        [Some(Some(directory.to_owned())), lib_path, lib_directory]
    });
    build
        .into_iter()
        .chain(proc_macro_roots.into_iter().flatten().flatten())
        .map(root)
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
fn root(path: Option<String>) -> ExecRoot {
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
