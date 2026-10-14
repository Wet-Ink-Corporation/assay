//! The reviewer prompt bundle (`docs/architecture/04-pipeline.md` §3): one system prompt shared
//! by every role, so the prompt cache is reused, and one instruction per role, sent after the
//! context pack. The files live in `plugin/prompts/`, are versioned by directory and are
//! content-addressed: the bundle hash is the evidence key's `prompt_bundle_hash` (02 §3).

mod role;

use std::fmt;

use assay_domain::ContentHash;

use crate::model_runner::{InvalidPrompt, Prompt};

pub use role::Role;

/// One file of the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BundleFile {
    /// The system prompt every role shares, passed with `--append-system-prompt-file`.
    System,
    /// A role's instruction, sent after the context pack.
    Instruction(Role),
    /// The blind verifier's instruction, sent after the one claim it judges (04 §3, §4 step 8).
    /// The verifier is not a [`Role`]: it writes verdicts, never findings, so validation must
    /// not accept a findings document in its name.
    Verifier,
    /// The blind verifier's instruction for a finding carried from an earlier run, asking
    /// whether the defect is still present at head (04 §7).
    CarriedVerifier,
    /// The coordinator's instruction, sent after the findings it groups (04 §3, §4 step 7).
    /// Like the verifier it is not a [`Role`]: it answers with groups of finding ids, never
    /// findings of its own.
    Coordinator,
}

impl BundleFile {
    /// Every file, in the order the bundle hash covers them: the system prompt, each role's
    /// instruction in roster order, then the verifier's, the carried verifier's, then the
    /// coordinator's.
    pub const ALL: [Self; 10] = [
        Self::System,
        Self::Instruction(Role::Bugs),
        Self::Instruction(Role::Security),
        Self::Instruction(Role::Design),
        Self::Instruction(Role::Meta),
        Self::Instruction(Role::Combined),
        Self::Instruction(Role::CombinedMicro),
        Self::Verifier,
        Self::CarriedVerifier,
        Self::Coordinator,
    ];

    /// The file's path relative to `plugin/prompts/`, starting with the bundle version. The
    /// bundle hash covers it, so a file renamed or moved to another version changes the hash.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::System => "v1/system.md",
            Self::Instruction(Role::Bugs) => "v1/bugs.md",
            Self::Instruction(Role::Security) => "v1/security.md",
            Self::Instruction(Role::Design) => "v1/design.md",
            Self::Instruction(Role::Meta) => "v1/meta.md",
            Self::Instruction(Role::Combined) => "v1/combined.md",
            Self::Instruction(Role::CombinedMicro) => "v1/combined-micro.md",
            Self::Verifier => "v1/verifier.md",
            Self::CarriedVerifier => "v1/verifier-carried.md",
            Self::Coordinator => "v1/coordinator.md",
        }
    }
}

/// The text of every bundle file, as read by the caller from `plugin/prompts/` in the base
/// checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFiles {
    /// [`BundleFile::System`].
    pub system: String,
    /// The instruction of [`Role::Bugs`].
    pub bugs: String,
    /// The instruction of [`Role::Security`].
    pub security: String,
    /// The instruction of [`Role::Design`].
    pub design: String,
    /// The instruction of [`Role::Meta`].
    pub meta: String,
    /// The instruction of [`Role::Combined`].
    pub combined: String,
    /// The instruction of [`Role::CombinedMicro`].
    pub combined_micro: String,
    /// [`BundleFile::Verifier`].
    pub verifier: String,
    /// [`BundleFile::CarriedVerifier`].
    pub verifier_carried: String,
    /// [`BundleFile::Coordinator`].
    pub coordinator: String,
}

impl BundleFiles {
    fn text(&self, file: BundleFile) -> &str {
        match file {
            BundleFile::System => &self.system,
            BundleFile::Instruction(Role::Bugs) => &self.bugs,
            BundleFile::Instruction(Role::Security) => &self.security,
            BundleFile::Instruction(Role::Design) => &self.design,
            BundleFile::Instruction(Role::Meta) => &self.meta,
            BundleFile::Instruction(Role::Combined) => &self.combined,
            BundleFile::Instruction(Role::CombinedMicro) => &self.combined_micro,
            BundleFile::Verifier => &self.verifier,
            BundleFile::CarriedVerifier => &self.verifier_carried,
            BundleFile::Coordinator => &self.coordinator,
        }
    }
}

