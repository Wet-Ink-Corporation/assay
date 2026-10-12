//! Files of a commit's tree, read as data (`docs/architecture/03-contracts.md` §2.1): the
//! entries `git ls-tree -r -z` lists at or under a path, with the bytes of each regular file.

use super::Malformed;

/// What one tree entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeContent {
    /// A regular file (mode `100644`) and its bytes.
    File(Vec<u8>),
    /// An executable file (mode `100755`) and its bytes.
    Executable(Vec<u8>),
    /// A symbolic link (mode `120000`); its target is never read or followed.
    Symlink,
    /// A submodule (mode `160000`): a commit of another repository, with no content here.
    Submodule,
}

/// One file of a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    path: String,
    content: TreeContent,
}

impl TreeFile {
    /// The file's path from the root of the tree, `/`-separated.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the entry holds.
    #[must_use]
    pub const fn content(&self) -> &TreeContent {
        &self.content
    }

    /// The path and the content, for a caller that keeps both.
    #[must_use]
    pub fn into_parts(self) -> (String, TreeContent) {
        (self.path, self.content)
    }
}

/// How a listed entry must be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Executable,
    Symlink,
    Submodule,
}

/// One line of `git ls-tree -r -z`: its kind, object name and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listed {
    kind: Kind,
    object: String,
    path: String,
}

impl Listed {
    /// The file this entry is, reading the bytes of a regular or executable file with `blob`
    /// (given the object name); a symlink or a submodule is never read.
    pub(super) fn read<E>(
        self,
        blob: impl FnOnce(&str) -> Result<Vec<u8>, E>,
    ) -> Result<TreeFile, E> {
        let content = match self.kind {
            Kind::File => TreeContent::File(blob(&self.object)?),
            Kind::Executable => TreeContent::Executable(blob(&self.object)?),
            Kind::Symlink => TreeContent::Symlink,
            Kind::Submodule => TreeContent::Submodule,
        };
        Ok(TreeFile {
            path: self.path,
            content,
        })
    }
}

/// Parses `git ls-tree -r -z` output: `<mode> <type> <object>\t<path>`, NUL-terminated. A mode
/// other than the four git writes, a type that does not match it, or an object name that is
/// not a full hex id is refused, which fails closed.
pub(super) fn parse(stdout: &[u8]) -> Result<Vec<Listed>, Malformed> {
    let Some(fields) = super::nul_terminated_fields(stdout)? else {
        return Ok(Vec::new());
    };
    fields.map(parse_entry).collect()
}

fn parse_entry(field: &str) -> Result<Listed, Malformed> {
    let (meta, path) = field
        .split_once('\t')
        .ok_or(Malformed("an ls-tree entry has no path"))?;
    let mut parts = meta.split(' ');
    let (Some(mode), Some(kind), Some(object), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Malformed("an ls-tree entry is not <mode> <type> <object>"));
    };
    let kind = match (mode, kind) {
        ("100644", "blob") => Kind::File,
        ("100755", "blob") => Kind::Executable,
        ("120000", "blob") => Kind::Symlink,
        ("160000", "commit") => Kind::Submodule,
        _ => return Err(Malformed("an ls-tree entry has an unknown mode or type")),
    };
    let full_id = matches!(object.len(), 40 | 64) && object.bytes().all(|b| b.is_ascii_hexdigit());
    if !full_id || path.is_empty() {
        return Err(Malformed("an ls-tree entry has no object id or no path"));
    }
    Ok(Listed {
        kind,
        object: object.to_owned(),
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    #[test]
    fn parses_each_kind_of_entry() {
        let stdout = format!(
            "100644 blob {OID}\ta.txt\0100755 blob {OID}\tbin/run\0\
             120000 blob {OID}\tlink\0160000 commit {OID}\tvendor/sub\0"
        );

        let listed = parse(stdout.as_bytes()).expect("well-formed output");

        assert_eq!(
            listed
                .iter()
                .map(|entry| (entry.kind, entry.path.as_str()))
                .collect::<Vec<_>>(),
            [
                (Kind::File, "a.txt"),
                (Kind::Executable, "bin/run"),
                (Kind::Symlink, "link"),
                (Kind::Submodule, "vendor/sub"),
            ],
            "every mode git writes is recognised"
        );
    }

    #[test]
    fn refuses_an_unknown_mode() {
        let stdout = format!("040000 tree {OID}\tsrc\0");

        assert_eq!(
            parse(stdout.as_bytes()),
            Err(Malformed("an ls-tree entry has an unknown mode or type")),
            "a tree entry never appears with -r, so it is unreadable output"
        );
    }

    #[test]
    fn refuses_an_abbreviated_object_id() {
        let stdout = "100644 blob e69de29\ta.txt\0";

        assert_eq!(
            parse(stdout.as_bytes()),
            Err(Malformed("an ls-tree entry has no object id or no path")),
            "only a full id names one object"
        );
    }
}
