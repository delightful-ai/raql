//! The compiler diagnostic type and span formatting helpers.

use core::fmt;

use camino::Utf8PathBuf;
use miette::{Diagnostic, Severity};
use raql_syntax::SrcSpan;
use thiserror::Error;

pub type DiagBundle = Vec<CompilerDiagnostic>;

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct CompilerDiagnostic {
    code: &'static str,
    pub(crate) message: String,
    severity: Severity,
    span: Option<SrcSpan>,
    pub(crate) help: Option<String>,
    include_stack: Box<[Utf8PathBuf]>,
}

impl CompilerDiagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Option<SrcSpan>) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Error,
            span,
            help: None,
            include_stack: Vec::new().into_boxed_slice(),
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn code_str(&self) -> &'static str {
        self.code
    }

    pub fn span(&self) -> Option<SrcSpan> {
        self.span
    }

    pub fn include_stack(&self) -> &[Utf8PathBuf] {
        &self.include_stack
    }

    fn attach_include_stack_from_sources(&mut self, sources: &raql_syntax::SourceMap) {
        if !self.include_stack.is_empty() {
            return;
        }
        let Some(span) = self.span else {
            return;
        };
        let Some(file) = sources.get(span.file) else {
            return;
        };
        if file.include_stack().is_empty() {
            return;
        }
        self.include_stack = file.include_stack().to_vec().into_boxed_slice();
    }
}

impl Diagnostic for CompilerDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let include_note = if self.include_stack.is_empty() {
            None
        } else {
            Some(format!(
                "include stack: {}",
                self.include_stack
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ))
        };
        match (self.help.as_deref(), include_note) {
            (None, None) => None,
            (Some(help), None) => Some(Box::new(help) as Box<dyn fmt::Display>),
            (None, Some(note)) => Some(Box::new(note) as Box<dyn fmt::Display>),
            (Some(help), Some(note)) => Some(Box::new(format!("{help}\n{note}"))),
        }
    }
}

pub(crate) fn enrich_include_stack_context(
    diagnostics: &mut [CompilerDiagnostic],
    sources: &raql_syntax::SourceMap,
) {
    for diagnostic in diagnostics {
        diagnostic.attach_include_stack_from_sources(sources);
    }
}

fn clamp_to_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut clamped = byte_offset.min(text.len());
    while clamped > 0 && !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

fn line_col_for_offset(text: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for ch in text[..byte_offset].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

pub(crate) fn format_span_brief(sources: &raql_syntax::SourceMap, span: SrcSpan) -> String {
    let Some(file) = sources.get(span.file) else {
        return format!("file-{}:?:?", span.file.0);
    };
    let start = clamp_to_char_boundary(file.text(), u32::from(span.range.start()) as usize);
    let end = clamp_to_char_boundary(file.text(), u32::from(span.range.end()) as usize);
    let (start_line, start_col) = line_col_for_offset(file.text(), start);
    let (end_line, end_col) = line_col_for_offset(file.text(), end);
    if start_line == end_line && start_col == end_col {
        format!("{}:{start_line}:{start_col}", file.path())
    } else {
        format!(
            "{}:{start_line}:{start_col}-{end_line}:{end_col}",
            file.path()
        )
    }
}

pub(crate) fn format_span_excerpt(sources: &raql_syntax::SourceMap, span: SrcSpan) -> String {
    let Some(file) = sources.get(span.file) else {
        return "<unknown>".to_string();
    };
    let start = clamp_to_char_boundary(file.text(), u32::from(span.range.start()) as usize);
    let end = clamp_to_char_boundary(file.text(), u32::from(span.range.end()) as usize);
    let Some(snippet) = file.text().get(start..end) else {
        return "<unknown>".to_string();
    };
    let normalized = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "<unknown>".to_string()
    } else {
        normalized
    }
}
