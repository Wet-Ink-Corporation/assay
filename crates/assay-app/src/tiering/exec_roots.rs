//! Exec roots of one manifest (04 §2.2): where a custom build script or a proc-macro crate
//! lives, so an in-place edit there is exec surface.

use toml::{Table, Value};

/// The exec roots the manifest at `manifest_path`, whose text is `manifest`, declares.
///
/// They are canonical repo-relative paths for the domain's `ChangeSet::exec_roots`. Call it
/// for every workspace manifest at base and at head.
///
/// - `[package] build = "<path>"` gives that script, resolved against the manifest's directory
///   (`build = true` gives its `build.rs`).
/// - `[lib] proc-macro = true` gives the crate's directory.
///
/// Fails closed: a manifest that does not parse gives its whole directory, and a build path
/// that is absolute or climbs out of the repository gives `""`, which holds every path.
///
/// # Examples
///
/// ```
/// let manifest = "[package]\nname = \"x\"\nbuild = \"gen.rs\"\n";
/// assert_eq!(assay_app::exec_roots("crates/x/Cargo.toml", manifest), ["crates/x/gen.rs"]);
/// ```
#[must_use]
pub fn exec_roots(manifest_path: &str, manifest: &str) -> Vec<String> {
    let directory = manifest_path
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let Ok(table) = manifest.parse::<Table>() else {
        return vec![directory.to_owned()];
    };
    let build = match table
        .get("package")
        .and_then(|package| package.get("build"))
    {
        Some(Value::String(script)) => Some(script.as_str()),
        Some(Value::Boolean(true)) => Some("build.rs"),
        _ => None,
    };
    let proc_macro = table.get("lib").is_some_and(|lib| {
        ["proc-macro", "proc_macro"]
            .iter()
            .any(|key| lib.get(*key).and_then(Value::as_bool) == Some(true))
    });
    build
        .map(|script| join(directory, script))
        .into_iter()
        .chain(proc_macro.then(|| directory.to_owned()))
        .collect()
}

/// `relative` resolved against `directory`, with `.` and `..` segments removed; `""` when it is
/// absolute or climbs out of the repository.
fn join(directory: &str, relative: &str) -> String {
    if relative.starts_with('/') || relative.contains('\\') {
        return String::new();
    }
    let mut segments: Vec<&str> = directory.split('/').filter(|s| !s.is_empty()).collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return String::new();
                }
            }
            name => segments.push(name),
        }
    }
    segments.join("/")
}
