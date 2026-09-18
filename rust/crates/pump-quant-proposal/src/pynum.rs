//! `PyNum` — the Python number of unknown type, rendered the way Python renders it.
//!
//! WHY THIS EXISTS. The corpus was produced by Python and its numbers carry Python's
//! *type*, not just a value: a field computed by integer arithmetic prints `2380` while
//! the same field computed through a float prints `2380.0`, and the model saw those two
//! different tokens. Rust has no such unity — an `f64` is always an `f64` — so the type
//! has to be carried explicitly, or the live prompt silently tokenizes differently from
//! every training row of that shape.
//!
//! The producer is responsible for choosing the variant the corpus arithmetic would have
//! produced for the same inputs (integer lamport arithmetic stays `Int`; anything that
//! went through a float stays `Float`). `Bool` exists because the enrichment pipeline
//! carries Python bools (`creator_known=True`).

use crate::fmt::py_float;

/// A Python number, with the type Python would have given it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PyNum {
    /// A Python `int` — printed without a fractional part.
    Int(i64),
    /// A Python `float` — printed as `str(float)` (trailing `.0` kept).
    Float(f64),
    /// A Python `bool` — printed as `True` / `False`.
    Bool(bool),
}

impl PyNum {
    /// Render exactly as Python's `str()` would for the same value and type.
    pub fn render(self) -> String {
        match self {
            PyNum::Int(v) => v.to_string(),
            PyNum::Float(v) => py_float(v),
            PyNum::Bool(v) => if v { "True".to_string() } else { "False".to_string() },
        }
    }

    /// The numeric value as an `f64` (bools as 0.0 / 1.0).
    pub fn as_f64(self) -> f64 {
        match self {
            PyNum::Int(v) => v as f64,
            PyNum::Float(v) => v,
            PyNum::Bool(v) => if v { 1.0 } else { 0.0 },
        }
    }

    /// True when Python would print this number as a zero.
    pub fn is_zero(self) -> bool {
        self.as_f64() == 0.0
    }
}

impl From<i64> for PyNum {
    fn from(v: i64) -> Self {
        PyNum::Int(v)
    }
}

impl From<f64> for PyNum {
    fn from(v: f64) -> Self {
        PyNum::Float(v)
    }
}

impl From<bool> for PyNum {
    fn from(v: bool) -> Self {
        PyNum::Bool(v)
    }
}

/// Render an optional Python number: `None` is the literal `None`, as Python printed it.
pub fn opt(v: Option<PyNum>) -> String {
    match v {
        Some(x) => x.render(),
        None => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_by_python_type_not_just_value() {
        assert_eq!(PyNum::Int(2380).render(), "2380");
        assert_eq!(PyNum::Float(2380.0).render(), "2380.0");
        assert_eq!(PyNum::Bool(true).render(), "True");
        assert_eq!(PyNum::Bool(false).render(), "False");
        assert_eq!(opt(None), "None");
        assert_eq!(opt(Some(PyNum::Float(0.5))), "0.5");
    }
}
