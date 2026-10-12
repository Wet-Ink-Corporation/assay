//! A commit's tree read as data (`docs/architecture/03-contracts.md` §2.1): the entries
//! `git ls-tree -r -z -l` lists, and the bytes `git cat-file --batch` prints for them, both
//! bounded by [`TreeLimits`].

use super::Malformed;

/// What a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular file (mode `100644`).
    File,
    /// An executable file (mode `100755`).
    Executable,
    /// A symbolic link (mode `120000`); its blob is the target's text, never followed.
    Symlink,
    /// A submodule (mode `160000`): a commit of another repository, with no blob here.
    Submodule,
}

/// One entry of a commit's tree, as `ls-tree -l` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Its path from the root of the tree, `/`-separated.
    pub path: String,
    /// What it is.
    pub kind: EntryKind,
    /// Its full object id (a blob, or a commit for a submodule).
    pub object: String,
    /// Its blob's size in bytes; `None` for a submodule.
    pub size: Option<u64>,
}

/// Bounds on reading a tree whose shape the PR controls (H-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLimits {
    /// Most entries one listing may hold.
    pub max_entries: usize,
    /// Most bytes, summed over the listed sizes, one [`ObjectRepo::read_blobs`](super::ObjectRepo::read_blobs) may read.
    pub max_total_bytes: u64,
}

impl Default for TreeLimits {
    /// 100,000 entries and 32 MiB, inside the default 64 MiB output cap with room for
    /// `cat-file --batch` headers.
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 32 << 20,
        }
    }
}

/// Parses `git ls-tree -r -z -l` output: `<mode> <type> <object> <size>\t<path>`,
/// NUL-terminated, the size padded with spaces and `-` for a submodule. A mode other than the
/// four git writes, a type or size that does not match it, or an object name that is not a
/// full hex id is refused, which fails closed.
pub(super) fn parse(stdout: &[u8]) -> Result<Vec<TreeEntry>, Malformed> {
    let Some(fields) = super::nul_terminated_fields(stdout)? else {
        return Ok(Vec::new());
    };
    fields.map(parse_entry).collect()
}

fn parse_entry(field: &str) -> Result<TreeEntry, Malformed> {
    let (meta, path) = field
        .split_once('\t')
        .ok_or(Malformed("an ls-tree entry has no path"))?;
    let mut parts = meta.split_whitespace();
    let (Some(mode), Some(kind), Some(object), Some(size), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(Malformed(
            "an ls-tree entry is not <mode> <type> <object> <size>",
        ));
    };
    let (kind, size) = match (mode, kind, size) {
        ("100644", "blob", size) => (EntryKind::File, blob_size(size)?),
        ("100755", "blob", size) => (EntryKind::Executable, blob_size(size)?),
        ("120000", "blob", size) => (EntryKind::Symlink, blob_size(size)?),
        ("160000", "commit", "-") => (EntryKind::Submodule, None),
        _ => return Err(Malformed("an ls-tree entry has an unknown mode or type")),
    };
    if !is_object_id(object) || path.is_empty() {
        return Err(Malformed("an ls-tree entry has no object id or no path"));
    }
    Ok(TreeEntry {
        path: path.to_owned(),
        kind,
        object: object.to_owned(),
        size,
    })
}

fn blob_size(size: &str) -> Result<Option<u64>, Malformed> {
    size.parse()
        .map(Some)
        .map_err(|_number| Malformed("an ls-tree blob size is not a number"))
}

/// Whether `object` is a full SHA-1 or SHA-256 object id.
pub(super) fn is_object_id(object: &str) -> bool {
    matches!(object.len(), 40 | 64) && object.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Splits `git cat-file --batch` output into the blobs of `expected`, which must come back in
/// order, as blobs, with exactly the listed sizes.
pub(super) fn parse_batch(
    stdout: &[u8],
    expected: &[(&str, u64)],
) -> Result<Vec<Vec<u8>>, Malformed> {
    let mut rest = stdout;
    let mut blobs = Vec::with_capacity(expected.len());
    for &(object, size) in expected {
        let newline = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or(Malformed("a cat-file header is not terminated"))?;
        let (header, body) = rest.split_at(newline);
        let header =
            str::from_utf8(header).map_err(|_utf8| Malformed("a cat-file header is not UTF-8"))?;
        let wanted = format!("{object} blob {size}");
        if header != wanted {
            return Err(Malformed(
                "cat-file printed another object, type or size than listed",
            ));
        }
        let length =
            usize::try_from(size).map_err(|_size| Malformed("a blob is too large to hold"))?;
        let body = body
            .get(1..)
            .ok_or(Malformed("a cat-file header is not terminated"))?;
        let (blob, after) = body
            .split_at_checked(length)
            .ok_or(Malformed("a cat-file blob is truncated"))?;
        rest = after
            .strip_prefix(b"\n")
            .ok_or(Malformed("a cat-file blob is not terminated"))?;
        blobs.push(blob.to_vec());
    }
    if rest.is_empty() {
        Ok(blobs)
    } else {
        Err(Malformed("cat-file printed more than was asked"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    #[test]
    fn parses_each_kind_of_entry_with_its_size() {
        let stdout = format!(
            "100644 blob {OID}      12\ta.txt\0100755 blob {OID}       3\tbin/run\0\
             120000 blob {OID}       7\tlink\0160000 commit {OID}       -\tvendor/sub\0"
        );

        let listed = parse(stdout.as_bytes()).expect("well-formed output");

        assert_eq!(
            listed
                .iter()
                .map(|entry| (entry.kind, entry.path.as_str(), entry.size))
                .collect::<Vec<_>>(),
            [
                (EntryKind::File, "a.txt", Some(12)),
                (EntryKind::Executable, "bin/run", Some(3)),
                (EntryKind::Symlink, "link", Some(7)),
                (EntryKind::Submodule, "vendor/sub", None),
            ],
            "every mode git writes is recognised"
        );
    }

    #[test]
    fn refuses_an_unknown_mode() {
        let stdout = format!("040000 tree {OID}       -\tsrc\0");

        assert_eq!(
            parse(stdout.as_bytes()),
            Err(Malformed("an ls-tree entry has an unknown mode or type")),
            "a tree entry never appears with -r, so it is unreadable output"
        );
    }

    #[test]
    fn refuses_an_abbreviated_object_id() {
        let stdout = "100644 blob e69de29       0\ta.txt\0";

        assert_eq!(
            parse(stdout.as_bytes()),
            Err(Malformed("an ls-tree entry has no object id or no path")),
            "only a full id names one object"
        );
    }

    #[test]
    fn a_batch_whose_size_differs_from_the_listing_is_refused() {
        let stdout = format!("{OID} blob 3\nabc\n");

        assert_eq!(
            parse_batch(stdout.as_bytes(), &[(OID, 2)]),
            Err(Malformed(
                "cat-file printed another object, type or size than listed"
            )),
            "the listing bounded the read, so the read must match it"
        );
    }
}
