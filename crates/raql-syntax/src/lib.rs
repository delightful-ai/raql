//! RAQL syntax crate.

mod ast;
mod diag;
mod include_loader;
mod lexer;
mod parser;
mod source;

pub use ast::{
    AggregateBinder, AggregateName, ArithOp, ArithmeticBindConstraint, AstPhase, Atom,
    ChooseTopkBinder, Constraint, DeclArg, DeclAttr, Declaration, DeclarationKind, Directive,
    DisjunctionGroup, Expr, Fact, Goal, IncludeDirective, ModeArg, ModeDirection, ModeDirective,
    NotGoal, PragmaDirective, Program, RelOp, RelationalConstraint, Rule, Stmt, Term, TypeAst,
    TypeDirective,
};
pub use diag::{Diagnostic, DiagnosticKind, DiagnosticLabel, ParseResult};
pub use include_loader::{IncludeLoader, IncludeLoaderOptions, parse_program_from_file};
pub use source::{RaqlFileId, SourceFile, SourceMap, Spanned, SrcSpan};

use parser::parse_source;
use source::SourceMap as InternalSourceMap;

pub fn parse_program(text: &str) -> ParseResult<Program<AstPhase>> {
    parse_program_with_path(camino::Utf8PathBuf::from("<memory>"), text)
}

pub fn parse_program_with_path(
    path: camino::Utf8PathBuf,
    text: &str,
) -> ParseResult<Program<AstPhase>> {
    let mut sources = InternalSourceMap::new();
    let file_id = sources.add_file(path, text.to_string(), Vec::new().into_boxed_slice());
    let (phase, diagnostics) = parse_source(file_id, text, &[]);
    if diagnostics.is_empty() {
        Ok(Program::new(sources, phase))
    } else {
        Err(diagnostics)
    }
}
