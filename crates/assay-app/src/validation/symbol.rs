//! The enclosing function of a line, for the fingerprint and the omission anchor (03 §3,
//! 04 §4 step 2).
//!
//! A deterministic heuristic over formatted Rust, not a parser: a line belongs to the nearest
//! `fn` signature above it reached through lines each indented less than the one before (the
//! chain of enclosing blocks), skipping blank and continuation lines. Its answers only need to be
//! stable for one text, which they are; where it misreads unusual layout (a multi-line string at
//! column 0 inside a body), fingerprints stay stable and a dismissal fails safe by not matching.

/// Lines that continue a construct rather than open one: closing delimiters, `where` and
/// return-type lines of a signature, comments and attributes.
const CONTINUATIONS: [&str; 10] = [")", "]", "}", "{", "->", "where", "//", "/*", "*", "#["];

/// Words that may precede `fn` in a signature.
const QUALIFIERS: [&str; 5] = ["const", "async", "unsafe", "default", "safe"];

/// A line that opens a block other lines may sit in.
struct Opener<'t> {
    /// Its indentation.
    depth: usize,
    /// The function it is or sits in, empty at file scope.
    function: &'t str,
}

/// The function each line of `text` belongs to, by 0-based line index; empty at file scope. A
/// signature line belongs to its own function.
///
/// One forward pass keeps the chain of open blocks as a stack of strictly increasing depth, so
/// every line is answered in a single pass over the file, not one scan per line asked about.
pub(super) fn enclosing_functions(text: &str) -> Vec<&str> {
    let mut open: Vec<Opener<'_>> = Vec::new();
    let mut functions = Vec::new();
    for line in text.lines() {
        let code = line.trim_start();
        let opens =
            !code.is_empty() && !CONTINUATIONS.iter().any(|prefix| code.starts_with(prefix));
        // A blank line sits in whatever is open; the others in the nearest shallower opener.
        let depth = if code.is_empty() {
            usize::MAX
        } else {
            line.len().saturating_sub(code.len())
        };
        if opens {
            while open.last().is_some_and(|opener| opener.depth >= depth) {
                open.pop();
            }
        }
        // Depths rise strictly up the stack, so the nearest shallower opener is found by binary
        // search: O(log d) per line, however deep the nesting and however many blank lines.
        let shallower = open.partition_point(|opener| opener.depth < depth);
        let outer = shallower
            .checked_sub(1)
            .and_then(|index| open.get(index))
            .map_or("", |opener| opener.function);
        let function = signature_name(line).unwrap_or(outer);
        functions.push(function);
        if opens {
            open.push(Opener { depth, function });
        }
    }
    functions
}

/// The name a function signature on `line` declares (`pub(crate) async fn name(`), if it is one.
pub(super) fn signature_name(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    loop {
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (word, after) = rest.split_at_checked(end)?;
        let after = after.trim_start();
        rest = match word {
            "fn" => {
                let name_end = after
                    .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '#'))
                    .unwrap_or(after.len());
                let name = after.get(..name_end)?;
                return (!name.is_empty()).then_some(name);
            }
            "pub" => match after.strip_prefix('(') {
                Some(scope) => scope.split_once(')')?.1.trim_start(),
                None => after,
            },
            "extern" => match after.strip_prefix('"') {
                Some(abi) => abi.split_once('"')?.1.trim_start(),
                None => after,
            },
            _ if QUALIFIERS.contains(&word) => after,
            _ => return None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{enclosing_functions, signature_name};

    /// The function 1-based `line` of `text` belongs to.
    fn enclosing(text: &str, line: usize) -> &str {
        enclosing_functions(text)[line - 1]
    }

    const FILE: &str = "\
use std::fmt;

impl Thing {
    pub(crate) async fn load(
        &self,
        path: &str,
    ) -> Result<(), Error>
    where
        Self: Sized,
    {
        if path.is_empty() {
            return Err(Error);
        } else {
            go();
        }
        Ok(())
    }

    const LIMIT: u32 = 3;
}

fn free() {
    struct Local {
        field: u32,
    }
}
";

    #[test]
    fn reads_the_name_of_qualified_signatures() {
        let names = [
            "fn a()",
            "    pub fn b<T>(t: T)",
            "pub(in crate::x) const unsafe fn c()",
            "pub extern \"C\" fn d()",
            "async fn r#match()",
        ]
        .map(signature_name);

        assert_eq!(
            names,
            [Some("a"), Some("b"), Some("c"), Some("d"), Some("r#match")]
        );
    }

    #[test]
    fn reads_no_name_from_other_lines() {
        let names = [
            "let f = fn_ptr;",
            "pub const X: u32 = 1;",
            "fn",
            "// fn a()",
        ]
        .map(signature_name);

        assert_eq!(names, [None; 4]);
    }

    #[test]
    fn a_body_line_belongs_to_its_function_through_a_multi_line_signature() {
        assert_eq!(
            [11, 12, 14, 16].map(|line| enclosing(FILE, line)),
            ["load"; 4]
        );
    }

    #[test]
    fn a_signature_line_belongs_to_its_own_function() {
        assert_eq!(enclosing(FILE, 4), "load");
    }

    #[test]
    fn items_outside_functions_are_at_file_scope() {
        assert_eq!([1, 3, 19, 20].map(|line| enclosing(FILE, line)), [""; 4]);
    }

    #[test]
    fn an_item_nested_in_a_function_belongs_to_it() {
        assert_eq!(enclosing(FILE, 24), "free");
    }

    #[test]
    fn a_closing_brace_sits_in_the_block_around_the_one_it_closes() {
        assert_eq!(
            [15, 17, 25, 26].map(|line| enclosing(FILE, line)),
            ["load", "", "free", ""]
        );
    }

    #[test]
    fn an_empty_text_has_no_lines() {
        assert_eq!(enclosing_functions(""), Vec::<&str>::new());
    }
}
