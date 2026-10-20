//! Each flag of the 03 §8 argv, with the profile checks that show it (A-10, #84).

use std::fmt;

use super::ProfileCheck;

/// One flag of the production argv (03 §8), in argv order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessFlag {
    /// `-p`, with its prompt on stdin (wi-9fe37e).
    Print,
    /// `--output-format stream-json`.
    OutputFormat,
    /// `--verbose`.
    Verbose,
    /// `--json-schema`.
    JsonSchema,
    /// `--append-system-prompt-file`.
    AppendSystemPromptFile,
    /// `--exclude-dynamic-system-prompt-sections`.
    ExcludeDynamicSections,
    /// `--restricted`.
    Restricted,
    /// `--safe-mode`.
    SafeMode,
    /// `--settings` (hooks off, reads denied, `blockReadsOutsideWorkingDirectories`).
    Settings,
    /// `--tools Read,Grep,Glob`.
    Tools,
    /// `--add-dir`.
    AddDir,
    /// `--permission-mode dontAsk`.
    PermissionMode,
    /// `--max-turns`.
    MaxTurns,
    /// `--max-budget-usd`.
    MaxBudgetUsd,
    /// `--include-hook-events`, which the profile canary adds.
    IncludeHookEvents,
}

impl HeadlessFlag {
    /// Every flag, in argv order.
    pub const ALL: [Self; 15] = [
        Self::Print,
        Self::OutputFormat,
        Self::Verbose,
        Self::JsonSchema,
        Self::AppendSystemPromptFile,
        Self::ExcludeDynamicSections,
        Self::Restricted,
        Self::SafeMode,
        Self::Settings,
        Self::Tools,
        Self::AddDir,
        Self::PermissionMode,
        Self::MaxTurns,
        Self::MaxBudgetUsd,
        Self::IncludeHookEvents,
    ];

    /// The checks whose pass the flag needs.
    #[must_use]
    pub const fn checks(self) -> &'static [ProfileCheck] {
        use ProfileCheck as C;
        match self {
            Self::Print => &[C::StdinPrompt],
            Self::OutputFormat | Self::Verbose => &[C::ProfileAccepted],
            Self::JsonSchema => &[C::StdinPrompt, C::SystemPromptFile, C::AddDir],
            Self::AppendSystemPromptFile => &[C::SystemPromptFile],
            Self::Restricted => &[C::ProfileAccepted, C::Preflight, C::OutsideRead],
            Self::SafeMode => &[C::ProfileAccepted, C::HookCanary],
            Self::Settings => &[C::FenceCanary, C::OutsideRead, C::HookCanary],
            Self::Tools => &[C::Preflight],
            Self::AddDir => &[C::AddDir],
            Self::PermissionMode => &[C::PermissionMode],
            Self::ExcludeDynamicSections | Self::MaxTurns | Self::MaxBudgetUsd => {
                &[C::ProfileAccepted]
            }
            Self::IncludeHookEvents => &[C::HookCanary],
        }
    }

    /// Whether the checks show the flag's effect, not only that the CLI accepted it. The
    /// prompt-cache sections, the turn and budget limits and the hook events of a session in
    /// which no hook may run are not exercised by one short call.
    #[must_use]
    pub const fn exercised(self) -> bool {
        !matches!(
            self,
            Self::ExcludeDynamicSections
                | Self::MaxTurns
                | Self::MaxBudgetUsd
                | Self::IncludeHookEvents
        )
    }
}

impl fmt::Display for HeadlessFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Print => "-p (prompt on stdin)",
            Self::OutputFormat => "--output-format stream-json",
            Self::Verbose => "--verbose",
            Self::JsonSchema => "--json-schema",
            Self::AppendSystemPromptFile => "--append-system-prompt-file",
            Self::ExcludeDynamicSections => "--exclude-dynamic-system-prompt-sections",
            Self::Restricted => "--restricted",
            Self::SafeMode => "--safe-mode",
            Self::Settings => "--settings",
            Self::Tools => "--tools Read,Grep,Glob",
            Self::AddDir => "--add-dir",
            Self::PermissionMode => "--permission-mode dontAsk",
            Self::MaxTurns => "--max-turns",
            Self::MaxBudgetUsd => "--max-budget-usd",
            Self::IncludeHookEvents => "--include-hook-events",
        })
    }
}

/// A flag and, when one of its checks failed (or is missing), the first such check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagResult {
    /// The flag.
    pub flag: HeadlessFlag,
    /// Its first check that did not pass; `None` when all passed.
    pub failed: Option<ProfileCheck>,
}

impl fmt::Display for FlagResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.flag)?;
        if let Some(check) = self.failed {
            return write!(f, "{check} failed");
        }
        let exercised = self.flag.exercised();
        f.write_str(if exercised { "shown by " } else { "accepted (" })?;
        for (index, check) in self.flag.checks().iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{check}")?;
        }
        if !exercised {
            f.write_str("), its effect is not exercised")?;
        }
        Ok(())
    }
}
