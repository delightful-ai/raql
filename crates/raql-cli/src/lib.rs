#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand};
use raql_compiler::{PlannedProgram, plan, resolve, typecheck};
use raql_daemon::{spawn_or_connect, serve};
use raql_engine::{EvalResult, HostValueKind, RuntimeValue, execute};
use raql_host::MockHostRuntime;
use raql_host_ra::legacy::LegacyRaHostRuntime;
use raql_protocol::{DaemonEvent, DaemonState, PlanSummary, ProtocolValue, RelationRows};
use raql_syntax::parse_program_from_file;

#[derive(Debug, Parser)]
#[command(name = "raql")]
#[command(about = "RAQL command line interface")]
struct Cli {
    #[command(subcommand)]
    command: TopLevelCommand,
}

#[derive(Debug, Subcommand)]
enum TopLevelCommand {
    #[command(subcommand)]
    Lang(LangCommand),
    #[command(subcommand)]
    Dev(DevCommand),
    #[command(name = "__daemon-serve", hide = true)]
    DaemonServe(DaemonServeArgs),
}

#[derive(Debug, Subcommand)]
enum LangCommand {
    Check(LangCheckArgs),
    Run(LangRunArgs),
}

#[derive(Debug, Subcommand)]
enum DevCommand {
    // TODO(ra-daemon-cutover): remove this once all remaining direct-runtime
    // debugging and legacy backend coverage has migrated to daemon-backed or
    // explicit internal host-ra test helpers.
    #[command(name = "run-direct")]
    RunDirect(LangRunArgs),
}

#[derive(Debug, Args)]
struct DaemonServeArgs {
    #[arg(long = "workspace-root")]
    workspace_root: PathBuf,
    #[arg(long = "socket")]
    socket: PathBuf,
}

#[derive(Debug, Args, Clone)]
struct LangCommonArgs {
    program: PathBuf,
    #[arg(long = "include-dir", short = 'I')]
    include_dirs: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct LangCheckArgs {
    #[command(flatten)]
    common: LangCommonArgs,
}

#[derive(Debug, Args, Clone)]
struct LangRunArgs {
    #[command(flatten)]
    common: LangCommonArgs,
    #[arg(long = "rust-file")]
    rust_file: Option<PathBuf>,
    #[arg(long = "relation")]
    relation_filters: Vec<String>,
    #[arg(long = "show-empty", default_value_t = false)]
    show_empty: bool,
    #[arg(long = "max-rows", default_value_t = 50)]
    max_rows: usize,
}

pub fn main_entry() -> ExitCode {
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
        TopLevelCommand::Dev(cmd) => run_dev(cmd),
        TopLevelCommand::DaemonServe(args) => {
            serve(&args.socket, &args.workspace_root).map_err(|err| err.to_string())
        }
    }
}

fn run_lang(cmd: LangCommand) -> Result<(), String> {
    match cmd {
        LangCommand::Check(args) => run_lang_check(args),
        LangCommand::Run(args) => run_lang_run(args),
    }
}

fn run_dev(cmd: DevCommand) -> Result<(), String> {
    match cmd {
        DevCommand::RunDirect(args) => run_direct(args),
    }
}

fn run_lang_check(args: LangCheckArgs) -> Result<(), String> {
    let (program_path, include_dirs) = decode_common_args(&args.common)?;
    let planned = load_and_plan(&program_path, &include_dirs)?;
    print_plan_summary(&plan_summary_from_planned(&program_path, &planned));
    Ok(())
}

fn run_lang_run(args: LangRunArgs) -> Result<(), String> {
    let rust_file = args.rust_file.clone().ok_or_else(|| {
        "supported `raql lang run` requires `--rust-file`; use `raql dev run-direct` for the quarantined direct runtime path".to_string()
    })?;
    let (program_path, include_dirs) = decode_common_args(&args.common)?;
    let current_exe = std::env::current_exe().map_err(|err| format!("failed to locate current executable: {err}"))?;
    let mut client = spawn_or_connect(&current_exe, &rust_file).map_err(|err| err.to_string())?;
    let events = client.run(&program_path, &include_dirs).map_err(|err| err.to_string())?;
    render_events(&events, &args.relation_filters, args.show_empty, args.max_rows)
}

