//! Typed argument schema of extern predicates (SPEC §8.6).

/// Argument types of extern predicates. These are catalog-level type names;
/// the host's value representation is its own concern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgType {
    /// An RA-owned definition identity.
    Def,
    /// A resolved source span (file + range).
    Span,
    /// A source file.
    File,
    /// A position within a file (zero-based line/column, `LineIndex`
    /// convention).
    Position,
    /// Plain string data (names, canonical paths, handles).
    String,
    /// A language-level enum, named by its type (e.g. `DefKind`).
    Enum(&'static str),
    /// Plain integer data.
    Int,
}

impl ArgType {
    pub fn name(self) -> &'static str {
        match self {
            ArgType::Def => "Def",
            ArgType::Span => "Span",
            ArgType::File => "File",
            ArgType::Position => "Position",
            ArgType::String => "String",
            ArgType::Enum(name) => name,
            ArgType::Int => "Int",
        }
    }
}

/// One argument of a predicate: name and type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArgDef {
    pub name: &'static str,
    pub ty: ArgType,
}
