//! Parses JSON into a [`Value`] and refuses any object that repeats a key.
//!
//! JSON leaves a repeated key's meaning to the parser (RFC 8259 §4): `serde_json` keeps the last
//! value, others keep the first. A document validated in one reading and acted on in another is
//! not validated, so a repeated key is refused instead of resolved.
//!
//! The visitor recurses once per nested array or object and sets no depth limit of its own:
//! `serde_json`'s recursion limit refuses a 128th level as a parse error before the visitor
//! sees it (pinned by `tests/it/read_limits.rs`).

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Parses `document` as one JSON value whose objects repeat no key.
pub(super) fn from_slice(document: &[u8]) -> Result<Value, serde_json::Error> {
    serde_json::from_slice::<Strict>(document).map(|Strict(value)| value)
}

/// A [`Value`] whose objects repeat no key.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Self)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value whose objects repeat no key")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    /// `serde_json` refuses an out-of-range number while parsing, so this error is a backstop.
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number is not finite"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    /// The error names no key: the document is untrusted, and so are its keys.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let Strict(value) = map.next_value()?;
            if object.insert(key, value).is_some() {
                return Err(de::Error::custom("an object repeats a key"));
            }
        }
        Ok(Value::Object(object))
    }
}
