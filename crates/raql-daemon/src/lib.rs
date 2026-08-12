#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::{fs::OpenOptions, io::ErrorKind};
use std::thread;
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use raql_compiler::{PlannedProgram, plan, required_extern_capabilities, resolve, typecheck};
use raql_host::MissingCapabilitiesError;
use raql_host_ra::daemon::{DaemonWorkspace, WarmupSnapshot, resolve_workspace_root};
use raql_host_ra::{ProjectedRunResult, ProjectedValue};
use raql_protocol::{
    DaemonEvent, DaemonRequest, DaemonState, ErrorEvent, PROTOCOL_VERSION, PlanSummary,
    ProtocolValue, RelationRows, RunNote, RunRequest, RunResult, SessionEvent,
    WarmupPhaseEvent, WarmupStatusEvent,
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

const DEFAULT_IDLE_TIMEOUT_MS: u64 = 21_600_000;
const IDLE_TIMEOUT_ENV: &str = "RAQL_DAEMON_IDLE_TIMEOUT_MS";
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 20_000;
const CONNECT_TIMEOUT_ENV: &str = "RAQL_DAEMON_CONNECT_TIMEOUT_MS";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 5_000;
const REQUEST_TIMEOUT_ENV: &str = "RAQL_DAEMON_REQUEST_TIMEOUT_MS";
const ACCEPT_POLL_INTERVAL_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WarmupGeneration(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarmupPhase {
    Cold,
    Running,
    Warm,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WarmupStatus {
    phase: WarmupPhase,
    generation: WarmupGeneration,
    error: Option<Arc<str>>,
}

type WarmupStateCell = Mutex<WarmupStatus>;

trait WarmupExecutor: Send + Sync + 'static {
    fn spawn(
        &self,
        analysis: WarmupSnapshot,
        generation: WarmupGeneration,
        state: Arc<WarmupStateCell>,
    );
}

#[derive(Default)]
struct ParallelPrimeWarmupExecutor;

impl WarmupExecutor for ParallelPrimeWarmupExecutor {
    fn spawn(
        &self,
        analysis: WarmupSnapshot,
        generation: WarmupGeneration,
        state: Arc<WarmupStateCell>,
    ) {
        let worker_threads = warmup_worker_threads();
        thread::spawn(move || {
            let result = analysis.parallel_prime_caches(worker_threads);
            finish_warmup_generation(state, generation, result);
        });
    }
}

struct WarmupCoordinator {
    state: Arc<WarmupStateCell>,
    current_generation: WarmupGeneration,
    executor: Arc<dyn WarmupExecutor>,
}

impl WarmupCoordinator {
    fn new(executor: Arc<dyn WarmupExecutor>) -> Self {
        Self {
            state: Arc::new(Mutex::new(WarmupStatus {
                phase: WarmupPhase::Cold,
                generation: WarmupGeneration(0),
                error: None,
            })),
            current_generation: WarmupGeneration(0),
            executor,
        }
    }

    fn start_initial(&mut self, analysis: WarmupSnapshot) {
        self.start_next(analysis);
    }

    fn restart_after_reload(&mut self, analysis: WarmupSnapshot) {
        self.start_next(analysis);
    }

    fn snapshot(&self) -> WarmupStatus {
        self.state.lock().expect("lock warmup state").clone()
    }

    fn start_next(&mut self, analysis: WarmupSnapshot) {
        let next_generation = WarmupGeneration(self.current_generation.0 + 1);
        self.current_generation = next_generation;
        {
            let mut state = self.state.lock().expect("lock warmup state");
            *state = WarmupStatus {
                phase: WarmupPhase::Running,
                generation: next_generation,
                error: None,
            };
        }
        self.executor
            .spawn(analysis, next_generation, Arc::clone(&self.state));
    }
}

fn finish_warmup_generation(
    state: Arc<WarmupStateCell>,
    generation: WarmupGeneration,
    result: Result<(), String>,
) {
    let mut current = state.lock().expect("lock warmup state");
    if current.generation != generation {
        return;
    }
    match result {
        Ok(()) => {
            current.phase = WarmupPhase::Warm;
            current.error = None;
        }
        Err(message) => {
            current.phase = WarmupPhase::Failed;
            current.error = Some(message.into());
        }
    }
}

fn warmup_worker_threads() -> usize {
    std::env::var("RAQL_PRIME_CACHE_THREADS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|threads| threads.get().min(4))
                .unwrap_or(1)
        })
}

fn protocol_warmup_status(status: &WarmupStatus) -> WarmupStatusEvent {
    WarmupStatusEvent {
        phase: match status.phase {
            WarmupPhase::Cold => WarmupPhaseEvent::Cold,
            WarmupPhase::Running => WarmupPhaseEvent::Running,
            WarmupPhase::Warm => WarmupPhaseEvent::Warm,
            WarmupPhase::Failed => WarmupPhaseEvent::Failed,
        },
        generation: status.generation.0,
        error: status.error.as_ref().map(|message| message.to_string()),
    }
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
    spawn_or_connect_with_timeout(current_exe, rust_input, daemon_connect_timeout())
}

fn spawn_or_connect_with_timeout(
    current_exe: &Path,
    rust_input: &Path,
    connect_timeout: Duration,
) -> Result<DaemonClient, DaemonError> {
    let workspace_root = resolve_workspace_root(rust_input).map_err(|err| {
        DaemonError::Message(format!(
            "failed to initialize rust-analyzer runtime from `{}`: {err}",
            rust_input.display()
        ))
    })?;
    let socket_path = socket_path_for_workspace(&workspace_root);
    trace_daemon_debug(format!(
        "client.resolve workspace_root={} socket={}",
        workspace_root.display(),
        socket_path.display()
    ));
    if let Ok(stream) = UnixStream::connect(&socket_path) {
        trace_daemon_debug(format!("client.connect_hit socket={}", socket_path.display()));
        return Ok(DaemonClient { stream });
    }
    let lock_path = spawn_lock_path_for_socket(&socket_path);
    let startup_log_path = startup_log_path_for_socket(&socket_path);
    if let Some(_lock) = try_acquire_spawn_lock(&lock_path)? {
        if let Ok(stream) = UnixStream::connect(&socket_path) {
            trace_daemon_debug(format!(
                "client.connect_hit_after_lock socket={}",
                socket_path.display()
            ));
            return Ok(DaemonClient { stream });
        }
        trace_daemon_debug(format!(
            "client.spawn socket={} workspace_root={}",
            socket_path.display(),
            workspace_root.display()
        ));
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
            connect_timeout,
        );
        if connected.is_err() {
            cleanup_failed_spawn(&mut child)?;
        }
        if connected.is_ok() {
            let _ = std::fs::remove_file(&startup_log_path);
        }
        return connected;
    }
    trace_daemon_debug(format!(
        "client.wait_existing socket={}",
        socket_path.display()
    ));
    wait_for_connect(
        &socket_path,
        Some(startup_log_path.as_path()),
        None,
        connect_timeout,
    )
}

