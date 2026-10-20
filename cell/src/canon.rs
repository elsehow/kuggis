//! Canonical bytes: compact JSON with fields in declaration order and sets
//! sorted. The same bytes are what gets hashed and what gets signed, on the
//! laptop and on the box, so a verifier elsewhere only needs to reproduce
//! this one encoding.

use serde::Serialize;

pub fn bytes<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("core types always serialize")
}

/// Display a value as its JSON, for errors and logs.
pub fn display<T: Serialize>(v: &T, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&serde_json::to_string(v).expect("core types always serialize"))
}
