use std::fs;
use std::path::Path;

use super::*;

fn scalar(text: &str) -> Node {
    Node::Scalar(text.to_owned())
}

fn map(entries: &[(&str, Node)]) -> Node {
    Node::Map(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

/// What `text` was refused for, and on which line.
fn refused(text: &str) -> (usize, YamlRefusal) {
    let refusal = read(text).expect_err("the text is refused");
    (refusal.line(), refusal.reason())
}

#[test]
fn mappings_sequences_and_scalars_are_read() {
    let text = "\
# a comment
name: rust   # trailing
on:
  push:
    branches: [main, 'release/*']
  pull_request:
jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@0123 # v4
        with:
          fetch-depth: 0
      - run: 'echo a # b' # a comment
        shell: bash  # a comment
";

    let node = read(text).expect("the text is read");

    let steps = Node::Seq(vec![
        map(&[
            ("uses", scalar("actions/checkout@0123")),
            ("with", map(&[("fetch-depth", scalar("0"))])),
        ]),
        map(&[("run", scalar("echo a # b")), ("shell", scalar("bash"))]),
    ]);
    assert_eq!(
        node,
        map(&[
            ("name", scalar("rust")),
            (
                "on",
                map(&[
                    (
                        "push",
                        map(&[(
                            "branches",
                            Node::Seq(vec![scalar("main"), scalar("release/*")])
                        )])
                    ),
                    ("pull_request", Node::Null),
                ])
            ),
            (
                "jobs",
                map(&[(
                    "gate",
                    map(&[("runs-on", scalar("ubuntu-latest")), ("steps", steps)])
                )])
            ),
        ])
    );
}

#[test]
fn quoted_scalars_and_keys_are_unquoted() {
    let node = read("'on': \"push\"\nx: 'it''s'\ny: {}\nz: []\n").expect("read");

    assert_eq!(
        node,
        map(&[
            ("on", scalar("push")),
            ("x", scalar("it's")),
            ("y", Node::Map(Vec::new())),
            ("z", Node::Seq(Vec::new())),
        ])
    );
}

#[test]
fn a_block_scalar_keeps_its_lines_and_ends_at_the_indent() {
    let text = "run: |\n  echo ${{ secrets.TOKEN }}\n\n  # not a comment\nnext: 1\n";

    let node = read(text).expect("read");

    assert_eq!(
        node,
        map(&[
            (
                "run",
                Node::Text("echo ${{ secrets.TOKEN }}\n\n# not a comment".to_owned())
            ),
            ("next", scalar("1")),
        ])
    );
}

#[test]
fn a_sequence_may_sit_at_its_keys_indent() {
    let node = read("on:\n- push\n- pull_request_target\n").expect("read");

    assert_eq!(
        node,
        map(&[(
            "on",
            Node::Seq(vec![scalar("push"), scalar("pull_request_target")])
        )])
    );
}

#[test]
fn a_leading_document_marker_is_accepted() {
    assert_eq!(read("---\na: 1\n"), Ok(map(&[("a", scalar("1"))])));
}

#[test]
fn what_the_reader_cannot_classify_is_refused() {
    let cases: [(&str, (usize, YamlRefusal)); 14] = [
        ("a:\n\tb: 1\n", (2, YamlRefusal::Tab)),
        ("a: &x 1\n", (1, YamlRefusal::Indicator)),
        ("a: *x\n", (1, YamlRefusal::Indicator)),
        ("a: !!str 1\n", (1, YamlRefusal::Indicator)),
        ("<<: 1\n", (1, YamlRefusal::Key)),
        ("a: {b: 1}\n", (1, YamlRefusal::FlowMapping)),
        ("a: [b,\n  c]\n", (1, YamlRefusal::FlowSequence)),
        ("a: 1\na: 2\n", (2, YamlRefusal::RepeatedKey)),
        ("a: one\n  two\n", (2, YamlRefusal::Indentation)),
        ("a: 1\n---\nb: 2\n", (2, YamlRefusal::Document)),
        ("a: b: c\n", (1, YamlRefusal::Plain)),
        ("a: 'open\n", (1, YamlRefusal::Quote)),
        ("a: \"back\\\\slash\"\n", (1, YamlRefusal::Quote)),
        ("- - a\n", (1, YamlRefusal::Indicator)),
    ];

    for (text, expected) in cases {
        assert_eq!(refused(text), expected, "{text:?}");
    }
}

#[test]
fn characters_yaml_reads_as_breaks_or_that_are_controls_are_refused() {
    // `\r`, NEL, LS and PS break lines for YAML, so each would hide the key after it.
    let cases = [
        ("on:\n  push:\n    branches: x\r  pull_request: y\n", 3),
        ("a: x\u{85}b: y\n", 1),
        ("a: x\u{2028}b: y\n", 1),
        ("a: x\n\nb: y\u{2029}c: z\n", 3),
        ("\u{feff}a: x\n", 1),
        ("a: x\u{0}\n", 1),
        ("a: 'x\u{1b}'\n", 1),
        ("a: |\n  x\u{7f}\n", 2),
        ("a: x\u{9b}\n", 1),
        ("a: x\r", 1),
    ];

    for (text, line) in cases {
        assert_eq!(refused(text), (line, YamlRefusal::Character), "{text:?}");
    }
}

#[test]
fn crlf_line_ends_are_read_as_line_ends() {
    assert_eq!(
        read("a: x\r\nb: |\r\n  y\r\n"),
        Ok(map(&[
            ("a", scalar("x")),
            ("b", Node::Text("y".to_owned()))
        ]))
    );
}

#[test]
fn a_tab_outside_quoted_or_block_scalar_text_is_refused() {
    let cases = [
        "a:\tb\n",
        "a: b\t# c\n",
        "a: b\t\n",
        "- a:\tb\n",
        "- \tb\n",
        "a: [x,\ty]\n",
        "a: [x\t, y]\n",
        "a: ['x']\t\n",
        "a: 'x'\t# c\n",
        "a: {}\t\n",
        "a: |\t\n  x\n",
        "a: # c\td\n",
        "# c\td\na: b\n",
        "a: b\n\t\nc: d\n",
    ];

    for text in cases {
        assert_eq!(refused(text).1, YamlRefusal::Tab, "{text:?}");
    }
}

#[test]
fn a_tab_inside_quoted_or_block_scalar_text_is_kept() {
    assert_eq!(
        read("a: 'x\ty'\nb: [\"p\tq\"]\nc: |\n  r\ts\n"),
        Ok(map(&[
            ("a", scalar("x\ty")),
            ("b", Node::Seq(vec![scalar("p\tq")])),
            ("c", Node::Text("r\ts".to_owned())),
        ]))
    );
}

#[test]
fn a_mention_ignores_ascii_case() {
    let node = read("env:\n  KEY: ${{ SECRETS.TOKEN }}\n").expect("read");

    assert!(node.mentions_ignoring_case("secrets"));
    assert!(!node.mentions_ignoring_case("vars"));
}

#[test]
fn nesting_past_the_cap_is_refused() {
    let lines: Vec<String> = (0..=MAX_DEPTH)
        .map(|depth| format!("{}k:\n", "  ".repeat(depth)))
        .collect();
    let text = lines.concat();

    assert_eq!(refused(&text).1, YamlRefusal::Depth);
}

#[test]
fn an_empty_document_is_null() {
    assert_eq!(read("# nothing\n\n"), Ok(Node::Null));
}

#[test]
fn the_repositorys_own_workflows_are_read() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows");
    let mut read_any = false;
    for entry in fs::read_dir(&dir).expect("the workflows directory is readable") {
        let path = entry.expect("a directory entry").path();
        let text = fs::read_to_string(&path).expect("a workflow is UTF-8");

        let node = read(&text).unwrap_or_else(|refusal| panic!("{}: {refusal}", path.display()));

        assert!(node.get("jobs").is_some(), "{}", path.display());
        read_any = true;
    }
    assert!(read_any, "the repository has workflows");
}
