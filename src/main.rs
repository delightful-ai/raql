use std::path::PathBuf;
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand};
use miette::Diagnostic;
use raql_compiler::{CompilerDiagnostic, PlannedProgram, plan, resolve, typecheck};
use raql_engine::{EvalResult, EvalStatus, HostValueKind, RuntimeValue, execute};
use raql_host::MockHostRuntime;
use raql_host_ra::RaHostRuntime;
use raql_syntax::{DiagnosticLabel, SrcSpan, parse_program_from_file};

#[derive(Debug, Parser)]
#[command(name = "raql")]
#[command(about = "RAQL command line interface")]
struct Cli {
    #[command(subcommand)]
    command: TopLevelCommand,
}

#[derive(Debug, Subcommand)]
enum TopLevelCommand {
    /// Language commands.
    #[command(subcommand)]
    Lang(LangCommand),
}

#[derive(Debug, Subcommand)]
enum LangCommand {
    /// Parse, resolve, typecheck, and plan a RAQL program.
    Check(LangCheckArgs),
    /// Compile and execute a RAQL program.
    Run(LangRunArgs),
}

#[derive(Debug, Args)]
struct LangCommonArgs {
    /// Path to a RAQL program file.
    program: PathBuf,
    /// Additional include directories used to resolve `.include "..."`.
    #[arg(long = "include-dir", short = 'I')]
    include_dirs: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct LangCheckArgs {
    #[command(flatten)]
    common: LangCommonArgs,
}

#[derive(Debug, Args)]
struct LangRunArgs {
    #[command(flatten)]
    common: LangCommonArgs,
    /// Optional Rust workspace root or Cargo manifest used to initialize the RA host runtime.
    #[arg(long = "rust-file")]
    rust_file: Option<PathBuf>,
    /// Restrict output to one or more relation names.
    #[arg(long = "relation")]
    relation_filters: Vec<String>,
    /// Show empty relations in output.
    #[arg(long = "show-empty", default_value_t = false)]
    show_empty: bool,
    /// Maximum rows printed per relation.
    #[arg(long = "max-rows", default_value_t = 50)]
    max_rows: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        TopLevelCommand::Lang(cmd) => run_lang(cmd),
    }
}

fn run_lang(cmd: LangCommand) -> Result<(), String> {
    match cmd {
        LangCommand::Check(args) => run_lang_check(args),
        LangCommand::Run(args) => run_lang_run(args),
    }
}

fn run_lang_check(args: LangCheckArgs) -> Result<(), String> {
    let (program_path, include_dirs) = decode_common_args(&args.common)?;
    let planned = load_and_plan(&program_path, &include_dirs)?;
    print_plan_summary(&program_path, &planned);
    Ok(())
}

fn run_lang_run(args: LangRunArgs) -> Result<(), String> {
    let (program_path, include_dirs) = decode_common_args(&args.common)?;
    let planned = load_and_plan(&program_path, &include_dirs)?;
    print_plan_summary(&program_path, &planned);

    let result = if let Some(rust_file) = args.rust_file {
        let runtime_path = into_utf8_pathbuf(rust_file, "--rust-file")?;
        let mut runtime = if runtime_path.file_name() == Some("Cargo.toml") {
            RaHostRuntime::from_manifest_path(runtime_path.as_std_path())
        } else {
            RaHostRuntime::from_workspace_root(runtime_path.as_std_path())
        }
        .map_err(|err| {
            format!(
                "failed to initialize rust-analyzer runtime from `{}`: {err}",
                runtime_path
            )
        })?;
        execute(&planned, &mut runtime)
    } else {
        let mut runtime = MockHostRuntime::new();
        execute(&planned, &mut runtime)
    };

    print_eval_result(
        &result,
        args.relation_filters.as_slice(),
        args.show_empty,
        args.max_rows,
    );
    Ok(())
}

fn decode_common_args(args: &LangCommonArgs) -> Result<(Utf8PathBuf, Vec<Utf8PathBuf>), String> {
    let program_path = into_utf8_pathbuf(args.program.clone(), "program path")?;
    let mut include_dirs = args
        .include_dirs
        .iter()
        .cloned()
        .map(|path| into_utf8_pathbuf(path, "--include-dir"))
        .collect::<Result<Vec<_>, _>>()?;

    if let Ok(cwd) = std::env::current_dir()
        && let Ok(cwd_utf8) = Utf8PathBuf::from_path_buf(cwd)
        && !include_dirs.iter().any(|dir| dir == &cwd_utf8)
    {
        include_dirs.push(cwd_utf8);
    }
    Ok((program_path, include_dirs))
}

fn into_utf8_pathbuf(path: PathBuf, label: &str) -> Result<Utf8PathBuf, String> {
    Utf8PathBuf::from_path_buf(path.clone())
        .map_err(|_| format!("{label} must be valid UTF-8: {}", path.display()))
}

fn load_and_plan(
    program_path: &Utf8Path,
    include_dirs: &[Utf8PathBuf],
) -> Result<PlannedProgram, String> {
    let parsed = parse_program_from_file(program_path, include_dirs)
        .map_err(|diags| format_parse_diagnostics(program_path, &diags))?;
    let sources = parsed.sources().clone();
    let resolved = resolve(parsed)
        .map_err(|diags| format_compiler_diagnostics("resolve", &sources, &diags))?;
    let typed = typecheck(resolved)
        .map_err(|diags| format_compiler_diagnostics("typecheck", &sources, &diags))?;
    plan(typed).map_err(|diags| format_compiler_diagnostics("plan", &sources, &diags))
}

