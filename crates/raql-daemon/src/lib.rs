#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::{fs::OpenOptions, io::ErrorKind};
use std::thread;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use raql_compiler::{PlannedProgram, plan, required_extern_capabilities, resolve, typecheck};
use raql_engine::{EvalResult, RuntimeValue};
use raql_host::MissingCapabilitiesError;
use raql_host_ra::daemon::{DaemonWorkspace, resolve_workspace_root};
use raql_protocol::{
    DaemonEvent, DaemonRequest, DaemonState, ErrorEvent, PROTOCOL_VERSION, PlanSummary,
    ProtocolValue, RelationRows, RunNote, RunRequest, RunResult, SessionEvent,
};
use raql_syntax::parse_program_from_file;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("{0}")]
    Message(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn socket_path_for_workspace(workspace_root: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workspace_root.to_string_lossy().hash(&mut hasher);
    std::env::temp_dir().join(format!(
        "raql-daemon-v{}-{:016x}.sock",
        PROTOCOL_VERSION,
        hasher.finish()
    ))
}

fn spawn_lock_path_for_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("sock.lock")
}

fn startup_log_path_for_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("sock.startup.log")
}

pub fn spawn_or_connect(current_exe: &Path, rust_input: &Path) -> Result<DaemonClient, DaemonError> {
    let workspace_root = resolve_workspace_root(rust_input).map_err(|err| {
        DaemonError::Message(format!(
            "failed to initialize rust-analyzer runtime from `{}`: {err}",
            rust_input.display()
        ))
    })?;
    let socket_path = socket_path_for_workspace(&workspace_root);
    if let Ok(stream) = UnixStream::connect(&socket_path) {
        return Ok(DaemonClient { stream });
    }
    let lock_path = spawn_lock_path_for_socket(&socket_path);
    let startup_log_path = startup_log_path_for_socket(&socket_path);
    if let Some(_lock) = try_acquire_spawn_lock(&lock_path)? {
        if let Ok(stream) = UnixStream::connect(&socket_path) {
            return Ok(DaemonClient { stream });
        }
        let stderr = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&startup_log_path)?;
        let mut child = Command::new(current_exe)
            .arg("__daemon-serve")
            .arg("--workspace-root")
            .arg(&workspace_root)
            .arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()?;
        let connected = wait_for_connect(
            &socket_path,
            Some(startup_log_path.as_path()),
            Some(&mut child),
        );
        if connected.is_ok() {
            let _ = std::fs::remove_file(&startup_log_path);
        }
        return connected;
    }
    wait_for_connect(&socket_path, Some(startup_log_path.as_path()), None)
}

fn wait_for_connect(
    socket_path: &Path,
    startup_log_path: Option<&Path>,
    mut child: Option<&mut Child>,
) -> Result<DaemonClient, DaemonError> {
    for _ in 0..200 {
        if let Some(child) = child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            return Err(DaemonError::Message(connection_failure_message(
                socket_path,
                startup_log_path,
                Some(status),
            )));
        }
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(DaemonClient { stream }),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    Err(DaemonError::Message(connection_failure_message(
        socket_path,
        startup_log_path,
        None,
    )))
}

fn connection_failure_message(
    socket_path: &Path,
    startup_log_path: Option<&Path>,
    child_status: Option<ExitStatus>,
) -> String {
    let mut message = format!("failed to connect to daemon socket `{}`", socket_path.display());
    if let Some(status) = child_status {
        message.push_str(format!("; daemon exited with status {status}").as_str());
    }
    if let Some(log_path) = startup_log_path
        && let Ok(log) = std::fs::read_to_string(log_path)
    {
        let trimmed = log.trim();
        if !trimmed.is_empty() {
            let tail = trimmed
                .lines()
                .rev()
                .take(10)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            message.push_str(format!("; startup log: {tail}").as_str());
        }
    }
    message
}