fn wait_for_connect(
    socket_path: &Path,
    startup_log_path: Option<&Path>,
    mut child: Option<&mut Child>,
    connect_timeout: Duration,
) -> Result<DaemonClient, DaemonError> {
    let start = Instant::now();
    while start.elapsed() < connect_timeout {
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

fn cleanup_failed_spawn(child: &mut Child) -> Result<(), DaemonError> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
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
    try_acquire_spawn_lock_with_timeout(path, daemon_connect_timeout())
}

fn try_acquire_spawn_lock_with_timeout(
    path: &Path,
    stale_after: Duration,
) -> Result<Option<SpawnLock>, DaemonError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => Ok(Some(SpawnLock {
            path: path.to_path_buf(),
            _file: file,
        })),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            if spawn_lock_is_stale(path, stale_after)? {
                let _ = std::fs::remove_file(path);
                match OpenOptions::new().write(true).create_new(true).open(path) {
                    Ok(file) => Ok(Some(SpawnLock {
                        path: path.to_path_buf(),
                        _file: file,
                    })),
                    Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(None),
                    Err(err) => Err(err.into()),
                }
            } else {
                Ok(None)
            }
        }
        Err(err) => Err(err.into()),
    }
}

fn spawn_lock_is_stale(path: &Path, stale_after: Duration) -> Result<bool, DaemonError> {
    let modified = std::fs::metadata(path)?.modified()?;
    Ok(modified.elapsed().unwrap_or(Duration::ZERO) >= stale_after)
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
    serve_with_timeouts(
        socket_path,
        workspace_root,
        idle_shutdown_timeout(),
        request_read_timeout(),
    )
}

