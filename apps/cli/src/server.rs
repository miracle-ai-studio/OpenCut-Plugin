//! Local process lifecycle only. Editing, authorization and workers remain in the shared host.
//! Locks establish liveness; PID files are informational and are never used to send signals.
use clap::{Args, Subcommand};
use opencut_contracts::EditorError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, EditorError>;

#[derive(Subcommand)]
pub enum Command {
    /// Start in the background; return only after authenticated API and database checks.
    Start(Options),
    /// Check process ownership and authenticated API readiness (exit 1 unless ready).
    Status,
    /// Request graceful shutdown of this exact instance and wait for its lock to release.
    Stop {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout_seconds: u64,
    },
    #[command(hide = true)]
    Run {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        instance: String,
    },
}

#[derive(Args, Clone)]
pub struct Options {
    #[arg(skip)]
    transcription: Option<Transcription>,
    /// JSON service configuration; defaults to server directory/config.json when present.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:8788")]
    listen: SocketAddr,
    #[arg(long)]
    allow_origin: Vec<String>,
    /// Serve a built Web application alongside the authenticated API.
    #[arg(long)]
    web_root: Option<PathBuf>,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Transcription {
    model: PathBuf,
    #[serde(default)]
    executable: Option<PathBuf>,
}
impl Transcription {
    fn inherited() -> Result<Option<Self>> {
        std::env::var_os("OPENCUT_WHISPER_MODEL")
            .filter(|path| !path.is_empty())
            .map(|model| {
                Ok(Self {
                    model: absolute(Path::new(&model))?,
                    executable: std::env::var_os("OPENCUT_WHISPER_CLI")
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from),
                })
            })
            .transpose()
    }
}

#[derive(Deserialize, Serialize)]
struct State {
    version: u32,
    instance: String,
    pid: u32,
    database: PathBuf,
    executable: PathBuf,
    #[serde(default)]
    executable_sha256: Option<String>,
    endpoint: String,
    listen: SocketAddr,
    allow_origin: Vec<String>,
    #[serde(default)]
    web_root: Option<PathBuf>,
    #[serde(default)]
    transcription: Option<Transcription>,
    started_at_unix_ms: u128,
}

pub(crate) struct Runtime {
    directory: PathBuf,
    state: State,
    _lock: File,
}

fn error(e: impl std::fmt::Display) -> EditorError {
    EditorError::new("SERVICE_ERROR", e.to_string())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(error)
}

fn database_identity(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => absolute(path),
        result => result.map_err(error),
    }
}

fn directory(path: Option<&Path>) -> Result<PathBuf> {
    match path {
        Some(path) => absolute(path),
        None => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".opencut/server"))
            .ok_or_else(|| error("HOME is unavailable; specify server --directory")),
    }
}

fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path).map_err(error)?.permissions().mode() & 0o077 != 0 {
            return Err(error(
                "Service directory must be private (mode 0700); choose a dedicated --directory",
            ));
        }
    }
    Ok(())
}

fn open_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(error)
}

fn lock(path: &Path) -> Result<Option<File>> {
    let file = open_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(error(e)),
    }
}

fn running(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    Ok(lock(&path.join("runtime.lock"))?.is_none())
}

