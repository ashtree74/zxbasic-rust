//! Runtime values in our BASIC implementation.
//!
//! A `Value` is either a 64-bit float ("numeric") or an owned byte string.
//! Spectrum BASIC has just these two scalar types; arrays of either kind
//! arrive in MVP-5. Strings are byte strings (not UTF-8 strings) because the
//! Spectrum character set includes block-graphic codepoints in $80..$8F and
//! tokens above $A4 — we mirror that by treating strings as raw bytes.

use crate::expression::EvalError;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Str(Vec<u8>),
}

impl Value {
    pub fn as_num(&self) -> Result<f64, EvalError> {
        match self {
            Value::Num(n) => Ok(*n),
            Value::Str(_) => Err(EvalError::TypeMismatch),
        }
    }

    pub fn as_str(&self) -> Result<&[u8], EvalError> {
        match self {
            Value::Str(s) => Ok(s),
            Value::Num(_) => Err(EvalError::TypeMismatch),
        }
    }

    pub fn is_num(&self) -> bool {
        matches!(self, Value::Num(_))
    }

    pub fn is_str(&self) -> bool {
        matches!(self, Value::Str(_))
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Num(n)
    }
}

impl From<Vec<u8>> for Value {
    fn from(s: Vec<u8>) -> Self {
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.as_bytes().to_vec())
    }
}

/// Whether a variable name (e.g. `A$`, `B`) is string-typed by the Spectrum
/// suffix rule.
pub fn is_string_name(name: &str) -> bool {
    name.ends_with('$')
}