struct SpawnLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn try_acquire_spawn_lock(path: &Path) -> Result<Option<SpawnLock>, DaemonError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => Ok(Some(SpawnLock {
            path: path.to_path_buf(),
            _file: file,
        })),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    pub fn run(&mut self, program_path: &Utf8Path, include_dirs: &[Utf8PathBuf]) -> Result<Vec<DaemonEvent>, DaemonError> {
        let request = DaemonRequest::Run(RunRequest {
            protocol_version: PROTOCOL_VERSION,
            program_path: program_path.as_str().to_string(),
            include_dirs: include_dirs.iter().map(|p| p.as_str().to_string()).collect(),
        });
        let mut writer = BufWriter::new(self.stream.try_clone()?);
        serde_json::to_writer(&mut writer, &request)?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        let mut reader = BufReader::new(self.stream.try_clone()?);
        let mut line = String::new();
        let mut events = Vec::new();
        let mut saw_done = false;
        let mut saw_terminal = false;
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            let event: DaemonEvent = serde_json::from_str(line.trim_end())?;
            match &event {
                DaemonEvent::Result(_) | DaemonEvent::Error(_) => {
                    saw_terminal = true;
                }
                DaemonEvent::Done => {
                    saw_done = true;
                }
                _ => {}
            }
            let done = matches!(event, DaemonEvent::Done);
            events.push(event);
            if done {
                break;
            }
        }
        if !saw_done {
            return Err(DaemonError::Message(
                "daemon connection closed before completion".to_string(),
            ));
        }
        if !saw_terminal {
            return Err(DaemonError::Message(
                "daemon response missing terminal result".to_string(),
            ));
        }
        Ok(events)
    }
}

pub fn serve(socket_path: &Path, workspace_root: &Path) -> Result<(), DaemonError> {
    let mut session = DaemonWorkspace::from_workspace_root(workspace_root)
        .map_err(|err| DaemonError::Message(format!("failed to initialize rust-analyzer runtime from `{}`: {err}", workspace_root.display())))?;
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)?;
    let mut cold = true;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = handle_connection(stream, &mut session, cold);
                cold = false;
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn handle_connection(
    stream: UnixStream,
    session: &mut DaemonWorkspace,
    cold: bool,
) -> Result<(), DaemonError> {
    let mut writer = BufWriter::new(stream.try_clone()?);
    let mut reader = BufReader::new(stream);
    if let Err(err) = handle_connection_impl(&mut reader, session, cold, &mut writer) {
        let _ = emit_event(
            &mut writer,
            &DaemonEvent::Error(ErrorEvent {
                message: err.to_string(),
            }),
        );
        let _ = emit_event(&mut writer, &DaemonEvent::Done);
    }
    Ok(())
}

fn handle_connection_impl(
    reader: &mut BufReader<UnixStream>,
    session: &mut DaemonWorkspace,
    cold: bool,
    writer: &mut BufWriter<UnixStream>,
) -> Result<(), DaemonError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(DaemonError::Message(
            "daemon connection closed before request".to_string(),
        ));
    }
    let request: DaemonRequest = serde_json::from_str(line.trim_end())?;
    match request {
        DaemonRequest::Run(run) => handle_run(run, session, cold, writer)?,
    }
    Ok(())
}
fn handle_run(
    run: RunRequest,
    session: &mut DaemonWorkspace,
    cold: bool,
    writer: &mut BufWriter<UnixStream>,
) -> Result<(), DaemonError> {
    if run.protocol_version != PROTOCOL_VERSION {
        emit_event(
            writer,
            &DaemonEvent::Error(ErrorEvent {
                message: format!(
                    "protocol version mismatch: client={}, daemon={}",
                    run.protocol_version, PROTOCOL_VERSION
                ),
            }),
        )?;
        emit_event(writer, &DaemonEvent::Done)?;
        return Ok(());
    }

    emit_event(
        writer,
        &DaemonEvent::Session(SessionEvent {
            workspace_root: session.workspace_root().display().to_string(),
            daemon_state: if cold { DaemonState::Cold } else { DaemonState::Warm },
            workspace_epoch: session.workspace_epoch(),
            content_revision: session.content_revision(),
            supported_capabilities: session.supported_capabilities().into_iter().map(|cap| cap.as_str().to_string()).collect(),
        }),
    )?;

    let include_dirs = run
        .include_dirs
        .iter()
        .map(|path| Utf8PathBuf::from(path.as_str()))
        .collect::<Vec<_>>();
    let program_path = Utf8PathBuf::from(run.program_path.as_str());
    let planned = load_and_plan(&program_path, &include_dirs).map_err(DaemonError::Message)?;
    emit_event(writer, &DaemonEvent::Plan(plan_summary(&program_path, &planned)))?;

    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    if let Err(err) = MissingCapabilitiesError::from_required_and_supported(required, session.supported_capabilities()) {
        emit_event(writer, &DaemonEvent::Error(ErrorEvent { message: err.to_string() }))?;
        emit_event(writer, &DaemonEvent::Done)?;
        return Ok(());
    }

    let result = session
        .run_planned(&planned)
        .map_err(|err| DaemonError::Message(err.to_string()))?;
    emit_event(writer, &DaemonEvent::Result(protocol_run_result(result)))?;
    emit_event(writer, &DaemonEvent::Done)?;
    Ok(())
}

