//! `closed_enum!`: one list of variants and wire codes generates a closed vocabulary of 03, so
//! its serde form, its `code()` and its `ALL` cannot drift apart.

/// Defines a fieldless enum whose variants serialise as the given wire codes.
///
/// The enum also gets an `ALL` array of every variant in declaration order and a `code()`
/// returning the wire code.
///
/// Attributes before `enum` (docs, extra derives) are kept; `Debug`, `Clone`, `Copy`,
/// `PartialEq`, `Eq`, `Hash`, `Serialize` and `Deserialize` are always derived.
macro_rules! closed_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $code:literal,
            )+
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $code)]
                $variant,
            )+
        }

        impl $name {
            /// Every variant, in the order the specification lists them.
            pub const ALL: [Self; [$($code),+].len()] = [$(Self::$variant),+];

            /// The wire code of this value, as it appears in the contract documents.
            #[must_use]
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)+
                }
            }
        }
    };
}

pub(crate) use closed_enum;
