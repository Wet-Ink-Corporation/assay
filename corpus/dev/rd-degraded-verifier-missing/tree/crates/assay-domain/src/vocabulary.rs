//! `closed_enum!`: one list of variants and wire codes generates a closed vocabulary of 03, so
//! its serde form, its `code()` and its `ALL` cannot drift apart.

/// Defines a fieldless enum whose variants serialise as the given wire codes.
///
/// The enum also gets an `ALL` array of every variant in declaration order and a `code()`
/// returning the wire code.
///
/// Docs and extra derives before `enum`, and docs and `#[default]` on a variant, are kept;
/// `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, `Serialize` and `Deserialize` are
/// always derived. Only those attributes are accepted, and each is written out by name: an
/// attribute forwarded whole (`#[$meta]`) could carry a lint suppression no source scan reads
/// (03 §2.1 step 2, the temper adapter's `blind-spot`).
macro_rules! closed_enum {
    (
        $(#[doc = $doc:expr])*
        $(#[derive $($derive:tt)*])*
        pub enum $name:ident {
            $(
                $(#[doc = $variant_doc:expr])*
                $(#[default $($default:tt)*])?
                $variant:ident => $code:literal,
            )+
        }
    ) => {
        $(#[doc = $doc])*
        $(#[derive $($derive)*])*
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
                $(#[doc = $variant_doc])*
                $(#[default $($default)*])?
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