fn read_state(path: &Path) -> Result<State> {
    serde_json::from_slice(&fs::read(path.join("state.json")).map_err(error)?).map_err(error)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

impl Runtime {
    pub fn publish(&self, listen: SocketAddr) -> io::Result<()> {
        let mut state = serde_json::to_value(&self.state)?;
        state["endpoint"] = json!(format!("http://{listen}"));
        atomic_write(
            &self.directory.join("state.json"),
            &serde_json::to_vec_pretty(&state)?,
        )
    }
}

pub fn shutdown_requested(
    runtime: Option<&Runtime>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let request = runtime.map(|runtime| {
        (
            runtime.directory.join("stop-request"),
            runtime.state.instance.clone(),
        )
    });
    async move {
        let Some((path, instance)) = request else {
            std::future::pending::<()>().await;
            return;
        };
        loop {
            if fs::read_to_string(&path).is_ok_and(|request| request == instance) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn probe(directory: &Path, state: &State) -> Result<()> {
    let token = fs::read_to_string(directory.join("api-token")).map_err(error)?;
    let client = opencut_sdk::Client::with_options(
        &state.endpoint,
        token.trim(),
        opencut_sdk::Options {
            request_timeout: Duration::from_secs(2),
            max_response_bytes: 4 * 1024 * 1024,
        },
    )
    .map_err(error)?;
    client
        .call::<opencut_sdk::api::tools::Capabilities>(&opencut_sdk::api::EmptyInput {})
        .map_err(error)?;
    // A DB-backed, bounded read distinguishes a static capability response from usable storage.
    client
        .call::<opencut_sdk::api::tools::EventChanges>(
            &opencut_sdk::api::event_stream::EventChangesInput {
                scope: opencut_sdk::api::event_stream::EventScope::Projects { project_id: None },
                cursor: None,
                limit: 1,
            },
        )
        .map_err(error)?;
    Ok(())
}

fn status(directory: &Path) -> Result<Value> {
    if !running(directory)? {
        return Ok(json!({"version":1,"state":"stopped","ready":false,"directory":directory}));
    }
    let state = match read_state(directory) {
        Ok(state) => state,
        Err(_) => {
            return Ok(json!({"version":1,"state":"starting","ready":false,"directory":directory}));
        }
    };
    let health = probe(directory, &state);
    let live = running(directory)?;
    let stopping = fs::read_to_string(directory.join("stop-request"))
        .is_ok_and(|request| request == state.instance);
    let ready = live && !stopping && health.is_ok();
    Ok(json!({
        "version":1,"state":if !live {"stopped"} else if stopping {"stopping"} else if ready {"running"} else {"unhealthy"},
        "ready":ready,"pid":state.pid,"instance":state.instance,
        "endpoint":state.endpoint,"web_url":state.web_root.as_ref().map(|_|format!("{}/editor",state.endpoint)),"database":state.database,"executable":state.executable,"executable_sha256":state.executable_sha256,
        "started_at_unix_ms":state.started_at_unix_ms,"directory":directory,
        "token_file":directory.join("api-token"),"log_file":directory.join("server.log"),
        "diagnostic":health.err().map(|e|e.message),
    }))
}

/// Attach a local MCP transport without acquiring service lifecycle ownership.
/// Validate before opening a workspace so a stale plugin cannot create or select
/// another database. Separate copies of the same executable are accepted.
pub(crate) fn require_ready(database: &Path, directory: &Path) -> Result<()> {
    let state = status(directory)?;
    if state["ready"] != true {
        return Err(EditorError::new(
            "SERVICE_NOT_READY",
            "The independent OpenCut service is not ready. Start the selected service before connecting this plugin.",
        ));
    }
    let service_database = state["database"]
        .as_str()
        .ok_or_else(|| error("Service database is missing"))?;
    if fs::canonicalize(database).map_err(error)?
        != fs::canonicalize(service_database).map_err(error)?
    {
        return Err(EditorError::new(
            "SERVICE_TARGET_MISMATCH",
            "The plugin workspace differs from the running service. Regenerate the connection for the intended workspace.",
        ));
    }
    let own_digest = executable_digest(&std::env::current_exe().map_err(error)?)?;
    if state["executable_sha256"].as_str() != Some(own_digest.as_str()) {
        return Err(EditorError::new(
            "SERVICE_CANDIDATE_MISMATCH",
            "The plugin build does not match the service's recorded startup build. Start the independent service with this OpenCut build before connecting.",
        ));
    }
    Ok(())
}

fn executable_digest(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut file = File::open(path).map_err(error)?;
    std::io::copy(&mut file, &mut hash).map_err(error)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn start(directory: &Path, database: &Path, options: &Options) -> Result<Value> {
    let mut transcription = options
        .transcription
        .clone()
        .or(Transcription::inherited()?);
    if let Some(config) = &mut transcription {
        config.model = absolute(&config.model)?;
        if !config.model.is_file() {
            return Err(error(
                "Configured transcription model must be a regular file",
            ));
        }
    }
    if !options.listen.ip().is_loopback() {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Service only accepts loopback addresses",
        ));
    }
    if running(directory)? {
        let existing = read_state(directory)?;
        if database_identity(&existing.database)? != database_identity(database)?
            || existing.listen != options.listen
            || existing.allow_origin != options.allow_origin
            || existing.web_root != options.web_root.as_deref().map(absolute).transpose()?
            || existing.transcription != transcription
        {
            return Err(error(
                "This service directory already manages a different configuration; stop it first or use another --directory",
            ));
        }
        let value = status(directory)?;
        return if value["ready"] == true {
            Ok(value)
        } else {
            Err(error(format!("Existing service is not ready: {value}")))
        };
    }
    let _ = fs::remove_file(directory.join("state.json"));
    let instance = uuid::Uuid::new_v4().to_string();
    let executable = std::env::current_exe().map_err(error)?;
    let log = open_file(&directory.join("server.log"))?;
    let mut child = Process::new(executable);
    child
        .arg("--database")
        .arg(absolute(database)?)
        .arg("server")
        .arg("--directory")
        .arg(directory)
        .arg("run")
        .arg("--instance")
        .arg(&instance)
        .arg("--listen")
        .arg(options.listen.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(error)?))
        .stderr(Stdio::from(log));
    if let Some(root) = &options.web_root {
        child.arg("--web-root").arg(absolute(root)?);
    }
    for origin in &options.allow_origin {
        child.arg("--allow-origin").arg(origin);
    }
    if let Some(config) = transcription {
        child.env("OPENCUT_WHISPER_MODEL", config.model);
        if let Some(executable) = config.executable {
            child.env("OPENCUT_WHISPER_CLI", executable);
        } else {
            child.env_remove("OPENCUT_WHISPER_CLI");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group, no terminal or inherited output pipes. No shell intermediary.
        child.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        child.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = child.spawn().map_err(error)?;
    let deadline = Instant::now() + Duration::from_secs(options.timeout_seconds);
    loop {
        if let Some(exit) = child.try_wait().map_err(error)? {
            return Err(error(format!(
                "Service exited with {exit}; inspect {}",
                directory.join("server.log").display()
            )));
        }
        if read_state(directory).is_ok_and(|state| state.instance == instance) {
            let value = status(directory)?;
            if value["ready"] == true {
                return Ok(value);
            }
        }
        if Instant::now() >= deadline {
            return Err(error(format!(
                "Startup readiness timed out; process may still be starting. Use server status/stop; log: {}",
                directory.join("server.log").display()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop(directory: &Path, timeout: u64) -> Result<Value> {
    if !running(directory)? {
        return status(directory);
    }
    let state = read_state(directory)?;
    atomic_write(&directory.join("stop-request"), state.instance.as_bytes()).map_err(error)?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while running(directory)? {
        if Instant::now() >= deadline {
            return Err(error(
                "Shutdown timed out; graceful shutdown remains requested. Check status/logs; no force-kill was sent",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    status(directory)
}

pub async fn execute(
    database: &Path,
    path: Option<&Path>,
    command: &Command,
    require_database: bool,
) -> Result<Value> {
    let directory = directory(path)?;
    if let Command::Run { options, instance } = command {
        private_directory(&directory)?;
        let lock = lock(&directory.join("runtime.lock"))?
            .ok_or_else(|| error("Service already running"))?;
        let executable = std::env::current_exe().map_err(error)?;
        let executable_sha256 = Some(executable_digest(&executable)?);
        let runtime = Runtime {
            state: State {
                version: 1,
                instance: instance.clone(),
                pid: std::process::id(),
                database: absolute(database)?,
                executable,
                executable_sha256,
                endpoint: format!("http://{}", options.listen),
                listen: options.listen,
                allow_origin: options.allow_origin.clone(),
                web_root: options.web_root.as_deref().map(absolute).transpose()?,
                transcription: Transcription::inherited()?,
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(error)?
                    .as_millis(),
            },
            directory: directory.clone(),
            _lock: lock,
        };
        crate::http::serve(
            &runtime.state.database,
            options.listen,
            &directory.join("api-token"),
            crate::http::Options {
                origins: options.allow_origin.clone(),
                mcp_session_timeout_seconds: 300,
                tool_surface: crate::mcp::ToolSurface::Domains,
                web_root: runtime.state.web_root.clone(),
            },
            Some(&runtime),
        )
        .await
        .map_err(error)?;
        return Ok(json!({"version":1,"state":"stopped","ready":false}));
    }
    if !directory.exists() && !matches!(command, Command::Start(_)) {
        return status(&directory);
    }
    private_directory(&directory)?;
    let _command = lock(&directory.join("command.lock"))?
        .ok_or_else(|| error("Another service lifecycle command is in progress; retry"))?;
    if require_database
        && matches!(command, Command::Status | Command::Stop { .. })
        && running(&directory)?
        && let Ok(state) = read_state(&directory)
        && database_identity(&state.database)? != database_identity(database)?
    {
        return Err(EditorError::new(
            "SERVICE_TARGET_MISMATCH",
            "The running service uses a different database; specify its original database to manage it.",
        ));
    }
    match command {
        Command::Start(options) => {
            let path = options
                .config
                .clone()
                .unwrap_or_else(|| directory.join("config.json"));
            let mut resolved = options.clone();
            if path.exists() || options.config.is_some() {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Config {
                    listen: SocketAddr,
                    web_root: PathBuf,
                    #[serde(default)]
                    allow_origin: Vec<String>,
                    #[serde(default)]
                    transcription: Option<Transcription>,
                }
                let config: Config =
                    serde_json::from_slice(&fs::read(&path).map_err(error)?).map_err(error)?;
                resolved.listen = config.listen;
                resolved.web_root = Some(if config.web_root.is_absolute() {
                    config.web_root
                } else {
                    path.parent()
                        .unwrap_or(Path::new("."))
                        .join(config.web_root)
                });
                resolved.allow_origin = config.allow_origin;
                resolved.transcription = config.transcription.map(|mut transcription| {
                    if !transcription.model.is_absolute() {
                        transcription.model = path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .join(transcription.model);
                    }
                    if let Some(executable) = &mut transcription.executable
                        && !executable.is_absolute()
                        && executable.components().count() > 1
                    {
                        *executable = path.parent().unwrap_or(Path::new(".")).join(&*executable);
                    }
                    transcription
                });
            }
            start(&directory, database, &resolved)
        }
        Command::Status => status(&directory),
        Command::Stop { timeout_seconds } => stop(&directory, *timeout_seconds),
        Command::Run { .. } => unreachable!(),
    }
}
