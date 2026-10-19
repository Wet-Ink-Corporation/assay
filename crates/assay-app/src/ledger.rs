//! The `Ledger` port and the ledger operations on top of it (`docs/architecture/06-operations.md`
//! §1, 02 §5, ADR-19, ADR-23).
//!
//! The port is the `assay-ledger` repository's files, read whole and written by compare-and-set
//! on the file's version. Everything else lives here, so every adapter shares one reading of the
//! records: their strict parse (ledger contents are untrusted, T23), deduplication by key, the
//! bounded retry of a conflicting write, and the 24-hour budget read.

mod aeu_text;
mod budget;
mod cas;
mod command;
mod cost;
mod error;
mod lines;
mod port;
mod push;
mod state;
mod time;

pub use budget::{BUDGET_WINDOW, COOLDOWN_WINDOW, CostWindow, LeaseReservation, read_costs};
pub use command::{CommandRecord, MAX_COMMAND_CHARS, append_command, read_commands};
pub use cost::{Appended, CostKind, CostRecord, CostRole, RoleCost, append_cost};
pub use error::{Corruption, LedgerReadError, LedgerWriteError};
pub use port::{
    FileVersion, Ledger, LedgerFile, LedgerPath, MAX_CAS_ATTEMPTS, MAX_LEDGER_FILE_BYTES,
    MAX_RECORD_BYTES, WriteCondition, WriteOutcome,
};
pub use push::{PushAppended, PushDraft, PushRecord, append_push, read_pushes};
pub use state::{
    ActorCounter, DecisionPointer, DeferralRecord, MAX_STATE_ENTRIES, PrState, StateUpdate,
    read_state, update_state,
};
pub use time::{LedgerMonth, LedgerTime, ParseLedgerTimeError};
