//! Push records (`pushes/<repo_id>/<pr>.jsonl`, written by `open`) and command records
//! (`commands/<repo_id>/<pr>.jsonl`, written by decide-only): append-only streams by
//! compare-and-set, one record per run or per comment (06 §1, R3-02, R3-07).

use std::error::Error;

use assay_app::{
    Appended, CommandRecord, CommentId, Corruption, LedgerPath, LedgerReadError, LedgerWriteError,
    PushAppended, PushDraft, PushKind, PushRecord, UserId, WriteOutcome, append_command,
    append_push, read_commands, read_pushes,
};

use crate::ledger_fakes::{MemoryLedger, at, commit, digest, id, run};

fn pushes() -> LedgerPath {
    LedgerPath::pushes(id(42), id(7))
}

fn commands() -> LedgerPath {
    LedgerPath::commands(id(42), id(7))
}

fn push(run_id: u64, when: &str) -> PushDraft {
    PushDraft {
        run_uid: run(run_id),
        pr: id(7),
        kind: PushKind::Push {
            before: commit('a'),
            forced: false,
        },
        after: commit('b'),
        sender_id: UserId(id(1001)),
        at: at(when),
    }
}

fn push_line(run_id: u64, seq: u64, when: &str) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-push/1","run_uid":{{"repo_id":42,"run_id":{run_id},"#,
            r#""run_attempt":1}},"pr":7,"seq":{seq},"kind":"push","before":"{before}","#,
            r#""after":"{after}","sender_id":1001,"forced":false,"at":"{when}"}}"#,
            "\n"
        ),
        run_id = run_id,
        seq = seq,
        before = "a".repeat(40),
        after = "b".repeat(40),
        when = when,
    )
}

fn recorded(draft: PushDraft, seq: u64) -> PushRecord {
    PushRecord {
        seq: id(seq),
        push: draft,
    }
}

#[test]
fn the_first_push_record_of_a_pr_is_number_one() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let appended = append_push(&ledger, &push(1, "2026-10-12T09:30:00.000Z"))?;

    assert_eq!(
        appended,
        PushAppended::Recorded(recorded(push(1, "2026-10-12T09:30:00.000Z"), 1))
    );
    assert_eq!(pushes().as_str(), "pushes/42/7.jsonl");
    assert_eq!(
        ledger.text(&pushes()),
        push_line(1, 1, "2026-10-12T09:30:00.000Z")
    );
    Ok(())
}

#[test]
fn a_concurrent_push_record_keeps_the_sequence_and_both_records() -> Result<(), Box<dyn Error>> {
    let rival = push_line(2, 1, "2026-10-12T09:29:00.000Z");
    let ledger = MemoryLedger::default().rival_appends(rival.as_bytes());

    let appended = append_push(&ledger, &push(1, "2026-10-12T09:30:00.000Z"))?;

    assert_eq!(
        appended,
        PushAppended::Recorded(recorded(push(1, "2026-10-12T09:30:00.000Z"), 2))
    );
    assert_eq!(
        read_pushes(&ledger, id(42), id(7))?,
        [
            recorded(push(2, "2026-10-12T09:29:00.000Z"), 1),
            recorded(push(1, "2026-10-12T09:30:00.000Z"), 2),
        ]
    );
    let outcomes: Vec<_> = ledger.writes().into_iter().map(|w| w.outcome).collect();
    assert_eq!(outcomes, [WriteOutcome::Conflict, WriteOutcome::Written]);
    Ok(())
}

#[test]
fn a_run_s_push_record_is_written_once() -> Result<(), Box<dyn Error>> {
    let first = push_line(1, 1, "2026-10-12T09:30:00.000Z");
    let ledger = MemoryLedger::default().with_file(&pushes(), first.as_bytes());

    let appended = append_push(&ledger, &push(1, "2026-10-12T09:45:00.000Z"))?;

    assert_eq!(
        appended,
        PushAppended::AlreadyRecorded(recorded(push(1, "2026-10-12T09:30:00.000Z"), 1))
    );
    assert_eq!(ledger.writes(), []);
    Ok(())
}

#[test]
fn an_absent_push_file_holds_no_record() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    assert_eq!(read_pushes(&ledger, id(42), id(7))?, []);
    Ok(())
}

/// Reading a push file holding `contents` is refused at `line` for `problem`.
fn pushes_refused(contents: &str, line: usize, problem: Corruption) {
    let ledger = MemoryLedger::default().with_file(&pushes(), contents.as_bytes());

    let read = read_pushes(&ledger, id(42), id(7));

    assert!(
        matches!(
            &read,
            Err(LedgerReadError::Corrupt { line: found, problem: actual, .. })
                if *found == line && *actual == problem
        ),
        "{contents:?}: {read:?}"
    );
}

#[test]
fn a_push_file_with_a_gap_in_its_sequence_is_refused() {
    let lines =
        push_line(1, 1, "2026-10-12T09:30:00.000Z") + &push_line(2, 3, "2026-10-12T09:31:00.000Z");
    pushes_refused(&lines, 2, Corruption::Sequence);
}

#[test]
fn a_push_file_not_starting_at_one_is_refused() {
    pushes_refused(
        &push_line(1, 2, "2026-10-12T09:30:00.000Z"),
        1,
        Corruption::Sequence,
    );
}