fn serve_with_timeouts(
    socket_path: &Path,
    workspace_root: &Path,
    idle_timeout: Duration,
    request_timeout: Duration,
) -> Result<(), DaemonError> {
    serve_with_executor(
        socket_path,
        workspace_root,
        idle_timeout,
        request_timeout,
        Arc::new(ParallelPrimeWarmupExecutor),
    )
}

fn serve_with_executor(
    socket_path: &Path,
    workspace_root: &Path,
    idle_timeout: Duration,
    request_timeout: Duration,
    warmup_executor: Arc<dyn WarmupExecutor>,
) -> Result<(), DaemonError> {
    if socket_path.exists() {
        let remove_started = Instant::now();
        let _ = std::fs::remove_file(socket_path);
        trace_daemon_timing("raql_daemon.remove_stale_socket", remove_started.elapsed());
    }
    let bind_started = Instant::now();
    let listener = UnixListener::bind(socket_path)?;
    trace_daemon_timing("raql_daemon.bind_socket", bind_started.elapsed());
    listener.set_nonblocking(true)?;
    let _socket_guard = SocketGuard {
        path: socket_path.to_path_buf(),
    };
    let session_started = Instant::now();
    let mut session = DaemonWorkspace::from_workspace_root(workspace_root)
        .map_err(|err| DaemonError::Message(format!("failed to initialize rust-analyzer runtime from `{}`: {err}", workspace_root.display())))?;
    trace_daemon_timing("raql_daemon.session_init", session_started.elapsed());
    let mut warmup = WarmupCoordinator::new(warmup_executor);
    let mut plan_cache = PlanCache::default();
    warmup.start_initial(session.analysis_snapshot());
    let poll_interval = Duration::from_millis(ACCEPT_POLL_INTERVAL_MS);
    let mut last_activity = Instant::now();
    let mut cold = true;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(request_timeout))?;
                let cold_before = cold;
                let served_request = handle_connection(
                    stream,
                    &mut session,
                    &mut plan_cache,
                    &mut warmup,
                    cold,
                )?;
                if served_request {
                    cold = false;
                    last_activity = Instant::now();
                }
                trace_daemon_debug(format!(
                    "server.accept served_request={} cold_before={} cold_after={} socket={}",
                    served_request,
                    cold_before,
                    cold,
                    socket_path.display()
                ));
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if last_activity.elapsed() >= idle_timeout {
                    break;
                }
                thread::sleep(poll_interval);
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn trace_daemon_timing(label: &str, elapsed: Duration) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!("raql-timing {label} {}ms", elapsed.as_millis());
}

fn trace_daemon_debug(message: String) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!("raql-timing {message}");
}

struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn handle_connection(
    stream: UnixStream,
    session: &mut DaemonWorkspace,
    plan_cache: &mut PlanCache,
    warmup: &mut WarmupCoordinator,
    cold: bool,
) -> Result<bool, DaemonError> {
    let mut writer = BufWriter::new(stream.try_clone()?);
    let mut reader = BufReader::new(stream);
    match handle_connection_impl(&mut reader, session, plan_cache, warmup, cold, &mut writer) {
        Ok(served_request) => Ok(served_request),
        Err(err) => {
            let _ = emit_event(
                &mut writer,
                &DaemonEvent::Error(ErrorEvent {
                    message: err.to_string(),
                }),
            );
            let _ = emit_event(&mut writer, &DaemonEvent::Done);
            Ok(true)
        }
    }
}

