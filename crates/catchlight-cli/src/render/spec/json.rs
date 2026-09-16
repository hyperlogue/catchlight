//! Bounded parsing that preserves duplicate-key errors at every nesting level.

use super::{bad, check_limit, MAX_JSON_BYTES};
use crate::Error;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

pub fn read_json(path: &Path) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut bytes = Vec::new();
    file.take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    check_limit(
        "spec_bytes",
        bytes.len() as u64,
        MAX_JSON_BYTES,
        "split the spec",
    )?;
    Ok(bytes)
}

pub fn decode_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    check_limit(
        "spec_bytes",
        bytes.len() as u64,
        MAX_JSON_BYTES,
        "split the spec",
    )?;
    let value: Unique =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    serde_json::from_value(value.0).map_err(|e| bad(format!("invalid render spec: {e}")))
}
struct Unique(serde_json::Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(n.into()))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(serde_json::Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element::<Unique>()? {
                    values.push(v.0);
                }
                Ok(Unique(values.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, v)) = a.next_entry::<String, Unique>()? {
                    if values.insert(key.clone(), v.0).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                    }
                }
                Ok(Unique(values.into()))
            }
        }
        d.deserialize_any(Visitor)
    }
}