fn print_plan_summary(program_path: &Utf8Path, planned: &PlannedProgram) {
    let max_stratum = planned.strata().values().copied().max().unwrap_or(0);
    let stratum_count = if planned.strata().is_empty() {
        0
    } else {
        max_stratum + 1
    };

    println!("compiled: {}", program_path);
    println!("predicates: {}", planned.predicates().len());
    println!("facts: {}", planned.facts().len());
    println!("rules: {}", planned.planned_rules().len());
    println!("strata: {stratum_count}");
    println!("sccs: {}", planned.sccs().len());
}

fn print_eval_result(
    result: &EvalResult,
    relation_filters: &[String],
    show_empty: bool,
    max_rows: usize,
) {
    let status = match result.status {
        EvalStatus::Ok => "ok",
        EvalStatus::Partial => "partial",
    };
    println!("status: {status}");
    println!("iterations: {}", result.iterations);

    if !result.notes.is_empty() {
        println!("notes:");
        for note in &result.notes {
            println!("- [{}] {}", note.section, note.message);
        }
    }

    println!("relations:");
    for (name, rows) in &result.relations {
        if !relation_filters.is_empty() && !relation_filters.iter().any(|f| f == name) {
            continue;
        }
        if !show_empty && rows.is_empty() {
            continue;
        }

        println!("{name}: {}", rows.len());
        let row_limit = max_rows.max(1);
        let mut shown = 0usize;
        for row in rows {
            if shown >= row_limit {
                break;
            }
            println!("  {}({}).", name, format_row_values(row));
            shown += 1;
        }
        if rows.len() > shown {
            println!(
                "  ... omitted {} row(s); use --max-rows to expand.",
                rows.len() - shown
            );
        }
    }
}

fn format_row_values(row: &[RuntimeValue]) -> String {
    row.iter().map(format_value).collect::<Vec<_>>().join(", ")
}

fn format_value(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Int(v) => v.to_string(),
        RuntimeValue::String(v) => format!("{v:?}"),
        RuntimeValue::Bool(v) => v.to_string(),
        RuntimeValue::Enum { name, variant } => format!("{name}::{variant}"),
        RuntimeValue::Host { kind, id } => format!("{}#0x{id:016x}", format_host_kind(*kind)),
        RuntimeValue::None => "none".to_string(),
        RuntimeValue::Some(inner) => format!("some({})", format_value(inner)),
        RuntimeValue::List(items) => {
            let inner = items
                .iter()
                .map(format_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
    }
}

fn format_host_kind(kind: HostValueKind) -> &'static str {
    match kind {
        HostValueKind::Def => "Def",
        HostValueKind::Span => "Span",
        HostValueKind::TypeRef => "TypeRef",
        HostValueKind::Node => "Node",
        HostValueKind::Call => "Call",
        HostValueKind::Ref => "Ref",
        HostValueKind::Impl => "Impl",
    }
}

fn format_parse_diagnostics(
    program_path: &Utf8Path,
    diagnostics: &[raql_syntax::Diagnostic],
) -> String {
    let mut out = format!(
        "parse failed for `{program_path}` with {} diagnostic(s):",
        diagnostics.len()
    );
    for diag in diagnostics {
        out.push_str(&format!("\n- [{}] {}", diag.code, diag.message));
        if let Some(primary) = &diag.primary {
            out.push_str(&format!(
                "\n  primary: {} ({})",
                format_label_location(primary),
                primary.message
            ));
        }
        for secondary in &diag.secondary {
            out.push_str(&format!(
                "\n  secondary: {} ({})",
                format_label_location(secondary),
                secondary.message
            ));
        }
        if !diag.include_stack.is_empty() {
            out.push_str("\n  include stack:");
            for path in &diag.include_stack {
                out.push_str(&format!("\n  - {path}"));
            }
        }
        for note in &diag.notes {
            out.push_str(&format!("\n  note: {note}"));
        }
    }
    out
}

fn format_label_location(label: &DiagnosticLabel) -> String {
    let start = label.span.range.start();
    let end = label.span.range.end();
    format!(
        "file#{}, bytes {}..{}",
        label.span.file.0,
        u32::from(start),
        u32::from(end)
    )
}

fn format_compiler_diagnostics(
    stage: &str,
    sources: &raql_syntax::SourceMap,
    diagnostics: &[CompilerDiagnostic],
) -> String {
    let mut out = format!("{stage} failed with {} diagnostic(s):", diagnostics.len());
    for diag in diagnostics {
        out.push_str(&format!("\n- [{}] {}", diag.code_str(), diag));
        if let Some(span) = diag.span() {
            out.push_str(&format!("\n  at {}", format_span_location(sources, span)));
        }
        if let Some(help) = Diagnostic::help(diag) {
            out.push_str(&format!("\n  help: {help}"));
        }
        if !diag.include_stack().is_empty() {
            out.push_str("\n  include stack:");
            for path in diag.include_stack() {
                out.push_str(&format!("\n  - {path}"));
            }
        }
    }
    out
}

fn format_span_location(sources: &raql_syntax::SourceMap, span: SrcSpan) -> String {
    let start = u32::from(span.range.start()) as usize;
    let end = u32::from(span.range.end()) as usize;
    match sources.get(span.file) {
        Some(file) => {
            let start = clamp_to_char_boundary(file.text(), start);
            let end = clamp_to_char_boundary(file.text(), end);
            let (line, col) = line_col_for_offset(file.text(), start);
            let (end_line, end_col) = line_col_for_offset(file.text(), end);
            format!("{}:{line}:{col}-{end_line}:{end_col}", file.path())
        }
        None => format!("file#{}:{start}..{end}", span.file.0),
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
