//! The built-in adapters: the one table of their `adapter` values and the plugin names reserved
//! for them, so the parser cannot accept a built-in whose name is not reserved.

use super::error::ConfigError;

/// A built-in adapter. Its plugin name is reserved: a plugin may take it only when it is that
/// built-in, so a rubric rule keyed by the name never reads another adapter's findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Builtin {
    Temper,
    WeighIn,
}

impl Builtin {
    const ALL: [Self; 2] = [Self::Temper, Self::WeighIn];

    /// The built-in whose `adapter` value is `adapter`, or `None` when it is not a built-in's.
    pub(super) fn from_adapter(adapter: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|builtin| builtin.adapter() == adapter)
    }

    /// Refuses plugin `gate` when its name is a built-in's and `adapter` is not that built-in.
    /// Checked on the adapter as written, before it is parsed, so an unknown adapter under a
    /// reserved name is refused for the name.
    pub(super) fn check_name(gate: &str, adapter: &str) -> Result<(), ConfigError> {
        match Self::ALL.into_iter().find(|builtin| builtin.name() == gate) {
            // The error owns its copies; the entry is dropped with the refusal.
            Some(builtin) if adapter != builtin.adapter() => Err(ConfigError::ReservedName {
                gate: gate.to_owned(),
                adapter: adapter.to_owned(),
                reserved_for: builtin.adapter(),
            }),
            Some(_) | None => Ok(()),
        }
    }

    /// The plugin name reserved for it.
    const fn name(self) -> &'static str {
        match self {
            Self::Temper => "temper",
            Self::WeighIn => "weigh-in",
        }
    }

    /// Its `adapter` value in plugins.toml.
    const fn adapter(self) -> &'static str {
        match self {
            Self::Temper => "builtin:temper",
            Self::WeighIn => "builtin:weigh-in",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Builtin, ConfigError};

    /// Its position in `Builtin::ALL`. The match is exhaustive, so a new built-in does not
    /// compile until it is placed here, and `all_lists_every_builtin` then needs it in `ALL`.
    const fn position(builtin: Builtin) -> usize {
        match builtin {
            Builtin::Temper => 0,
            Builtin::WeighIn => 1,
        }
    }

    #[test]
    fn all_lists_every_builtin() {
        for (index, builtin) in Builtin::ALL.into_iter().enumerate() {
            assert_eq!(position(builtin), index, "{builtin:?}");
        }
    }

    #[test]
    fn every_builtin_the_parser_accepts_has_its_name_reserved() {
        for builtin in Builtin::ALL {
            assert_eq!(Builtin::from_adapter(builtin.adapter()), Some(builtin));
            assert!(
                matches!(
                    Builtin::check_name(builtin.name(), builtin.adapter()),
                    Ok(())
                ),
                "{builtin:?}"
            );
            assert!(
                matches!(
                    Builtin::check_name(builtin.name(), "exec"),
                    Err(ConfigError::ReservedName { reserved_for, .. })
                        if reserved_for == builtin.adapter()
                ),
                "{builtin:?}"
            );
        }
    }

    #[test]
    fn an_adapter_that_is_not_a_builtin_is_none() {
        for adapter in ["exec", "builtin:other", "builtin:Temper", "temper", ""] {
            assert_eq!(Builtin::from_adapter(adapter), None, "{adapter:?}");
        }
    }
}
