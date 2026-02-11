use crate::ast::{AstPhase, Directive, Program, Stmt};
use crate::diag::{Diagnostic, DiagnosticKind, DiagnosticLabel, ParseResult};
use crate::parser::parse_source;
use crate::source::{SourceMap, Spanned, SrcSpan};
use camino::{Utf8Path, Utf8PathBuf};
use std::fs;

#[derive(Clone, Debug, Default)]
pub struct IncludeLoaderOptions {
    pub include_dirs: Vec<Utf8PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct IncludeLoader {
    options: IncludeLoaderOptions,
}

impl IncludeLoader {
    #[must_use]
    pub fn new(include_dirs: Vec<Utf8PathBuf>) -> Self {
        Self {
            options: IncludeLoaderOptions { include_dirs },
        }
    }

    #[must_use]
    pub fn with_options(options: IncludeLoaderOptions) -> Self {
        Self { options }
    }

    pub fn load_program(&self, entry_path: &Utf8Path) -> ParseResult<Program<AstPhase>> {
        let mut state = LoaderState::new(self.options.include_dirs.clone());
        let statements = state.load_file(entry_path.to_path_buf(), Vec::new(), Vec::new());

        if state.diagnostics.is_empty() {
            Ok(Program::new(state.sources, AstPhase { statements }))
        } else {
            Err(state.diagnostics)
        }
    }
}

#[must_use]
pub fn parse_program_from_file(
    entry_path: &Utf8Path,
    include_dirs: &[Utf8PathBuf],
) -> ParseResult<Program<AstPhase>> {
    IncludeLoader::new(include_dirs.to_vec()).load_program(entry_path)
}

struct LoaderState {
    include_dirs: Vec<Utf8PathBuf>,
    sources: SourceMap,
    diagnostics: Vec<Diagnostic>,
    visiting: Vec<Utf8PathBuf>,
}

#[derive(Clone, Debug)]
struct IncludeEdge {
    from_path: Utf8PathBuf,
    include_text: String,
    span: SrcSpan,
}

impl IncludeEdge {
    fn new(from_path: Utf8PathBuf, include_text: impl Into<String>, span: SrcSpan) -> Self {
        Self {
            from_path,
            include_text: include_text.into(),
            span,
        }
    }
}

impl LoaderState {
    fn new(include_dirs: Vec<Utf8PathBuf>) -> Self {
        Self {
            include_dirs,
            sources: SourceMap::new(),
            diagnostics: Vec::new(),
            visiting: Vec::new(),
        }
    }

    fn load_file(
        &mut self,
        requested_path: Utf8PathBuf,
        include_stack: Vec<Utf8PathBuf>,
        include_edges: Vec<IncludeEdge>,
    ) -> Vec<Spanned<Stmt>> {
        let normalized_path =
            normalize_existing_path(&requested_path).unwrap_or(requested_path.clone());
        let mut current_stack = include_stack.clone();
        current_stack.push(normalized_path.clone());

        if let Some(cycle_index) = self
            .visiting
            .iter()
            .position(|path| path == &normalized_path)
        {
            let mut cycle_paths = self.visiting[cycle_index..].to_vec();
            cycle_paths.push(normalized_path.clone());
            let cycle_text = cycle_paths
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            let mut diagnostic = Diagnostic::include_cycle(
                format!(
                    "include cycle detected while expanding `.include`: {cycle_text}; remove one `.include` edge or move shared declarations into a third file"
                ),
                None,
            )
            .with_include_stack(current_stack.clone())
            .with_note(
                "includes are expanded depth-first from the current file; this include re-entered a file already on the active stack",
            );
            diagnostic = self.annotate_include_legs(diagnostic, &include_edges, "cycle closes at");
            for path in cycle_paths {
                diagnostic = diagnostic.with_note(format!("cycle member: {path}"));
            }
            self.diagnostics.push(diagnostic);
            return Vec::new();
        }

        let text = match fs::read_to_string(normalized_path.as_std_path()) {
            Ok(text) => text,
            Err(err) => {
                let diagnostic = self.annotate_include_legs(
                    Diagnostic::io(format!("failed to read `{}`: {err}", normalized_path), None)
                        .with_include_stack(current_stack),
                    &include_edges,
                    "failed while following",
                );
                self.diagnostics.push(diagnostic);
                return Vec::new();
            }
        };

        self.visiting.push(normalized_path.clone());

        let file_id = self.sources.add_file(
            normalized_path.clone(),
            text.clone(),
            current_stack.clone().into_boxed_slice(),
        );
        let (phase, parse_diagnostics) = parse_source(file_id, &text, &current_stack);
        let parse_diagnostics = parse_diagnostics
            .into_iter()
            .map(|diag| self.annotate_parse_include_context(diag, &include_edges))
            .collect::<Vec<_>>();
        self.diagnostics.extend(parse_diagnostics);

        let mut statements = Vec::new();
        for stmt in phase.statements {
            let include_spec = match &stmt.value {
                Stmt::Directive(Directive::Include(include_directive)) => Some((
                    include_directive.path.value.clone(),
                    include_directive.path.span,
                )),
                _ => None,
            };

            statements.push(stmt);

            let Some((include_text, include_span)) = include_spec else {
                continue;
            };

            let (resolved, attempted) =
                self.resolve_include_path(&normalized_path, include_text.as_str());
            let mut child_stack = include_stack.clone();
            child_stack.push(normalized_path.clone());
            let include_edge =
                IncludeEdge::new(normalized_path.clone(), include_text.as_str(), include_span);

            let Some(resolved_path) = resolved else {
                let mut edge_chain = include_edges.clone();
                edge_chain.push(include_edge.clone());

                let mut diagnostic = Diagnostic::missing_include(
                    format!(
                        "unable to resolve include `{include_text}`; searched include locations in order and found no readable file"
                    ),
                    None,
                )
                .with_include_stack(child_stack)
                .with_note(
                    "search order: absolute paths are checked as written; relative paths are checked in the including file directory, then each configured include directory",
                )
                .with_note(
                    "corrective action: fix the include path, place the file in one of the searched directories, or add an include directory to IncludeLoader options",
                );
                diagnostic =
                    self.annotate_include_legs(diagnostic, &edge_chain, "missing target at");

                for candidate in attempted {
                    diagnostic = diagnostic.with_note(format!("tried candidate `{candidate}`"));
                }
                self.diagnostics.push(diagnostic);
                continue;
            };

            let mut child_edges = include_edges.clone();
            child_edges.push(include_edge);

            let mut nested = self.load_file(resolved_path, child_stack, child_edges);
            statements.append(&mut nested);
        }

        self.visiting.pop();
        statements
    }

