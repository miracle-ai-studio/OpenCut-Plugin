mod access;
mod audio_preview_http;
mod blocking_owned;
mod connection;
mod denials;
mod event_streams;
mod http;
mod http_identity;
mod job_wait;
mod mcp;
mod mcp_context;
mod mcp_http;
mod mcp_replay;
mod mcp_sessions;
mod media_http;
mod preview_http;
mod server;
mod signals;
mod storage_recovery;
mod upload_cli;
mod upload_http;
mod visual_preview_http;
mod web_host;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, parser::ValueSource};
use opencut_automation::{Response, capabilities, exit_code, tool_specs};
use opencut_contracts::EditorError;
use opencut_host_runtime::Automation;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "opencut",
    version,
    about = "Transactional video editing for people, scripts and agents"
)]
struct Cli {
    /// Override the shared local workspace for this command.
    #[arg(
        long,
        global = true,
        default_value = ".opencut/workspace.sqlite",
        hide_default_value = true
    )]
    database: PathBuf,
    /// Use a restricted principal credential; default local invocation is OS-owner authority.
    #[arg(long, global = true)]
    credential_file: Option<PathBuf>,
    /// Send business commands to a long-lived HTTP host; --credential-file supplies its bearer token.
    #[arg(long, global = true)]
    endpoint: Option<String>,
    /// Emit a single JSON envelope. Progress and diagnostics never use stdout.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage an independent local HTTP/MCP service and its worker.
    Server {
        /// Runtime files; defaults to ~/.opencut/server, independent of the current directory.
        #[arg(long, global = true)]
        directory: Option<PathBuf>,
        #[command(subcommand)]
        command: server::Command,
    },
    /// Local owner recovery of durable storage policy without opening SQLite.
    Storage {
        #[command(subcommand)]
        command: storage_recovery::Command,
    },
    Upload {
        #[command(subcommand)]
        command: upload_cli::Command,
    },
    Access {
        #[command(subcommand)]
        command: access::Command,
    },
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    Render {
        #[command(subcommand)]
        command: RenderCommand,
    },
    /// Start an authenticated local HTTP host for the shared editor API.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8788")]
        listen: std::net::SocketAddr,
        #[arg(long, default_value = ".opencut/api-token")]
        token_file: PathBuf,
        /// Serve the built Web application from this directory.
        #[arg(long)]
        web_root: Option<PathBuf>,
        #[arg(long)]
        allow_origin: Vec<String>,
        #[arg(long, default_value_t=300, value_parser=clap::value_parser!(u64).range(1..=3600))]
        mcp_session_timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = mcp::ToolSurface::Domains)]
        tool_surface: mcp::ToolSurface,
    },
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Edit {
        #[command(subcommand)]
        command: EditCommand,
    },
    Events {
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        project_id: Option<String>,
        #[command(flatten)]
        stream: event_streams::Options,
    },
    /// Invoke a registered operation using the same JSON arguments as MCP.
    Call {
        name: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// Invoke an action by domain; use `system domains` to discover actions.
    Domain {
        domain: String,
        action: String,
        #[arg(long)]
        input: PathBuf,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Subcommand)]
enum SystemCommand {
    Capabilities,
    Schema,
    /// Complete action catalog; add --domain for one domain and --schemas for exact contracts.
    Domains {
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        schemas: bool,
    },
    /// JSON Schema for versioned CLI NDJSON records.
    EventsSchema,
    /// Shared request and frame-header schemas for the binary audio stream.
    AudioPreviewSchema,
    /// Shared request and descriptor schemas for native RGBA picture bytes.
    VisualPreviewSchema,
}