fn handle_connection_impl(
    reader: &mut BufReader<UnixStream>,
    session: &mut DaemonWorkspace,
    plan_cache: &mut PlanCache,
    warmup: &mut WarmupCoordinator,
    cold: bool,
    writer: &mut BufWriter<UnixStream>,
) -> Result<bool, DaemonError> {
    let mut line = String::new();
    let n = match reader.read_line(&mut line) {
        Ok(n) => n,
        Err(err)
            if matches!(err.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
                && line.is_empty() =>
        {
            return Ok(false);
        }
        Err(err) => return Err(err.into()),
    };
    if n == 0 {
        return Ok(false);
    }
    let request: DaemonRequest = serde_json::from_str(line.trim_end())?;
    match request {
        DaemonRequest::Run(run) => handle_run(run, session, plan_cache, warmup, cold, writer)?,
    }
    Ok(true)
}
fn handle_run(
    run: RunRequest,
    session: &mut DaemonWorkspace,
    plan_cache: &mut PlanCache,
    warmup: &mut WarmupCoordinator,
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

    let workspace_epoch_before = session.workspace_epoch();
    session.sync().map_err(|err| DaemonError::Message(err.to_string()))?;
    if session.workspace_epoch() != workspace_epoch_before {
        warmup.restart_after_reload(session.analysis_snapshot());
    }
    let warmup_status = warmup.snapshot();

    emit_event(
        writer,
        &DaemonEvent::Session(SessionEvent {
            workspace_root: session.workspace_root().display().to_string(),
            daemon_state: if cold { DaemonState::Cold } else { DaemonState::Warm },
            warmup_status: protocol_warmup_status(&warmup_status),
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
    let planned = plan_cache
        .load_and_plan(&program_path, &include_dirs)
        .map_err(DaemonError::Message)?;
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PlanCacheKey {
    program_path: Utf8PathBuf,
    include_dirs: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
struct SourceFingerprint {
    path: Utf8PathBuf,
    content_hash: u64,
}

#[derive(Debug, Clone)]
struct PlanCacheEntry {
    planned: PlannedProgram,
    fingerprints: Vec<SourceFingerprint>,
}

#[derive(Debug, Default)]
struct PlanCache {
    entries: BTreeMap<PlanCacheKey, PlanCacheEntry>,
}

impl PlanCache {
    fn load_and_plan(
        &mut self,
        program_path: &Utf8Path,
        include_dirs: &[Utf8PathBuf],
    ) -> Result<PlannedProgram, String> {
        let key = PlanCacheKey {
            program_path: program_path.to_path_buf(),
            include_dirs: include_dirs.to_vec(),
        };
        if let Some(entry) = self.entries.get(&key)
            && fingerprints_match(&entry.fingerprints)?
        {
            return Ok(entry.planned.clone());
        }
        let planned = load_and_plan_uncached(program_path, include_dirs)?;
        let fingerprints = source_fingerprints(planned.source_map())?;
        self.entries.insert(
            key,
            PlanCacheEntry {
                planned: planned.clone(),
                fingerprints,
            },
        );
        Ok(planned)
    }
}

fn load_and_plan_uncached(program_path: &Utf8Path, include_dirs: &[Utf8PathBuf]) -> Result<PlannedProgram, String> {
    let parsed = parse_program_from_file(program_path, include_dirs)
        .map_err(|diags| format!("parse failed for `{program_path}`: {diags:?}"))?;
    let resolved = resolve(parsed).map_err(|diags| format!("resolve failed: {diags:?}"))?;
    let typed = typecheck(resolved).map_err(|diags| format!("typecheck failed: {diags:?}"))?;
    plan(typed).map_err(|diags| format!("plan failed: {diags:?}"))
}

fn source_fingerprints(source_map: &raql_syntax::SourceMap) -> Result<Vec<SourceFingerprint>, String> {
    let mut fingerprints = source_map
        .files()
        .iter()
        .map(|file| SourceFingerprint {
            path: file.path().clone(),
            content_hash: hash_text(file.text()),
        })
        .collect::<Vec<_>>();
    fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(fingerprints)
}

fn fingerprints_match(fingerprints: &[SourceFingerprint]) -> Result<bool, String> {
    for fingerprint in fingerprints {
        let text = std::fs::read_to_string(fingerprint.path.as_std_path())
            .map_err(|err| format!("failed to read `{}`: {err}", fingerprint.path))?;
        if hash_text(&text) != fingerprint.content_hash {
            return Ok(false);
        }
    }
    Ok(true)
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn plan_summary(program_path: &Utf8Path, planned: &PlannedProgram) -> PlanSummary {
    let max_stratum = planned.strata().values().copied().max().unwrap_or(0);
    let strata = if planned.strata().is_empty() { 0 } else { max_stratum + 1 };
    PlanSummary {
        program_path: program_path.as_str().to_string(),
        predicates: planned.predicates().len(),
        facts: planned.facts().len(),
        rules: planned.rules().len(),
        strata,
        sccs: planned.sccs().len(),
    }
}

fn protocol_run_result(result: ProjectedRunResult) -> RunResult {
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

/// Projected values map to the wire vocabulary one-for-one: everything
/// that leaves the host is already plain data (SPEC §13.1), so there is no
/// host-identity variant to carry.
fn protocol_value(value: ProjectedValue) -> ProtocolValue {
    match value {
        ProjectedValue::Int(v) => ProtocolValue::Int(v),
        ProjectedValue::String(v) => ProtocolValue::String(v),
        ProjectedValue::Bool(v) => ProtocolValue::Bool(v),
        ProjectedValue::Enum { name, variant } => ProtocolValue::Enum { name, variant },
        ProjectedValue::None => ProtocolValue::None,
        ProjectedValue::Some(inner) => ProtocolValue::Some(Box::new(protocol_value(*inner))),
        ProjectedValue::List(items) => {
            ProtocolValue::List(items.into_iter().map(protocol_value).collect())
        }
    }
}

fn idle_shutdown_timeout() -> Duration {
    let configured = std::env::var(IDLE_TIMEOUT_ENV).ok();
    idle_shutdown_timeout_from_raw(configured.as_deref())
}

fn idle_shutdown_timeout_from_raw(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS))
}

fn request_read_timeout() -> Duration {
    let configured = std::env::var(REQUEST_TIMEOUT_ENV).ok();
    request_read_timeout_from_raw(configured.as_deref())
}

fn request_read_timeout_from_raw(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS))
}

fn daemon_connect_timeout() -> Duration {
    let configured = std::env::var(CONNECT_TIMEOUT_ENV).ok();
    daemon_connect_timeout_from_raw(configured.as_deref())
}

fn daemon_connect_timeout_from_raw(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use super::{
        DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS, DEFAULT_REQUEST_TIMEOUT_MS,
        DaemonClient, PROTOCOL_VERSION, ParallelPrimeWarmupExecutor, PlanCache, WarmupCoordinator,
        WarmupExecutor, WarmupGeneration, WarmupPhase, connection_failure_message,
        daemon_connect_timeout_from_raw, handle_connection, idle_shutdown_timeout_from_raw,
        request_read_timeout_from_raw, serve_with_executor, serve_with_timeouts, socket_path_for_workspace,
        spawn_or_connect_with_timeout, startup_log_path_for_socket,
        try_acquire_spawn_lock_with_timeout,
    };
    use camino::Utf8PathBuf;
    use raql_host_ra::daemon::{DaemonWorkspace, WarmupSnapshot};
    use raql_protocol::{DaemonEvent, WarmupPhaseEvent};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_query_root(label: &str) -> Utf8PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "raql_daemon_query_{label}_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&root).expect("create query dir");
        Utf8PathBuf::from_path_buf(root).expect("utf8 query root")
    }

    fn temp_workspace_root(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "raql_daemon_{label}_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("Cargo.toml"),
            format!(
                r#"[package]
name = "raql_daemon_{stamp}"
version = "0.0.0"
edition = "2021"
"#
            ),
        )
        .expect("write Cargo.toml");
        fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");
        root
    }

    #[derive(Default)]
    struct DelayedWarmupExecutor {
        delay_ms: u64,
    }

    impl WarmupExecutor for DelayedWarmupExecutor {
        fn spawn(
            &self,
            analysis: WarmupSnapshot,
            generation: WarmupGeneration,
            state: Arc<Mutex<super::WarmupStatus>>,
        ) {
            let delay = self.delay_ms;
            thread::spawn(move || {
                thread::sleep(std::time::Duration::from_millis(delay));
                let result = analysis.parallel_prime_caches(1);
                super::finish_warmup_generation(state, generation, result);
            });
        }
    }

    #[derive(Default)]
    struct ManualWarmupExecutor {
        spawned: Arc<Mutex<Vec<(Arc<Mutex<super::WarmupStatus>>, WarmupGeneration)>>>,
    }

    impl WarmupExecutor for ManualWarmupExecutor {
        fn spawn(
            &self,
            _analysis: WarmupSnapshot,
            generation: WarmupGeneration,
            state: Arc<Mutex<super::WarmupStatus>>,
        ) {
            self.spawned.lock().expect("lock spawned").push((state, generation));
        }
    }

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

    #[test]
    fn idle_shutdown_timeout_uses_default_and_rejects_zero() {
        assert_eq!(
            idle_shutdown_timeout_from_raw(None),
            std::time::Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS)
        );
        assert_eq!(
            idle_shutdown_timeout_from_raw(Some("0")),
            std::time::Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS)
        );
        assert_eq!(
            idle_shutdown_timeout_from_raw(Some("200")),
            std::time::Duration::from_millis(200)
        );
    }

    #[test]
    fn connect_and_close_before_request_does_not_count_as_served_activity() {
        let root = temp_workspace_root("no_request");
        let mut session = DaemonWorkspace::from_workspace_root(root.as_path()).expect("session");
        let mut plan_cache = PlanCache::default();
        let mut warmup = WarmupCoordinator::new(Arc::new(ParallelPrimeWarmupExecutor));
        let (server, client) = UnixStream::pair().expect("stream pair");
        drop(client);

        let served = handle_connection(
            server,
            &mut session,
            &mut plan_cache,
            &mut warmup,
            true,
        )
        .expect("handle connection");
        assert!(
            !served,
            "connect-and-close without a request should not advance warm or idle accounting"
        );
    }

    #[test]
    fn warmup_coordinator_ignores_stale_completion() {
        let root = temp_workspace_root("warmup_generation");
        let session = DaemonWorkspace::from_workspace_root(root.as_path()).expect("session");
        let executor = Arc::new(ManualWarmupExecutor::default());
        let mut coordinator = WarmupCoordinator::new(executor.clone());

        coordinator.start_initial(session.analysis_snapshot());
        assert_eq!(coordinator.snapshot().phase, WarmupPhase::Running);
        let first_generation = coordinator.snapshot().generation;

        coordinator.restart_after_reload(session.analysis_snapshot());
        let second = coordinator.snapshot();
        assert_eq!(second.phase, WarmupPhase::Running);
        assert_ne!(second.generation, first_generation);

        let spawned = executor.spawned.lock().expect("lock spawned");
        let (first_state, first_generation) = spawned[0].clone();
        let (second_state, second_generation) = spawned[1].clone();
        drop(spawned);

        super::finish_warmup_generation(first_state, first_generation, Ok(()));
        let after_stale = coordinator.snapshot();
        assert_eq!(after_stale.phase, WarmupPhase::Running);
        assert_eq!(after_stale.generation, second_generation);

        super::finish_warmup_generation(second_state, second_generation, Ok(()));
        let warmed = coordinator.snapshot();
        assert_eq!(warmed.phase, WarmupPhase::Warm);
        assert_eq!(warmed.generation, second_generation);
    }

    #[test]
    fn plan_cache_reuses_planned_program_when_query_sources_are_unchanged() {
        let query_root = temp_query_root("plan_cache_hit");
        let helper = query_root.join("helper.raql");
        let entry = query_root.join("entry.raql");
        fs::write(&helper, ".decl helper().\nhelper().\n").expect("write helper");
        fs::write(&entry, ".include \"helper.raql\".\n.decl hit().\nhit() :- helper().\n")
            .expect("write entry");

        let mut cache = PlanCache::default();
        let first = cache
            .load_and_plan(entry.as_path(), &[])
            .expect("first load_and_plan");
        let second = cache
            .load_and_plan(entry.as_path(), &[])
            .expect("second load_and_plan");

        assert_eq!(
            first.rules().len(),
            second.rules().len(),
            "cached plan should match the original planned program"
        );
        assert_eq!(cache.entries.len(), 1, "query should occupy one cache entry");
    }

    #[test]
    fn plan_cache_invalidates_when_included_query_file_changes() {
        let query_root = temp_query_root("plan_cache_invalidation");
        let helper = query_root.join("helper.raql");
        let entry = query_root.join("entry.raql");
        fs::write(&helper, ".decl helper().\nhelper().\n").expect("write helper");
        fs::write(&entry, ".include \"helper.raql\".\n.decl hit().\nhit() :- helper().\n")
            .expect("write entry");

        let mut cache = PlanCache::default();
        let first = cache
            .load_and_plan(entry.as_path(), &[])
            .expect("first load_and_plan");
        fs::write(
            &helper,
            ".decl helper().\n.decl extra().\nhelper().\nextra().\n",
        )
        .expect("rewrite helper");
        let second = cache
            .load_and_plan(entry.as_path(), &[])
            .expect("second load_and_plan");

        assert_ne!(
            first.facts().len(),
            second.facts().len(),
            "included-file edits should invalidate the cached plan"
        );
    }

    #[test]
    fn request_read_timeout_uses_default_and_rejects_zero() {
        assert_eq!(
            request_read_timeout_from_raw(None),
            std::time::Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(
            request_read_timeout_from_raw(Some("0")),
            std::time::Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(
            request_read_timeout_from_raw(Some("200")),
            std::time::Duration::from_millis(200)
        );
    }

    #[test]
    fn daemon_connect_timeout_uses_default_and_rejects_zero() {
        assert_eq!(
            daemon_connect_timeout_from_raw(None),
            std::time::Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS)
        );
        assert_eq!(
            daemon_connect_timeout_from_raw(Some("0")),
            std::time::Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS)
        );
        assert_eq!(
            daemon_connect_timeout_from_raw(Some("200")),
            std::time::Duration::from_millis(200)
        );
    }

    #[test]
    fn half_open_client_times_out_without_blocking_next_request() {
        let root = temp_workspace_root("half_open_timeout");
        let query = root.join("query.raql");
        fs::write(&query, ".decl ping().\nping().\n").expect("write query");
        let socket = std::env::temp_dir().join(format!(
            "raql-daemon-half-open-{}_{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));

        let server_root = root.clone();
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            serve_with_timeouts(
                server_socket.as_path(),
                server_root.as_path(),
                std::time::Duration::from_millis(300),
                std::time::Duration::from_millis(100),
            )
        });

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(socket.exists(), "daemon socket should appear");

        let blocker = UnixStream::connect(&socket).expect("connect blocker");
        thread::sleep(std::time::Duration::from_millis(150));

        let mut client = DaemonClient {
            stream: UnixStream::connect(&socket).expect("connect client"),
        };
        let events = client
            .run(
                &Utf8PathBuf::from_path_buf(query.clone()).expect("utf8 query"),
                &[],
            )
            .expect("run query");
        assert!(
            events.iter().any(|event| matches!(event, DaemonEvent::Result(_))),
            "expected a terminal result after half-open client timeout; events={events:?}"
        );

        drop(blocker);
        let server_result = server.join().expect("join server");
        assert!(server_result.is_ok(), "server should exit cleanly; result={server_result:?}");
    }

    #[test]
    fn serve_marks_followup_requests_warm() {
        let root = temp_workspace_root("warm_reuse");
        let query = root.join("query.raql");
        fs::write(&query, ".decl ping() output.\nping().\n").expect("write query");
        let socket = std::env::temp_dir().join(format!(
            "raql-daemon-warm-reuse-{}_{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));

        let server_root = root.clone();
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            serve_with_timeouts(
                server_socket.as_path(),
                server_root.as_path(),
                std::time::Duration::from_millis(300),
                std::time::Duration::from_millis(5_000),
            )
        });

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(socket.exists(), "daemon socket should appear");

        let mut first = DaemonClient {
            stream: UnixStream::connect(&socket).expect("connect first client"),
        };
        let first_events = first
            .run(
                &Utf8PathBuf::from_path_buf(query.clone()).expect("utf8 query"),
                &[],
            )
            .expect("run first query");
        assert!(
            first_events.iter().any(|event| matches!(
                event,
                DaemonEvent::Session(session)
                    if matches!(session.daemon_state, raql_protocol::DaemonState::Cold)
            )),
            "expected first request to report cold daemon state; events={first_events:?}"
        );

        let mut second = DaemonClient {
            stream: UnixStream::connect(&socket).expect("connect second client"),
        };
        let second_events = second
            .run(
                &Utf8PathBuf::from_path_buf(query.clone()).expect("utf8 query"),
                &[],
            )
            .expect("run second query");
        assert!(
            second_events.iter().any(|event| matches!(
                event,
                DaemonEvent::Session(session)
                    if matches!(session.daemon_state, raql_protocol::DaemonState::Warm)
            )),
            "expected follow-up request to report warm daemon state; events={second_events:?}"
        );

        thread::sleep(std::time::Duration::from_millis(350));
        let server_result = server.join().expect("join server");
        assert!(server_result.is_ok(), "server should exit cleanly; result={server_result:?}");
    }

    #[test]
    fn serve_reports_running_warmup_while_still_serving_requests() {
        let root = temp_workspace_root("warmup_running");
        let query = root.join("query.raql");
        fs::write(&query, ".decl ping() output.\nping().\n").expect("write query");
        let socket = std::env::temp_dir().join(format!(
            "raql-warmup-{}_{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));

        let server_root = root.clone();
        let server_socket = socket.clone();
        let executor = Arc::new(DelayedWarmupExecutor { delay_ms: 200 });
        let server = thread::spawn(move || {
            serve_with_executor(
                server_socket.as_path(),
                server_root.as_path(),
                std::time::Duration::from_millis(300),
                std::time::Duration::from_millis(5_000),
                executor,
            )
        });

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(socket.exists(), "daemon socket should appear");

        let mut client = DaemonClient {
            stream: UnixStream::connect(&socket).expect("connect client"),
        };
        let events = client
            .run(
                &Utf8PathBuf::from_path_buf(query.clone()).expect("utf8 query"),
                &[],
            )
            .expect("run query");
        assert!(
            events.iter().any(|event| matches!(
                event,
                DaemonEvent::Session(session)
                    if matches!(session.warmup_status.phase, WarmupPhaseEvent::Running)
            )),
            "expected session to report running warmup; events={events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(event, DaemonEvent::Result(_))),
            "expected daemon to serve request while warmup is running; events={events:?}"
        );

        thread::sleep(std::time::Duration::from_millis(350));
        let server_result = server.join().expect("join server");
        assert!(server_result.is_ok(), "server should exit cleanly; result={server_result:?}");
    }

    #[test]
    fn timed_out_spawn_is_killed_before_lock_release() {
        let root = temp_workspace_root("timed_out_spawn");
        let script_dir = std::env::temp_dir().join(format!(
            "raql-daemon-timeout-script-{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&script_dir).expect("create script dir");
        let marker = script_dir.join("marker.txt");
        let script = script_dir.join("fake-daemon.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 1\necho timed-out > '{}'\n",
                marker.display()
            ),
        )
        .expect("write fake daemon script");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("chmod script");

        let err = spawn_or_connect_with_timeout(
            script.as_path(),
            root.as_path(),
            std::time::Duration::from_millis(100),
        )
        .err()
        .expect("fake daemon should time out");
        assert!(
            err.to_string().contains("failed to connect"),
            "expected connection timeout error; err={err}"
        );

        thread::sleep(std::time::Duration::from_millis(1_200));
        assert!(
            !marker.exists(),
            "timed-out spawn should be killed before it can outlive the spawn lock"
        );
    }

    #[test]
    fn stale_spawn_lock_is_recovered_after_timeout() {
        let lock_path = std::env::temp_dir().join(format!(
            "raql-daemon-stale-lock-{}_{}.lock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::write(&lock_path, "stale").expect("write stale lock");
        thread::sleep(std::time::Duration::from_millis(75));

        let lock = try_acquire_spawn_lock_with_timeout(
            lock_path.as_path(),
            std::time::Duration::from_millis(50),
        )
        .expect("acquire stale lock");
        assert!(lock.is_some(), "stale spawn lock should be recovered");
    }
}
