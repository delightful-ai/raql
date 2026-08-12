//! The engine value model (SPEC §7).
//!
//! Values are snapshot-scoped: RA handles and `FileId`s are live identities
//! inside one snapshot and **must not** escape it. Nothing here is
//! serializable; serialization happens only at the output boundary
//! ([`crate::projection`]).

use std::sync::Arc;

use crate::def::Def;

/// A position within a file: zero-based line and UTF-8 column, the
/// `line-index` `LineCol` convention. Plain data (not snapshot-scoped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// A language-level enum value, e.g. `DefKind::FN` (SPEC §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EnumTag {
    pub ty: &'static str,
    pub variant: &'static str,
}

/// One engine value (SPEC §7). `Clone` is cheap: RA handles are `Copy` IDs.
///
/// Total ordering is deliberately absent: ordering of RA handles is not
/// semantic. Deterministic output ordering happens after projection.
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
}

impl Value {
    pub fn string(s: impl Into<Arc<str>>) -> Value {
        Value::String(s.into())
    }
}