#[derive(Subcommand)]
enum WorkerCommand {
    Run {
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
enum JobCommand {
    Start {
        #[arg(long)]
        input: PathBuf,
    },
    Get {
        job_id: String,
    },
    /// Wait for a durable job; failed/cancelled jobs and timeout exit nonzero.
    Wait {
        job_id: String,
        #[arg(long, default_value_t = 300)]
        timeout_seconds: u64,
        /// Request durable cancellation only if this wait is interrupted.
        #[arg(long)]
        cancel_on_interrupt: bool,
    },
    List,
    Cancel {
        job_id: String,
    },
    Events {
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        job_id: Option<String>,
        #[command(flatten)]
        stream: event_streams::Options,
    },
}

#[derive(Subcommand)]
enum RenderCommand {
    Preflight {
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    List,
    Inspect {
        id: String,
        version: String,
        #[arg(long,value_parser=["wasm","presets"])]
        kind: String,
    },
    Install {
        #[arg(long)]
        input: PathBuf,
    },
    Plan {
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    History {
        project_id: String,
        #[arg(long)]
        from_revision: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Revision {
        project_id: String,
        revision: String,
    },
    Restore {
        project_id: String,
        #[arg(long)]
        input: PathBuf,
    },
    Create {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        idempotency_key: String,
    },
    List {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Inspect {
        project_id: String,
    },
}

#[derive(Subcommand)]
enum EditCommand {
    Plan {
        #[arg(long)]
        project: String,
        #[arg(long)]
        input: PathBuf,
    },
    Apply {
        #[arg(long)]
        project: String,
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    Serve {
        /// Run a local durable Worker with this stdio host (owner only).
        #[arg(long)]
        with_worker: bool,
        /// Bind this stdio connection to the same workspace and executable as an existing independent service.
        #[arg(long, conflicts_with = "with_worker")]
        service_directory: Option<PathBuf>,
        #[arg(long, default_value = "stdio", value_parser = ["stdio","streamable-http"])]
        transport: String,
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        allow_origin: Vec<String>,
        #[arg(long,value_parser=clap::value_parser!(u64).range(1..=3600))]
        session_timeout_seconds: Option<u64>,
        #[arg(long, value_enum, default_value_t = mcp::ToolSurface::Domains)]
        tool_surface: mcp::ToolSurface,
    },
}

fn shared_workspace() -> Result<(PathBuf, Option<PathBuf>), EditorError> {
    #[derive(serde::Deserialize)]
    struct Installation {
        format: String,
        runtime: PathBuf,
        #[serde(default)]
        database: Option<PathBuf>,
    }

    let error = |message: &str| EditorError::new("INVALID_ARGUMENT", message);
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let plugin_data = std::env::var_os("OPENCUT_PLUGIN_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".opencut-plugin")));
    if let Some(directory) = plugin_data {
        let directory = std::path::absolute(directory)
            .map_err(|_| error("Invalid OpenCut plugin data directory"))?;
        match std::fs::read(directory.join("install.json")) {
            Ok(bytes) => {
                let install: Installation = serde_json::from_slice(&bytes)
                    .map_err(|_| error("Invalid OpenCut plugin installation configuration"))?;
                let database = install
                    .database
                    .unwrap_or_else(|| directory.join("workspace.sqlite"));
                if install.format != "opencut.plugin-install.v1"
                    || !install.runtime.is_absolute()
                    || !database.is_absolute()
                {
                    return Err(error("Invalid OpenCut plugin installation configuration"));
                }
                return Ok((database, Some(directory.join("server"))));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(error(
                    "Could not read OpenCut plugin installation configuration",
                ));
            }
        }
    }
    Ok((
        home.ok_or_else(|| error("HOME is unavailable; specify --database"))?
            .join(".opencut/workspace.sqlite"),
        None,
    ))
}

#[tokio::main]
async fn main() {
    let (mut cli, database_explicit) = match Cli::command().try_get_matches().and_then(|matches| {
        let explicit = matches.value_source("database") == Some(ValueSource::CommandLine);
        Cli::from_arg_matches(&matches).map(|cli| (cli, explicit))
    }) {
        Ok(parsed) => parsed,
        Err(error) => {
            let args: Vec<_> = std::env::args_os().collect();
            let ndjson = args.iter().any(|arg| arg == "--format=ndjson")
                || args
                    .windows(2)
                    .any(|pair| pair[0] == "--format" && pair[1] == "ndjson");
            if error.use_stderr() && ndjson {
                use opencut_automation::event_stream::{StreamRecord, StreamRecordData};
                let record = StreamRecord::new(StreamRecordData::Error {
                    cursor: None,
                    error: EditorError::new("INVALID_ARGUMENT", error.to_string()),
                });
                use std::io::Write;
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    serde_json::to_string(&record).expect("serializable error")
                );
                std::process::exit(2);
            }
            if error.use_stderr() && args.iter().any(|arg| arg == "--json") {
                let response = opencut_automation::response(Err(EditorError::new(
                    "INVALID_ARGUMENT",
                    error.to_string(),
                )));
                println!(
                    "{}",
                    serde_json::to_string(&response).expect("serializable error")
                );
                std::process::exit(2);
            }
            error.exit();
        }
    };
    if let Some(endpoint) = &cli.endpoint {
        emit_response(remote_call(&cli, endpoint).await, cli.json);
        return;
    }
    let shared_service = if database_explicit {
        None
    } else {
        match shared_workspace() {
            Ok((database, service)) => {
                cli.database = database;
                service
            }
            Err(error) => {
                emit_response(opencut_automation::response(Err(error)), cli.json);
                return;
            }
        }
    };
    if let Command::Server { directory, command } = &cli.command {
        // Service administration is local OS-owner authority, never a scoped API credential.
        let result = if cli.credential_file.is_some() {
            Err(EditorError::new(
                "PERMISSION_DENIED",
                "Service administration requires local OS-owner authority; omit --credential-file",
            ))
        } else {
            let selected_directory = directory.as_deref().or(shared_service.as_deref());
            let require_database = database_explicit || directory.is_none();
            server::execute(&cli.database, selected_directory, command, require_database).await
        };
        let unhealthy = matches!(command, server::Command::Status)
            && result.as_ref().is_ok_and(|value| value["ready"] != true);
        emit_response(opencut_automation::response(result), cli.json);
        if unhealthy {
            std::process::exit(1);
        }
        return;
    }
    if let Command::Storage { command } = &cli.command {
        // Ordinary authentication and denial journaling both open SQLite. A
        // recovery entry must remain reachable when SQLite cannot allocate.
        emit_response(
            storage_recovery::execute(&cli.database, cli.credential_file.as_deref(), command),
            cli.json,
        );
        return;
    }
    if let Command::Mcp {
        command:
            McpCommand::Serve {
                service_directory: Some(directory),
                transport,
                ..
            },
    } = &cli.command
    {
        let ready = if transport == "stdio" {
            server::require_ready(&cli.database, directory)
        } else {
            Err(EditorError::new(
                "INVALID_ARGUMENT",
                "--service-directory attaches stdio to an existing service; it cannot start an HTTP host",
            ))
        };
        if let Err(error) = ready {
            eprintln!("{}: {}", error.code, error.message);
            std::process::exit(exit_code(&error));
        }
    }
    let caller = match access::caller(&cli.database, cli.credential_file.as_deref()) {
        Ok(caller) => caller,
        Err(error) => {
            let code = exit_code(&error);
            println!(
                "{}",
                serde_json::to_string(&denials::response(
                    &cli.database,
                    None,
                    opencut_automation::CallSource::Cli,
                    opencut_automation::denials::DeniedOperation::Authentication,
                    opencut_automation::denials::DenialReason::AuthenticationRejected,
                    opencut_automation::response(Err(error))
                ))
                .expect("serializable error")
            );
            std::process::exit(code);
        }
    };
    if !caller.is_owner()
        && (matches!(&cli.command, Command::Worker { .. } | Command::Serve { .. })
            || matches!(&cli.command, Command::Mcp {command:McpCommand::Serve {transport,with_worker,..}} if transport == "streamable-http" || *with_worker))
    {
        // The network host still uses the separately configured owner bearer.
        println!(
            "{}",
            serde_json::to_string(&denials::response(
                &cli.database,
                Some(&caller),
                opencut_automation::CallSource::Cli,
                opencut_automation::denials::DeniedOperation::HostStart,
                opencut_automation::denials::DenialReason::AuthorizationDenied,
                opencut_automation::response(Err(opencut_editor_application::access::denied()))
            ))
            .expect("serializable error")
        );
        std::process::exit(3);
    }
    if let Some((scope, limit, options)) = stream_request(&cli.command) {
        observe_events(
            connection::Connection::Local {
                database: cli.database.clone(),
                caller: caller.clone(),
            },
            scope,
            limit,
            options,
        )
        .await;
    }
    if let Command::Worker {
        command: WorkerCommand::Run { once },
    } = &cli.command
    {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = stop.clone();
        let database = cli.database.clone();
        let once = *once;
        let mut worker = tokio::task::spawn_blocking(move || {
            opencut_host_runtime::worker::run(&database, once, worker_stop)
        });
        let outcome = tokio::select! {
            result=&mut worker=>result,
            _=tokio::signal::ctrl_c()=>{
                stop.store(true,std::sync::atomic::Ordering::SeqCst);
                worker.await
            }
        };
        match outcome.unwrap_or_else(|e| Err(EditorError::new("INTERNAL_ERROR", e.to_string()))) {
            Ok(()) => return,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(8);
            }
        }
    }
    if let Command::Serve {
        listen,
        token_file,
        allow_origin,
        mcp_session_timeout_seconds,
        tool_surface,
        web_root,
    } = &cli.command
    {
        if let Err(error) = http::serve(
            &cli.database,
            *listen,
            token_file,
            http::Options {
                origins: allow_origin.clone(),
                mcp_session_timeout_seconds: *mcp_session_timeout_seconds,
                tool_surface: *tool_surface,
                web_root: web_root.clone(),
            },
            None,
        )
        .await
        {
            eprintln!("{error}");
            std::process::exit(8);
        }
        return;
    }
    if let Command::Mcp {
        command:
            McpCommand::Serve {
                with_worker,
                service_directory: _,
                transport,
                listen,
                token_file,
                allow_origin,
                session_timeout_seconds,
                tool_surface,
            },
    } = &cli.command
    {
        if transport == "stdio"
            && (listen.is_some()
                || token_file.is_some()
                || !allow_origin.is_empty()
                || session_timeout_seconds.is_some())
        {
            let response = opencut_automation::response(Err(EditorError::new(
                "INVALID_ARGUMENT",
                "HTTP options require --transport streamable-http",
            )));
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&response).expect("serializable error")
                );
            } else {
                eprintln!("HTTP options require --transport streamable-http");
            }
            std::process::exit(2);
        }
        let result = if transport == "streamable-http" {
            http::serve(
                &cli.database,
                listen.unwrap_or_else(|| "127.0.0.1:8788".parse().expect("valid address")),
                token_file
                    .as_deref()
                    .unwrap_or(std::path::Path::new(".opencut/api-token")),
                http::Options {
                    origins: allow_origin.clone(),
                    mcp_session_timeout_seconds: session_timeout_seconds.unwrap_or(300),
                    tool_surface: *tool_surface,
                    web_root: None,
                },
                None,
            )
            .await
        } else {
            mcp::serve(&cli.database, caller.clone(), *tool_surface, *with_worker).await
        };
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(8);
        }
        // Stdio peer closure must not wait on an unread Tokio stdout helper.
        std::process::exit(0);
    }
    let result = if let Command::Job {
        command:
            JobCommand::Wait {
                job_id,
                timeout_seconds,
                cancel_on_interrupt,
            },
    } = &cli.command
    {
        job_wait::run(
            connection::Connection::Local {
                database: cli.database.clone(),
                caller: caller.clone(),
            },
            job_id,
            *timeout_seconds,
            *cancel_on_interrupt,
        )
        .await
    } else {
        execute(&cli, caller.clone())
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => opencut_automation::response(Err(error)),
    };
    let response = denials::response(
        &cli.database,
        Some(&caller),
        opencut_automation::CallSource::Cli,
        opencut_automation::denials::DeniedOperation::CliRequest,
        opencut_automation::denials::DenialReason::AuthorizationDenied,
        response,
    );
    emit_response(response, cli.json);
}

fn emit_response(response: Response, json: bool) {
    let code = response
        .error
        .as_ref()
        .map(|error| {
            if error.code == "USER_INTERRUPTED" {
                130
            } else {
                exit_code(error)
            }
        })
        .unwrap_or(0);
    let output = if json {
        serde_json::to_string(&response)
    } else {
        serde_json::to_string_pretty(&response)
    };
    match output {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("Unable to serialize response: {error}");
            std::process::exit(8);
        }
    }
    if code != 0 {
        std::process::exit(code);
    }
}

fn stream_request(
    command: &Command,
) -> Option<(
    opencut_automation::event_stream::EventScope,
    usize,
    event_streams::Options,
)> {
    match command {
        Command::Events {
            limit,
            project_id,
            stream,
        } if stream.format.is_some() => Some((
            opencut_automation::event_stream::EventScope::Projects {
                project_id: project_id.clone(),
            },
            *limit,
            stream.clone(),
        )),
        Command::Job {
            command:
                JobCommand::Events {
                    limit,
                    job_id,
                    stream,
                },
        } if stream.format.is_some() => Some((
            opencut_automation::event_stream::EventScope::Jobs {
                job_id: job_id.clone(),
            },
            *limit,
            stream.clone(),
        )),
        _ => None,
    }
}

async fn observe_events(
    source: connection::Connection,
    scope: opencut_automation::event_stream::EventScope,
    limit: usize,
    options: event_streams::Options,
) {
    let timeout = options.timeout_seconds;
    let deadline = async move {
        if let Some(seconds) = timeout {
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let code = tokio::select! {
        result=event_streams::run(source,scope,limit,options)=>result,
        _=tokio::signal::ctrl_c()=>{eprintln!("Event observation interrupted; durable jobs were not cancelled");130},
        _=deadline=>{eprintln!("Event observation timed out; resume from the last fully processed cursor");124},
    };
    std::process::exit(code);
}

fn execute(cli: &Cli, caller: opencut_automation::Caller) -> Result<Response, EditorError> {
    if let Command::Upload { command } = &cli.command {
        return upload_cli::execute(&cli.database, caller, command);
    }
    if let Command::Access { command } = &cli.command {
        return access::execute(&cli.database, caller, command);
    }
    match &cli.command {
        Command::System {
            command: SystemCommand::Capabilities,
        } => return Ok(opencut_automation::response(Ok(capabilities()))),
        Command::System {
            command: SystemCommand::AudioPreviewSchema,
        } => {
            return Ok(opencut_automation::response(Ok(
                opencut_host_runtime::audio_preview_contracts::stream_schemas(),
            )));
        }
        Command::System {
            command: SystemCommand::VisualPreviewSchema,
        } => {
            return Ok(opencut_automation::response(Ok(
                opencut_host_runtime::visual_preview_contracts::picture_stream_schemas(),
            )));
        }
        Command::System {
            command: SystemCommand::EventsSchema,
        } => {
            return Ok(opencut_automation::response(Ok(
                opencut_automation::event_stream::schema(),
            )));
        }
        Command::System {
            command: SystemCommand::Schema,
        } => return Ok(opencut_automation::response(Ok(json!(tool_specs())))),
        Command::System {
            command: SystemCommand::Domains { domain, schemas },
        } => {
            return Ok(opencut_automation::response(Ok(json!(
                opencut_automation::domains::catalog(domain.as_deref(), *schemas)?
            ))));
        }
        _ => {}
    }
    let (name, arguments) = command_request(&cli.command)?;
    let mut automation = Automation::open(&cli.database)?
        .with_source(opencut_automation::CallSource::Cli)
        .with_caller(caller);
    Ok(automation.call(&name, arguments))
}

fn command_request(command: &Command) -> Result<(String, Value), EditorError> {
    let (name, arguments) = match command {
        Command::Render { command } => match command {
            RenderCommand::Preflight { input } => ("opencut_render_preflight", read_json(input)?),
        },
        Command::Plugin { command } => match command {
            PluginCommand::List => ("opencut_plugin_list", json!({})),
            PluginCommand::Inspect { id, version, kind } => (
                "opencut_plugin_inspect",
                json!({"kind":kind,"id":id,"version":version}),
            ),
            PluginCommand::Install { input } => ("opencut_plugin_install", read_json(input)?),
            PluginCommand::Plan { input } => ("opencut_project_plugin_plan", read_json(input)?),
        },
        Command::Job { command } => match command {
            JobCommand::Start { input } => ("opencut_job_submit", read_json(input)?),
            JobCommand::Get { job_id } => ("opencut_job_view", json!({"job_id":job_id})),
            JobCommand::Wait { .. } => unreachable!("wait handled before ordinary dispatch"),
            JobCommand::List => ("opencut_job_query", json!({})),
            JobCommand::Cancel { job_id } => {
                ("opencut_job_request_cancel", json!({"job_id":job_id}))
            }
            JobCommand::Events {
                limit,
                job_id,
                stream,
            } => (
                "opencut_job_changes",
                json!({"cursor":stream.cursor,"limit":limit,"job_id":job_id}),
            ),
        },
        Command::Project { command } => match command {
            ProjectCommand::History {
                project_id,
                from_revision,
                limit,
            } => (
                "opencut_project_history",
                json!({"project_id":project_id,"from_revision":from_revision,"limit":limit}),
            ),
            ProjectCommand::Revision {
                project_id,
                revision,
            } => (
                "opencut_project_inspect",
                json!({"project_id":project_id,"revision":revision}),
            ),
            ProjectCommand::Restore { project_id, input } => (
                "opencut_project_restore",
                scoped_project_input(&project_id, read_json(input)?)?,
            ),
            ProjectCommand::Create {
                input,
                idempotency_key,
            } => (
                "opencut_project_create",
                json!({"project":read_json(input)?, "idempotency_key":idempotency_key}),
            ),
            ProjectCommand::List { cursor, limit } => (
                "opencut_project_list",
                json!({"cursor":cursor,"limit":limit}),
            ),
            ProjectCommand::Inspect { project_id } => {
                ("opencut_project_inspect", json!({"project_id":project_id}))
            }
        },
        Command::Edit { command } => match command {
            EditCommand::Plan { project, input } => (
                "opencut_edit_plan",
                scoped_project_input(project, read_json(input)?)?,
            ),
            EditCommand::Apply { project, input } => (
                "opencut_edit_apply",
                scoped_project_input(project, read_json(input)?)?,
            ),
        },
        Command::Events {
            limit,
            project_id,
            stream,
        } => (
            "opencut_event_changes",
            json!({"scope":{"kind":"projects","project_id":project_id},"cursor":stream.cursor,"limit":limit}),
        ),
        Command::Call { name, input } => (name.as_str(), read_json(input)?),
        Command::Domain {
            domain,
            action,
            input,
        } => {
            let name = format!("opencut_{domain}");
            if !opencut_automation::domains::is_domain_tool(&name) {
                return Err(EditorError::new(
                    "INVALID_ARGUMENT",
                    "Unknown operation domain",
                ));
            }
            let arguments = json!({"action":action,"input":read_json(input)?});
            return Ok((name, arguments));
        }
        _ => {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "This command manages a local host; it cannot use --endpoint",
            ));
        }
    };
    Ok((name.to_owned(), arguments))
}

async fn remote_call(cli: &Cli, endpoint: &str) -> Response {
    let client = match connection::remote(endpoint, cli.credential_file.as_deref()) {
        Ok(client) => client,
        Err(error) => return opencut_automation::response(Err(error)),
    };
    let source = connection::Connection::Remote(client.clone());
    if let Some((scope, limit, options)) = stream_request(&cli.command) {
        observe_events(source, scope, limit, options).await;
        unreachable!("stream observation exits");
    }
    if let Command::Job {
        command:
            JobCommand::Wait {
                job_id,
                timeout_seconds,
                cancel_on_interrupt,
            },
    } = &cli.command
    {
        return match job_wait::run(source, job_id, *timeout_seconds, *cancel_on_interrupt).await {
            Ok(response) => response,
            Err(error) => opencut_automation::response(Err(error)),
        };
    }
    if let Command::Upload { command } = &cli.command {
        return upload_cli::execute_remote(&client, command)
            .unwrap_or_else(|e| opencut_automation::response(Err(e)));
    }
    if let Command::System { command } = &cli.command {
        return remote_system(&source, &client, command)
            .unwrap_or_else(|e| opencut_automation::response(Err(e)));
    }
    match command_request(&cli.command) {
        Ok((name, input)) => source.call(&name, input),
        Err(error) => opencut_automation::response(Err(error)),
    }
}

fn remote_system(
    source: &connection::Connection,
    client: &opencut_sdk::Client,
    command: &SystemCommand,
) -> Result<Response, EditorError> {
    use opencut_automation::context::{ResourcePayload, ResourceView};
    let resource = |uri: &str| -> Result<ResourcePayload, EditorError> {
        let view: ResourceView = source.data("opencut_resource_read", json!({"uri":uri}))?;
        Ok(view.payload)
    };
    let mismatch = || {
        EditorError::new(
            "INVALID_RESPONSE",
            "Host returned a different discovery resource",
        )
    };
    let data = match command {
        SystemCommand::Capabilities => return Ok(source.call("opencut_capabilities", json!({}))),
        SystemCommand::Schema => return Ok(connection::response(client.operation_schemas())),
        SystemCommand::EventsSchema => match resource("opencut://schemas/event-stream")? {
            ResourcePayload::EventSchema(schema) => schema,
            _ => return Err(mismatch()),
        },
        SystemCommand::AudioPreviewSchema | SystemCommand::VisualPreviewSchema => {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "Audio stream schema discovery is local; use the connected host render domain for preview operations",
            ));
        }
        SystemCommand::Domains { domain, schemas } => {
            let mut result = match client.domain_catalog(domain.as_deref()) {
                Ok(result) => result,
                Err(error) => return Ok(connection::response::<Value>(Err(error))),
            };
            if *schemas && domain.is_none() {
                let mut complete = Vec::new();
                for descriptor in &result.data {
                    match client.domain_catalog(Some(&descriptor.domain)) {
                        Ok(mut detail) => {
                            complete.append(&mut detail.data);
                            result.warnings.append(&mut detail.warnings);
                        }
                        Err(error) => return Ok(connection::response::<Value>(Err(error))),
                    }
                }
                result.data = complete;
            }
            if !schemas {
                for d in &mut result.data {
                    for a in &mut d.actions {
                        a.input_schema = None;
                        a.output_schema = None;
                    }
                }
            }
            return Ok(connection::response(Ok(result)));
        }
    };
    Ok(opencut_automation::response(Ok(data)))
}

fn read_json(path: &PathBuf) -> Result<Value, EditorError> {
    let reader: Box<dyn Read> = if path.as_os_str() == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).map_err(|e| {
            EditorError::new("INVALID_ARGUMENT", format!("{}: {e}", path.display()))
        })?)
    };
    let mut bytes = Vec::new();
    reader
        .take(2_097_153)
        .read_to_end(&mut bytes)
        .map_err(|e| EditorError::new("INVALID_ARGUMENT", e.to_string()))?;
    if bytes.len() > 2_097_152 {
        return Err(EditorError::new("INVALID_ARGUMENT", "Input exceeds 2 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|e| EditorError::new("INVALID_ARGUMENT", e.to_string()))
}

fn scoped_project_input(project: &str, mut input: Value) -> Result<Value, EditorError> {
    let body = input.as_object_mut().ok_or_else(|| {
        EditorError::new("INVALID_ARGUMENT", "Project input must be a JSON object")
    })?;
    if body
        .get("project_id")
        .is_some_and(|id| id.as_str() != Some(project))
    {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Input project_id differs from the bound project",
        ));
    }
    body.insert("project_id".into(), Value::String(project.to_owned()));
    Ok(input)
}
