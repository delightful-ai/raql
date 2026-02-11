use core::fmt;

/// Internable symbol used across IR phases.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(Box<str>);

impl Symbol {
    /// Creates a new symbol.
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the symbol text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Symbol {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Symbol {
    fn from(value: String) -> Self {
        Self::new(value.into_boxed_str())
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Symbol").field(&self.0).finish()
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Scalar literal values used in AST, typed terms, and plans.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScalarValue {
    /// Null sentinel.
    Null,
    /// Boolean scalar.
    Bool(bool),
    /// Signed 64-bit integer.
    I64(i64),
    /// UTF-8 string scalar.
    String(Box<str>),
}

impl From<&str> for ScalarValue {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}

impl From<String> for ScalarValue {
    fn from(value: String) -> Self {
        Self::String(value.into_boxed_str())
    }
}