fn run_direct(args: LangRunArgs) -> Result<(), String> {
    // TODO(ra-daemon-cutover): delete this eager direct-runtime CLI path after
    // the legacy host-ra tests and bring-up workflows stop depending on it.
    let (program_path, include_dirs) = decode_common_args(&args.common)?;
    let planned = load_and_plan(&program_path, &include_dirs)?;
    print_plan_summary(&plan_summary_from_planned(&program_path, &planned));

    let result = if let Some(rust_file) = args.rust_file {
        let runtime_path = into_utf8_pathbuf(rust_file, "--rust-file")?;
        let mut runtime = if runtime_path.file_name() == Some("Cargo.toml") {
            LegacyRaHostRuntime::from_manifest_path_no_deps(runtime_path.as_std_path())
        } else {
            LegacyRaHostRuntime::from_workspace_root_no_deps(runtime_path.as_std_path())
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

    print_eval_result(&result, &args.relation_filters, args.show_empty, args.max_rows);
    Ok(())
}

fn render_events(
    events: &[DaemonEvent],
    relation_filters: &[String],
    show_empty: bool,
    max_rows: usize,
) -> Result<(), String> {
    let mut saw_result = false;
    let mut saw_done = false;
    for event in events {
        match event {
            DaemonEvent::Session(session) => {
                let state = match session.daemon_state {
                    DaemonState::Cold => "cold",
                    DaemonState::Warm => "warm",
                };
                println!("daemon: {state}");
                println!("workspace: {}", session.workspace_root);
                println!("workspace_epoch: {}", session.workspace_epoch);
                println!("content_revision: {}", session.content_revision);
            }
            DaemonEvent::Plan(plan) => print_plan_summary(plan),
            DaemonEvent::Result(result) => {
                saw_result = true;
                print_protocol_result(result, relation_filters, show_empty, max_rows)
            }
            DaemonEvent::Error(error) => return Err(error.message.clone()),
            DaemonEvent::Done => {
                saw_done = true;
                break;
            }
        }
    }
    if !saw_done {
        return Err("daemon response ended before completion".to_string());
    }
    if !saw_result {
        return Err("daemon response completed without a run result".to_string());
    }
    Ok(())
}

fn decode_common_args(args: &LangCommonArgs) -> Result<(Utf8PathBuf, Vec<Utf8PathBuf>), String> {
    let cwd = std::env::current_dir().map_err(|err| format!("failed to determine current directory: {err}"))?;
    let program_path = normalize_existing_path(args.program.clone(), "program path", cwd.as_path())?;
    let mut include_dirs = args
        .include_dirs
        .iter()
        .cloned()
        .map(|path| normalize_existing_path(path, "--include-dir", cwd.as_path()))
        .collect::<Result<Vec<_>, _>>()?;
    if let Ok(cwd_utf8) = Utf8PathBuf::from_path_buf(cwd)
        && !include_dirs.iter().any(|dir| dir == &cwd_utf8)
    {
        include_dirs.push(cwd_utf8);
    }
    Ok((program_path, include_dirs))
}

fn normalize_existing_path(path: PathBuf, label: &str, cwd: &std::path::Path) -> Result<Utf8PathBuf, String> {
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let canonical = fs::canonicalize(&absolute)
        .map_err(|err| format!("{label} must exist and be readable ({}): {err}", absolute.display()))?;
    into_utf8_pathbuf(canonical, label)
}

fn into_utf8_pathbuf(path: PathBuf, label: &str) -> Result<Utf8PathBuf, String> {
    Utf8PathBuf::from_path_buf(path.clone())
        .map_err(|_| format!("{label} must be valid UTF-8: {}", path.display()))
}

fn load_and_plan(program_path: &Utf8Path, include_dirs: &[Utf8PathBuf]) -> Result<PlannedProgram, String> {
    let parsed = parse_program_from_file(program_path, include_dirs)
        .map_err(|diags| format!("parse failed for `{program_path}`: {diags:?}"))?;
    let resolved = resolve(parsed).map_err(|diags| format!("resolve failed: {diags:?}"))?;
    let typed = typecheck(resolved).map_err(|diags| format!("typecheck failed: {diags:?}"))?;
    plan(typed).map_err(|diags| format!("plan failed: {diags:?}"))
}

fn plan_summary_from_planned(program_path: &Utf8Path, planned: &PlannedProgram) -> PlanSummary {
    let max_stratum = planned.strata().values().copied().max().unwrap_or(0);
    let strata = if planned.strata().is_empty() { 0 } else { max_stratum + 1 };
    PlanSummary {
        program_path: program_path.as_str().to_string(),
        predicates: planned.predicates().len(),
        facts: planned.facts().len(),
        rules: planned.planned_rules().len(),
        strata,
        sccs: planned.sccs().len(),
    }
}

fn print_plan_summary(plan: &PlanSummary) {
    println!("compiled: {}", plan.program_path);
    println!("predicates: {}", plan.predicates);
    println!("facts: {}", plan.facts);
    println!("rules: {}", plan.rules);
    println!("strata: {}", plan.strata);
    println!("sccs: {}", plan.sccs);
}

fn print_protocol_result(
    result: &raql_protocol::RunResult,
    relation_filters: &[String],
    show_empty: bool,
    max_rows: usize,
) {
    println!("status: {}", result.status);
    println!("iterations: {}", result.iterations);
    if !result.notes.is_empty() {
        println!("notes:");
        for note in &result.notes {
            println!("- [{}] {}", note.section, note.message);
        }
    }
    println!("relations:");
    for RelationRows { name, rows } in &result.relations {
        if !relation_filters.is_empty() && !relation_filters.iter().any(|filter| filter == name) {
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
            println!("  {}({}).", name, format_protocol_row_values(row));
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

fn print_eval_result(
    result: &EvalResult,
    relation_filters: &[String],
    show_empty: bool,
    max_rows: usize,
) {
    let status = match result.status {
        raql_engine::EvalStatus::Ok => "ok",
        raql_engine::EvalStatus::Partial => "partial",
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
        if !relation_filters.is_empty() && !relation_filters.iter().any(|filter| filter == name) {
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

fn format_protocol_row_values(row: &[ProtocolValue]) -> String {
    row.iter().map(format_protocol_value).collect::<Vec<_>>().join(", ")
}

fn format_protocol_value(value: &ProtocolValue) -> String {
    match value {
        ProtocolValue::Int(v) => v.to_string(),
        ProtocolValue::String(v) => format!("{v:?}"),
        ProtocolValue::Bool(v) => v.to_string(),
        ProtocolValue::Enum { name, variant } => format!("{name}::{variant}"),
        ProtocolValue::Host { kind, id } => format!("{kind}#0x{id:016x}"),
        ProtocolValue::None => "none".to_string(),
        ProtocolValue::Some(inner) => format!("some({})", format_protocol_value(inner)),
        ProtocolValue::List(items) => {
            let inner = items.iter().map(format_protocol_value).collect::<Vec<_>>().join(", ");
            format!("[{inner}]")
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
            let inner = items.iter().map(format_value).collect::<Vec<_>>().join(", ");
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