/// Why a bundle file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BundleFileError {
    /// The file is empty or only whitespace.
    #[error("it is empty")]
    Empty,
    /// The file holds a NUL byte, which no prompt needs and a text reader may truncate at.
    #[error("it holds a NUL byte")]
    Nul,
}

/// A refused bundle: which file, why, and the files handed back (ERR-14). `Debug` and
/// `Display` show only the file and the reason.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("prompt bundle file {} is refused: {reason}", .file.path())]
pub struct InvalidBundle {
    file: BundleFile,
    reason: BundleFileError,
    /// Boxed: nine texts would make every `Result` carrying the error that large.
    files: Box<BundleFiles>,
}

impl InvalidBundle {
    /// The first refused file, in [`BundleFile::ALL`] order.
    #[must_use]
    pub const fn file(&self) -> BundleFile {
        self.file
    }

    /// Why it was refused.
    #[must_use]
    pub const fn reason(&self) -> BundleFileError {
        self.reason
    }

    /// The files, as given.
    #[must_use]
    pub fn into_files(self) -> BundleFiles {
        *self.files
    }
}

impl fmt::Debug for InvalidBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvalidBundle")
            .field("file", &self.file)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

/// A checked prompt bundle and its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptBundle {
    files: BundleFiles,
    hash: ContentHash,
}

impl PromptBundle {
    /// Checks `files` and hashes them.
    ///
    /// # Errors
    /// Returns [`InvalidBundle`], holding the files, for the first file in [`BundleFile::ALL`]
    /// order that is empty ([`BundleFileError::Empty`]) or holds a NUL byte
    /// ([`BundleFileError::Nul`]).
    pub fn new(files: BundleFiles) -> Result<Self, InvalidBundle> {
        for file in BundleFile::ALL {
            let text = files.text(file);
            let reason = if text.trim().is_empty() {
                BundleFileError::Empty
            } else if text.contains('\0') {
                BundleFileError::Nul
            } else {
                continue;
            };
            return Err(InvalidBundle {
                file,
                reason,
                files: Box::new(files),
            });
        }
        let hash = ContentHash::of_files(
            BundleFile::ALL.map(|file| (file.path(), files.text(file).as_bytes())),
        );
        Ok(Self { files, hash })
    }

    /// The bundle hash: the [`ContentHash`] of every file in [`BundleFile::ALL`] order, each
    /// under its [`BundleFile::path`]. It changes when any byte of any file does.
    #[must_use]
    pub const fn hash(&self) -> ContentHash {
        self.hash
    }

    /// The system prompt, the same for every role.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        self.files.text(BundleFile::System)
    }

    /// The instruction of `role`.
    #[must_use]
    pub fn instruction(&self, role: Role) -> &str {
        self.files.text(BundleFile::Instruction(role))
    }

    /// The blind verifier's instruction, sent after the claim it judges (04 §4 step 8).
    #[must_use]
    pub fn verifier_instruction(&self) -> &str {
        self.files.text(BundleFile::Verifier)
    }

    /// The blind verifier's instruction for a carried finding, sent after its claim: is the
    /// defect still present at head (04 §7)?
    #[must_use]
    pub fn carried_verifier_instruction(&self) -> &str {
        self.files.text(BundleFile::CarriedVerifier)
    }

    /// The coordinator's instruction, sent after the findings it groups (04 §4 step 7).
    #[must_use]
    pub fn coordinator_instruction(&self) -> &str {
        self.files.text(BundleFile::Coordinator)
    }

    /// The user prompt of a call in `role`: the context pack first, then a blank line, then the
    /// role's instruction (04 §3).
    ///
    /// # Errors
    /// Returns [`InvalidPrompt`] when the composed text is not a valid [`Prompt`]: the context
    /// pack starts with `-` or holds a NUL byte.
    pub fn user_prompt(&self, role: Role, context_pack: &str) -> Result<Prompt, InvalidPrompt> {
        Prompt::new(format!("{context_pack}\n\n{}", self.instruction(role)))
    }
}
