//! The engine value model (SPEC §7).
//!
//! Values are snapshot-scoped: RA handles and `FileId`s are live identities
//! inside one snapshot and **must not** escape it. Nothing here is
//! serializable; serialization happens only at the output boundary
//! ([`crate::projection`]).

use std::cmp::Ordering;
use std::sync::Arc;

use crate::def::Def;

/// A position within a file: zero-based line and UTF-8 column, the
/// `line-index` `LineCol` convention. Plain data (not snapshot-scoped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// A language-level enum value, e.g. `DefKind::FN` (SPEC §7). The names
/// are dynamic because the engine constructs tags for every language enum
/// (including render-layer ones this crate never sees); operator code uses
/// [`EnumTag::new`] with its static names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnumTag {
    pub ty: Arc<str>,
    pub variant: Arc<str>,
}

impl EnumTag {
    pub fn new(ty: &str, variant: &str) -> EnumTag {
        EnumTag { ty: Arc::from(ty), variant: Arc::from(variant) }
    }
}

/// One engine value (SPEC §7). `Clone` is cheap: RA handles are `Copy` IDs,
/// strings and tags are `Arc`s.
///
/// Total ordering is deliberately absent: ordering of RA handles is not
/// semantic. Deterministic output ordering happens after projection;
/// engine-internal ordering goes through `EngineValue::plain_cmp`, which
/// refuses handles.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// An RA-owned definition identity (SPEC §7 lists the variants flat;
    /// they are grouped under [`Def`] because the language's `Def` type is
    /// exactly this union).
    Def(Def),
    /// A resolved source span (snapshot-scoped file id).
    FileRange(ide_db::FileRange),
    /// A source file (snapshot-scoped).
    File(ide_db::FileId),
    /// A position within a file.
    Position(Position),
    String(Arc<str>),
    Int(i64),
    Bool(bool),
    Enum(EnumTag),
    /// A language `option<T>` value.
    Option(Option<Box<Value>>),
    /// A language `list<T>` value (engine-managed builtins only).
    List(Vec<Value>),
}

impl Value {
    pub fn string(s: impl Into<Arc<str>>) -> Value {
        Value::String(s.into())
    }
}

impl raql_plan::EngineValue for Value {
    fn int(value: i64) -> Value {
        Value::Int(value)
    }

    fn string(value: &str) -> Value {
        Value::String(Arc::from(value))
    }

    fn boolean(value: bool) -> Value {
        Value::Bool(value)
    }

    fn enum_tag(ty: &str, variant: &str) -> Value {
        Value::Enum(EnumTag::new(ty, variant))
    }

    fn none() -> Value {
        Value::Option(None)
    }

    fn some(inner: Value) -> Value {
        Value::Option(Some(Box::new(inner)))
    }

    fn list(items: Vec<Value>) -> Value {
        Value::List(items)
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(value) => Some(*value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn as_enum(&self) -> Option<(&str, &str)> {
        match self {
            Value::Enum(tag) => Some((&tag.ty, &tag.variant)),
            _ => None,
        }
    }

    fn as_option(&self) -> Option<Option<&Value>> {
        match self {
            Value::Option(inner) => Some(inner.as_deref()),
            _ => None,
        }
    }

    fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    fn plain_cmp(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            (Value::Enum(a), Value::Enum(b)) => {
                Some(a.ty.cmp(&b.ty).then_with(|| a.variant.cmp(&b.variant)))
            }
            (Value::Option(a), Value::Option(b)) => match (a, b) {
                (None, None) => Some(Ordering::Equal),
                (None, Some(_)) => Some(Ordering::Less),
                (Some(_), None) => Some(Ordering::Greater),
                (Some(a), Some(b)) => a.plain_cmp(b),
            },
            (Value::List(a), Value::List(b)) => {
                for (left, right) in a.iter().zip(b) {
                    match left.plain_cmp(right)? {
                        Ordering::Equal => continue,
                        other => return Some(other),
                    }
                }
                Some(a.len().cmp(&b.len()))
            }
            _ => None,
        }
    }

    fn type_tag(&self) -> String {
        match self {
            Value::Def(_) => "Def".to_string(),
            Value::FileRange(_) => "Span".to_string(),
            Value::File(_) => "File".to_string(),
            Value::Position(_) => "Position".to_string(),
            Value::String(_) => "string".to_string(),
            Value::Int(_) => "int".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Enum(tag) => tag.ty.to_string(),
            Value::Option(_) => "option<_>".to_string(),
            Value::List(_) => "list<_>".to_string(),
        }
    }
}
