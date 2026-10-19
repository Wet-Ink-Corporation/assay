//! One decision atom, `.kb/_intake/decisions/<id>-<slug>.md` (decision 0003): Markdown whose
//! YAML front matter holds `weighin_item: "<id>"`, the value JSON-quoted. Only that line is
//! read; the rest of the front matter is not modelled as YAML. An atom whose front matter has
//! no `weighin_item` is not a Weigh-In atom. A front matter that does not open on the first
//! line or never closes, a second `weighin_item`, a value that is not one JSON string holding a
//! Weigh-In id, and a file name that is not `<id>-<slug>.md` refuse the atom (wi-b5affa).

use super::id::ItemId;

/// The line that opens and closes the front matter.
const FENCE: &str = "---";
/// The front matter key naming the atom's item.
const KEY: &str = "weighin_item:";

/// Why an atom was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AtomFault {
    NotUtf8,
    NoFrontMatter,
    Unclosed,
    Twice,
    NotAnId,
    Misnamed,
}

impl AtomFault {
    /// What is wrong, completing "the atom ...".
    pub(super) const fn text(self) -> &'static str {
        match self {
            Self::NotUtf8 => "is not UTF-8",
            Self::NoFrontMatter => "does not open with a `---` front matter",
            Self::Unclosed => "has a front matter that never closes",
            Self::Twice => "names `weighin_item` twice",
            Self::NotAnId => "has a `weighin_item` that is not one quoted Weigh-In id",
            Self::Misnamed => "is not named `<weighin_item>-<slug>.md`",
        }
    }
}

/// The item the atom named `name` (its file name) with text `bytes` records, or `None` when
/// it is not a Weigh-In atom.
pub(super) fn item(name: &str, bytes: &[u8]) -> Result<Option<ItemId>, AtomFault> {
    let text = str::from_utf8(bytes).map_err(|_utf8| AtomFault::NotUtf8)?;
    let mut lines = text.split('\n');
    if lines.next() != Some(FENCE) {
        return Err(AtomFault::NoFrontMatter);
    }
    let mut found = None;
    for line in lines {
        if line == FENCE {
            return match found {
                Some(id) if named_after(name, &id) => Ok(Some(id)),
                Some(_) => Err(AtomFault::Misnamed),
                None => Ok(None),
            };
        }
        let Some(value) = line.strip_prefix(KEY) else {
            continue;
        };
        if found.is_some() {
            return Err(AtomFault::Twice);
        }
        let id = serde_json::from_str::<String>(value.trim())
            .ok()
            .and_then(ItemId::new)
            .ok_or(AtomFault::NotAnId)?;
        found = Some(id);
    }
    Err(AtomFault::Unclosed)
}

/// Whether the file name `name` is `<id>-<slug>.md` with a non-empty slug.
fn named_after(name: &str, id: &ItemId) -> bool {
    name.strip_prefix(id.as_str())
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".md"))
        .is_some_and(|slug| !slug.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "wi-1e61ab-optional-fp.md";

    fn front(lines: &str) -> String {
        format!("---\nid: \"kb-decision-wi-1e61ab\"\n{lines}---\n\n# Title\n")
    }

    fn id(text: &str) -> Option<ItemId> {
        ItemId::new(text.to_owned())
    }

    #[test]
    fn reads_the_quoted_item_of_an_atom_named_after_it() {
        let text = front("weighin_item: \"wi-1e61ab\"\nquestion: \"Why?\"\n");

        assert_eq!(item(NAME, text.as_bytes()), Ok(id("wi-1e61ab")));
    }

    #[test]
    fn an_atom_without_weighin_item_is_not_a_weigh_in_atom() {
        let text = front("title: \"x\"\n");

        assert_eq!(item("kb-decision-x.md", text.as_bytes()), Ok(None));
    }

    #[test]
    fn a_key_after_the_front_matter_is_body_text() {
        let text = "---\ntitle: x\n---\nweighin_item: \"wi-1e61ab\"\n";

        assert_eq!(item(NAME, text.as_bytes()), Ok(None));
    }

    #[test]
    fn refuses_what_is_not_exactly_the_documented_form() {
        let once = || front("weighin_item: \"wi-1e61ab\"\n");
        let refused = [
            (NAME, front("weighin_item: wi-1e61ab\n"), AtomFault::NotAnId),
            (
                NAME,
                front("weighin_item: \"wi-1E61AB\"\n"),
                AtomFault::NotAnId,
            ),
            (
                NAME,
                front("weighin_item: \"wi-1e61ab\" # x\n"),
                AtomFault::NotAnId,
            ),
            (
                NAME,
                front("weighin_item: \"wi-1e61ab\"\nweighin_item: \"wi-1e61ab\"\n"),
                AtomFault::Twice,
            ),
            ("wi-aaaaaa-other.md", once(), AtomFault::Misnamed),
            ("wi-1e61ab.md", once(), AtomFault::Misnamed),
            ("wi-1e61ab-.md", once(), AtomFault::Misnamed),
            ("wi-1e61abc-x.md", once(), AtomFault::Misnamed),
            (
                NAME,
                "---\nweighin_item: \"wi-1e61ab\"\n".to_owned(),
                AtomFault::Unclosed,
            ),
            (
                NAME,
                "\n---\nweighin_item: \"wi-1e61ab\"\n---\n".to_owned(),
                AtomFault::NoFrontMatter,
            ),
            (
                NAME,
                "---\r\nweighin_item: \"wi-1e61ab\"\r\n---\r\n".to_owned(),
                AtomFault::NoFrontMatter,
            ),
        ];
        for (name, text, fault) in refused {
            assert_eq!(item(name, text.as_bytes()), Err(fault), "{name} {text:?}");
        }
    }

    #[test]
    fn refuses_text_that_is_not_utf8() {
        assert_eq!(item(NAME, b"---\n\xff\n---\n"), Err(AtomFault::NotUtf8));
    }
}