fn emit_event(writer: &mut BufWriter<UnixStream>, event: &DaemonEvent) -> Result<(), DaemonError> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn load_and_plan(program_path: &Utf8Path, include_dirs: &[Utf8PathBuf]) -> Result<PlannedProgram, String> {
    let parsed = parse_program_from_file(program_path, include_dirs)
        .map_err(|diags| format!("parse failed for `{program_path}`: {diags:?}"))?;
    let resolved = resolve(parsed).map_err(|diags| format!("resolve failed: {diags:?}"))?;
    let typed = typecheck(resolved).map_err(|diags| format!("typecheck failed: {diags:?}"))?;
    plan(typed).map_err(|diags| format!("plan failed: {diags:?}"))
}

fn plan_summary(program_path: &Utf8Path, planned: &PlannedProgram) -> PlanSummary {
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

fn protocol_run_result(result: EvalResult) -> RunResult {
    let mut relations = BTreeMap::new();
    for (name, rows) in result.relations {
        relations.insert(
            name.clone(),
            RelationRows {
                name,
                rows: rows
                    .into_iter()
                    .map(|row| row.into_iter().map(protocol_value).collect())
                    .collect(),
            },
        );
    }
    RunResult {
        status: match result.status {
            raql_engine::EvalStatus::Ok => "ok".to_string(),
            raql_engine::EvalStatus::Partial => "partial".to_string(),
        },
        iterations: result.iterations,
        notes: result
            .notes
            .into_iter()
            .map(|note| RunNote {
                section: note.section,
                message: note.message,
            })
            .collect(),
        relations: relations.into_values().collect(),
    }
}

fn protocol_value(value: RuntimeValue) -> ProtocolValue {
    match value {
        RuntimeValue::Int(v) => ProtocolValue::Int(v),
        RuntimeValue::String(v) => ProtocolValue::String(v),
        RuntimeValue::Bool(v) => ProtocolValue::Bool(v),
        RuntimeValue::Enum { name, variant } => ProtocolValue::Enum { name, variant },
        RuntimeValue::Host { kind, id } => ProtocolValue::Host {
            kind: kind.label().to_string(),
            id,
        },
        RuntimeValue::None => ProtocolValue::None,
        RuntimeValue::Some(inner) => ProtocolValue::Some(Box::new(protocol_value(*inner))),
        RuntimeValue::List(items) => {
            ProtocolValue::List(items.into_iter().map(protocol_value).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROTOCOL_VERSION, connection_failure_message, socket_path_for_workspace,
        startup_log_path_for_socket,
    };
    use std::fs;

    #[test]
    fn socket_path_for_workspace_is_protocol_versioned() {
        let workspace = std::env::temp_dir().join("raql-daemon-versioned-socket");
        let socket = socket_path_for_workspace(workspace.as_path());
        let name = socket
            .file_name()
            .and_then(|value| value.to_str())
            .expect("socket file name");
        assert!(
            name.contains(&format!("v{PROTOCOL_VERSION}")),
            "socket path should encode protocol version; name={name}"
        );
    }

    #[test]
    fn connection_failure_message_includes_startup_log_contents() {
        let temp = std::env::temp_dir().join(format!("raql-daemon-log-test-{}", std::process::id()));
        let socket = temp.join("daemon.sock");
        let log = startup_log_path_for_socket(socket.as_path());
        if let Some(parent) = log.parent() {
            fs::create_dir_all(parent).expect("create log parent");
        }
        fs::write(&log, "io error: Operation not permitted\nbind failed\n")
            .expect("write startup log");

        let message = connection_failure_message(socket.as_path(), Some(log.as_path()), None);
        assert!(message.contains("failed to connect to daemon socket"), "message={message}");
        assert!(message.contains("Operation not permitted"), "message={message}");
        assert!(message.contains("bind failed"), "message={message}");
    }
}