    fn resolve_include_path(
        &self,
        current_file: &Utf8Path,
        include_text: &str,
    ) -> (Option<Utf8PathBuf>, Vec<Utf8PathBuf>) {
        let include_path = Utf8Path::new(include_text);
        let mut attempted = Vec::new();

        if include_path.is_absolute() {
            let candidate = include_path.to_path_buf();
            attempted.push(candidate.clone());
            if candidate.is_file() {
                return (
                    Some(normalize_existing_path(&candidate).unwrap_or(candidate)),
                    attempted,
                );
            }
            return (None, attempted);
        }

        if let Some(parent) = current_file.parent() {
            let candidate = parent.join(include_path);
            attempted.push(candidate.clone());
            if candidate.is_file() {
                return (
                    Some(normalize_existing_path(&candidate).unwrap_or(candidate)),
                    attempted,
                );
            }
        }

        for include_dir in &self.include_dirs {
            let candidate = include_dir.join(include_path);
            attempted.push(candidate.clone());
            if candidate.is_file() {
                return (
                    Some(normalize_existing_path(&candidate).unwrap_or(candidate)),
                    attempted,
                );
            }
        }

        (None, attempted)
    }

    fn annotate_include_legs(
        &self,
        mut diagnostic: Diagnostic,
        include_edges: &[IncludeEdge],
        terminal_prefix: &str,
    ) -> Diagnostic {
        if include_edges.is_empty() {
            return diagnostic;
        }

        let total = include_edges.len();
        for (index, edge) in include_edges.iter().enumerate() {
            let label_text = Self::include_leg_text(index, total, edge);
            if index + 1 == total {
                diagnostic.primary = Some(DiagnosticLabel::new(
                    edge.span,
                    format!("{terminal_prefix} {label_text}"),
                ));
            } else {
                diagnostic = diagnostic.with_secondary(DiagnosticLabel::new(edge.span, label_text));
            }
        }

        diagnostic
    }

    fn annotate_parse_include_context(
        &self,
        mut diagnostic: Diagnostic,
        include_edges: &[IncludeEdge],
    ) -> Diagnostic {
        if diagnostic.kind != DiagnosticKind::Parse || include_edges.is_empty() {
            return diagnostic;
        }

        let total = include_edges.len();
        for (index, edge) in include_edges.iter().enumerate() {
            diagnostic = diagnostic.with_secondary(DiagnosticLabel::new(
                edge.span,
                format!(
                    "included via {}",
                    Self::include_leg_text(index, total, edge)
                ),
            ));
        }

        if let Some(edge) = include_edges.last() {
            diagnostic = diagnostic.with_note(format!(
                "this file was pulled in by `.include \"{}\".` in `{}`",
                edge.include_text, edge.from_path
            ));
        }

        diagnostic
    }

    fn include_leg_text(index: usize, total: usize, edge: &IncludeEdge) -> String {
        format!(
            "include leg {}/{}: `{}` includes `{}`",
            index + 1,
            total,
            edge.from_path,
            edge.include_text
        )
    }
}

fn normalize_existing_path(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let canonical = fs::canonicalize(path.as_std_path()).ok()?;
    Utf8PathBuf::from_path_buf(canonical).ok()
}
