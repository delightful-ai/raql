use crate::source::SrcSpan;
use camino::Utf8PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    Lex,
    Parse,
    IncludeCycle,
    MissingInclude,
    Io,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticLabel {
    pub span: SrcSpan,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub kind: DiagnosticKind,
    pub message: String,
    pub primary: Option<DiagnosticLabel>,
    pub secondary: Vec<DiagnosticLabel>,
    pub notes: Vec<String>,
    pub include_stack: Vec<Utf8PathBuf>,
}

pub type ParseResult<T> = Result<T, Vec<Diagnostic>>;

impl Diagnostic {
    #[must_use]
    pub fn lex(message: impl Into<String>, primary: Option<DiagnosticLabel>) -> Self {
        Self {
            code: "RAQL0001",
            kind: DiagnosticKind::Lex,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            include_stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn parse(message: impl Into<String>, primary: Option<DiagnosticLabel>) -> Self {
        Self {
            code: "RAQL0002",
            kind: DiagnosticKind::Parse,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            include_stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn include_cycle(message: impl Into<String>, primary: Option<DiagnosticLabel>) -> Self {
        Self {
            code: "RAQL0003",
            kind: DiagnosticKind::IncludeCycle,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            include_stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn missing_include(message: impl Into<String>, primary: Option<DiagnosticLabel>) -> Self {
        Self {
            code: "RAQL0004",
            kind: DiagnosticKind::MissingInclude,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            include_stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn io(message: impl Into<String>, primary: Option<DiagnosticLabel>) -> Self {
        Self {
            code: "RAQL0005",
            kind: DiagnosticKind::Io,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            include_stack: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    #[must_use]
    pub fn with_secondary(mut self, label: DiagnosticLabel) -> Self {
        self.secondary.push(label);
        self
    }

    #[must_use]
    pub fn with_include_stack(mut self, stack: Vec<Utf8PathBuf>) -> Self {
        self.include_stack = stack;
        self
    }
}

impl DiagnosticLabel {
    #[must_use]
    pub fn new(span: SrcSpan, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}
