use core::fmt;

use miette::{Diagnostic, Severity};
use thiserror::Error;

/// Stable diagnostic categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCategory {
    /// Parser and syntax diagnostics.
    Parse,
    /// Name resolution diagnostics.
    Resolve,
    /// Type-checking diagnostics.
    Type,
    /// Mode planning diagnostics.
    Mode,
    /// Stratification and safety diagnostics.
    Stratification,
    /// Runtime boundary diagnostics.
    Runtime,
    /// Internal invariant diagnostics.
    Internal,
}

impl DiagnosticCategory {
    /// Stable base code for the category (RAQLxxxx).
    pub const fn base(self) -> u16 {
        match self {
            Self::Parse => 0,
            Self::Resolve => 100,
            Self::Type => 200,
            Self::Mode => 300,
            Self::Stratification => 400,
            Self::Runtime => 900,
            Self::Internal => 990,
        }
    }
}

/// Stable, human-readable code for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagnosticCode {
    category: DiagnosticCategory,
    index: u16,
}

impl DiagnosticCode {
    /// Creates a new stable code.
    pub const fn new(category: DiagnosticCategory, index: u16) -> Self {
        Self { category, index }
    }

    /// Returns the category.
    pub const fn category(self) -> DiagnosticCategory {
        self.category
    }

    /// Returns the category-local numeric index.
    pub const fn index(self) -> u16 {
        self.index
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.category.base().saturating_add(self.index);
        write!(f, "RAQL{code:04}")
    }
}

/// Canonical stable code constants used by early IR phases.
pub mod codes {
    use crate::diag::{DiagnosticCategory, DiagnosticCode};

    /// Unexpected or unsupported token in parsing.
    pub const PARSE_UNEXPECTED_TOKEN: DiagnosticCode =
        DiagnosticCode::new(DiagnosticCategory::Parse, 1);
    /// Type mismatch during checking.
    pub const TYPE_MISMATCH: DiagnosticCode = DiagnosticCode::new(DiagnosticCategory::Type, 1);
    /// Planner could not lower an operation.
    pub const PLAN_UNSUPPORTED_OP: DiagnosticCode =
        DiagnosticCode::new(DiagnosticCategory::Mode, 1);
    /// Host could not provide expected scalar.
    pub const HOST_MISSING_SCALAR_INPUT: DiagnosticCode =
        DiagnosticCode::new(DiagnosticCategory::Runtime, 1);
    /// Internal invariant failure.
    pub const INTERNAL_INVARIANT: DiagnosticCode =
        DiagnosticCode::new(DiagnosticCategory::Internal, 1);
}

/// RAQL diagnostic primitive implementing `miette::Diagnostic`.
#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct RaqlDiagnostic {
    code: DiagnosticCode,
    message: String,
    severity: Severity,
    help: Option<String>,
}

impl RaqlDiagnostic {
    /// Creates an error-severity diagnostic.
    pub fn error(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Error,
            help: None,
        }
    }

    /// Creates a warning-severity diagnostic.
    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Warning,
            help: None,
        }
    }

    /// Attaches an optional help message.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Returns this diagnostic's stable code.
    pub fn stable_code(&self) -> DiagnosticCode {
        self.code
    }

    /// Returns this diagnostic's textual message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns this diagnostic's severity.
    pub fn severity_level(&self) -> Severity {
        self.severity
    }
}

impl Diagnostic for RaqlDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help
            .as_deref()
            .map(|help| Box::new(help) as Box<dyn fmt::Display + 'a>)
    }
}

/// Mutable diagnostics collection.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Vec<RaqlDiagnostic>,
}

impl Diagnostics {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a diagnostic.
    pub fn push(&mut self, diagnostic: RaqlDiagnostic) {
        self.items.push(diagnostic);
    }

    /// Extends with diagnostics from an iterator.
    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = RaqlDiagnostic>) {
        self.items.extend(diagnostics);
    }

    /// Returns immutable iteration over diagnostics.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &RaqlDiagnostic> + DoubleEndedIterator {
        self.items.iter()
    }

    /// Returns true when empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns diagnostic count.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Converts into the underlying vector.
    pub fn into_vec(self) -> Vec<RaqlDiagnostic> {
        self.items
    }
}

impl IntoIterator for Diagnostics {
    type IntoIter = std::vec::IntoIter<RaqlDiagnostic>;
    type Item = RaqlDiagnostic;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use miette::Diagnostic;

    use crate::diag::{DiagnosticCategory, DiagnosticCode, Diagnostics, RaqlDiagnostic, codes};

    #[test]
    fn code_format_is_stable() {
        let code = DiagnosticCode::new(DiagnosticCategory::Mode, 42);
        assert_eq!(code.to_string(), "RAQL0342");
    }

    #[test]
    fn miette_code_matches_stable_code() {
        let diag = RaqlDiagnostic::error(codes::TYPE_MISMATCH, "type mismatch");
        let code = Diagnostic::code(&diag).expect("expected code").to_string();
        assert_eq!(code, "RAQL0201");
    }

    #[test]
    fn diagnostics_collection_round_trip() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.push(RaqlDiagnostic::warning(
            codes::HOST_MISSING_SCALAR_INPUT,
            "missing scalar input",
        ));

        assert_eq!(diagnostics.len(), 1);
        let collected: Vec<_> = diagnostics.into_iter().collect();
        assert_eq!(collected.len(), 1);
    }
}