#[test]
fn a_push_file_with_a_run_twice_is_refused() {
    let lines =
        push_line(1, 1, "2026-10-12T09:30:00.000Z") + &push_line(1, 2, "2026-10-12T09:31:00.000Z");
    pushes_refused(&lines, 2, Corruption::Duplicate);
}

#[test]
fn a_push_record_of_another_pr_is_refused() {
    let line = push_line(1, 1, "2026-10-12T09:30:00.000Z").replacen(r#""pr":7"#, r#""pr":8"#, 1);
    pushes_refused(&line, 1, Corruption::WrongPullRequest);
}

#[test]
fn a_push_record_of_another_repository_is_refused() {
    let line = push_line(1, 1, "2026-10-12T09:30:00.000Z").replacen(
        r#""repo_id":42"#,
        r#""repo_id":43"#,
        1,
    );
    pushes_refused(&line, 1, Corruption::WrongPullRequest);
}

#[test]
fn a_push_line_with_an_escaped_character_is_refused() {
    let line = push_line(1, 1, "2026-10-12T09:30:00.000Z");
    let escaped = line.replacen("assay.ledger-push/1", r"assay.ledger-push\/1", 1);
    assert_ne!(escaped, line, "the schema id was escaped");
    pushes_refused(&escaped, 1, Corruption::NotCanonical);
}

#[test]
fn a_command_line_with_an_escaped_character_is_refused() {
    let line = command_line(500, "/assay recheck");
    let escaped = line.replacen("/assay recheck", r"\/assay recheck", 1);
    assert_ne!(escaped, line, "the command was escaped");
    let ledger = MemoryLedger::default().with_file(&commands(), escaped.as_bytes());

    let read = read_commands(&ledger, id(42), id(7));

    assert!(
        matches!(
            &read,
            Err(LedgerReadError::Corrupt {
                line: 1,
                problem: Corruption::NotCanonical,
                ..
            })
        ),
        "{read:?}"
    );
}

fn command(comment_id: u64, text: &str) -> CommandRecord {
    CommandRecord {
        run_uid: run(9),
        pr: id(7),
        comment_id: CommentId(id(comment_id)),
        actor_id: UserId(id(1001)),
        command: text.to_owned(),
        body_sha256: digest('c'),
        created_at: at("2026-10-12T09:29:58.000Z"),
        at: at("2026-10-12T09:30:00.000Z"),
    }
}

fn command_line(comment_id: u64, text: &str) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-command/1","run_uid":{{"repo_id":42,"run_id":9,"#,
            r#""run_attempt":1}},"pr":7,"comment_id":{comment_id},"actor_id":1001,"#,
            r#""command":"{text}","body_sha256":"sha256:{body}","#,
            r#""created_at":"2026-10-12T09:29:58.000Z","at":"2026-10-12T09:30:00.000Z"}}"#,
            "\n"
        ),
        comment_id = comment_id,
        text = text,
        body = "c".repeat(64),
    )
}

#[test]
fn a_command_record_is_appended_once_per_comment() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let first = append_command(&ledger, &command(500, "/assay recheck"))?;
    let again = append_command(&ledger, &command(500, "/assay override llm"))?;
    let second = append_command(&ledger, &command(501, "/assay retry bugs"))?;

    assert_eq!(first, Appended::Recorded);
    assert_eq!(
        again,
        Appended::AlreadyRecorded(command(500, "/assay recheck"))
    );
    assert_eq!(second, Appended::Recorded);
    assert_eq!(commands().as_str(), "commands/42/7.jsonl");
    assert_eq!(
        ledger.text(&commands()),
        command_line(500, "/assay recheck") + &command_line(501, "/assay retry bugs")
    );
    assert_eq!(
        read_commands(&ledger, id(42), id(7))?,
        [
            command(500, "/assay recheck"),
            command(501, "/assay retry bugs")
        ]
    );
    Ok(())
}

#[test]
fn a_command_file_with_a_comment_twice_is_refused() {
    let lines = command_line(500, "/assay recheck") + &command_line(500, "/assay recheck");
    let ledger = MemoryLedger::default().with_file(&commands(), lines.as_bytes());

    let read = read_commands(&ledger, id(42), id(7));

    assert!(
        matches!(
            &read,
            Err(LedgerReadError::Corrupt {
                line: 2,
                problem: Corruption::Duplicate,
                ..
            })
        ),
        "{read:?}"
    );
}

#[test]
fn a_command_with_a_control_character_is_never_written() {
    let ledger = MemoryLedger::default();

    let appended = append_command(&ledger, &command(500, "/assay recheck\u{1b}[2J"));

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Invalid {
                problem: Corruption::Field("command"),
                ..
            })
        ),
        "{appended:?}"
    );
    assert_eq!(ledger.writes(), []);
}

#[test]
fn a_command_over_its_length_cap_is_never_written() {
    let ledger = MemoryLedger::default();

    let appended = append_command(&ledger, &command(500, &"x".repeat(257)));

    assert!(
        matches!(
            &appended,
            Err(LedgerWriteError::Invalid {
                problem: Corruption::Field("command"),
                ..
            })
        ),
        "{appended:?}"
    );
}
