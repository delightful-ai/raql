//! The engine-facing value contract (SPEC §5.1, §7).
//!
//! The engine executes joins, builtins, and aggregation over the host's
//! value type without knowing it: semantic identities (defs, spans) are
//! opaque — only equality and hashing — while plain data (ints, strings,
//! booleans, language enums, options, lists) must be constructible and
//! inspectable, because rule constants, string builtins, and aggregates
//! live on the engine's side of the boundary.
//!
//! Deliberately absent: any total order. Ordering of semantic handles is
//! not semantic (SPEC §7); [`EngineValue::plain_cmp`] orders plain data
//! only, and callers must treat `None` as "refuse to sort".

use std::cmp::Ordering;
use std::fmt::Debug;
use std::hash::Hash;

pub trait EngineValue: Clone + Eq + Hash + Debug {
    fn int(value: i64) -> Self;
    fn string(value: &str) -> Self;
    fn boolean(value: bool) -> Self;
    /// A language-enum value, e.g. `DefKind::FN` → `("DefKind", "FN")`.
    fn enum_tag(ty: &str, variant: &str) -> Self;
    fn none() -> Self;
    fn some(inner: Self) -> Self;
    fn list(items: Vec<Self>) -> Self;

    fn as_int(&self) -> Option<i64>;
    fn as_str(&self) -> Option<&str>;
    fn as_bool(&self) -> Option<bool>;
    fn as_enum(&self) -> Option<(&str, &str)>;
    /// `Some(None)` for the language `none`, `Some(Some(v))` for
    /// `some(v)`, `None` when the value is not an option.
    fn as_option(&self) -> Option<Option<&Self>>;
    fn as_list(&self) -> Option<&[Self]>;

    /// Total order over plain data; `None` when either side is a semantic
    /// handle. Options and lists compare structurally when their contents
    /// do.
    fn plain_cmp(&self, other: &Self) -> Option<Ordering>;

    /// Short type label for error messages (`"Def"`, `"int"`, ...).
    fn type_tag(&self) -> String;
}
