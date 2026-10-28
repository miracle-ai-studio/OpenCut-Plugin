use std::{
    fs,
    net::SocketAddr,
    path::Path as FsPath,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::http_identity::{AuthorizedHost, Identity};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{
        IntoResponse, Response as HttpResponse, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post, put},
};
use opencut_automation::{Response, capabilities, domains, tool_specs};
use opencut_contracts::EditorError;
use opencut_host_runtime::Automation;
use serde_json::{Value, json};

#[derive(Clone)]
pub(super) struct Host {
    pub database: Arc<std::path::PathBuf>,
    automation: Arc<AutomationConnections>,
    token: Arc<String>,
    browser_session: Option<crate::web_host::BrowserSession>,
    origins: Arc<Vec<String>>,
    pub media_root: Arc<std::path::PathBuf>,
    pub artifact_root: Arc<std::path::PathBuf>,
    shutdown: Arc<AtomicBool>,
    pub identity: Identity,
    pub upload_slots: Arc<tokio::sync::Semaphore>,
}

/// Host-local idle connections. Dispatch runs outside the lock and resets the
/// caller/source on every checkout; authorization remains an application read
/// for every call. No prepared jobs, permissions or transactions are cached.
#[derive(Default)]
struct AutomationConnections(Mutex<Vec<Automation>>);
impl AutomationConnections {
    fn with<T>(
        &self,
        database: &FsPath,
        identity: &Identity,
        operation: impl FnOnce(&mut Automation) -> Result<T, EditorError>,
    ) -> Result<T, EditorError> {
        identity.check_lifetime()?;
        let idle = self
            .0
            .lock()
            .map_err(|_| EditorError::new("INTERNAL_ERROR", "Host connections are unavailable"))?
            .pop();
        let app = match idle {
            Some(app) => app,
            None => Automation::open(database)?,
        };
        identity.check_lifetime()?;
        let mut app = app
            .with_source(opencut_automation::CallSource::Http)
            .with_caller(identity.caller.clone());
        let response = operation(&mut app);
        if let Ok(mut idle) = self.0.lock() {
            if idle.len() < 8 {
                idle.push(app);
            }
        }
        response
    }
    fn call(
        &self,
        database: &FsPath,
        identity: &Identity,
        name: &str,
        args: Value,
    ) -> Result<Response, EditorError> {
        self.with(database, identity, |app| Ok(app.call(name, args)))
    }
}

impl Host {
    pub(super) fn with_automation<T>(
        &self,
        operation: impl FnOnce(&mut Automation) -> Result<T, EditorError>,
    ) -> Result<T, EditorError> {
        self.automation
            .with(self.database.as_ref(), &self.identity, operation)
    }
}

pub struct Options {
    pub origins: Vec<String>,
    pub mcp_session_timeout_seconds: u64,
    pub tool_surface: crate::mcp::ToolSurface,
    pub web_root: Option<std::path::PathBuf>,
}

pub async fn serve(
    database: &FsPath,
    listen: SocketAddr,
    token_path: &FsPath,
    options: Options,
    service: Option<&crate::server::Runtime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Options {
        mut origins,
        mcp_session_timeout_seconds,
        tool_surface,
        web_root,
    } = options;
    let web_root = web_root.map(crate::web_host::validate_root).transpose()?;
    struct PreviewShutdown(std::path::PathBuf);
    impl Drop for PreviewShutdown {
        fn drop(&mut self) {
            let _ = opencut_host_runtime::transport_io::shutdown_previews(&self.0);
        }
    }
    if !listen.ip().is_loopback() {
        return Err("This local host only accepts loopback addresses; remote authorization is not implemented".into());
    }
    // Bind before workspace initialization so a port collision has no workspace side effects.
    let listener = tokio::net::TcpListener::bind(listen).await?;
    if web_root.is_some() {
        origins.push(format!("http://{}", listener.local_addr()?));
        origins.push(format!(
            "http://localhost:{}",
            listener.local_addr()?.port()
        ));
    }
    let token = read_or_create_token(token_path)?;
    let media_root = database
        .parent()
        .unwrap_or_else(|| FsPath::new("."))
        .join("media");
    fs::create_dir_all(&media_root)?;
    let artifact_root = database
        .parent()
        .unwrap_or_else(|| FsPath::new("."))
        .join("artifacts");
    fs::create_dir_all(&artifact_root)?;
    drop(Automation::open(database)?);
    opencut_host_runtime::transport_io::recover_previews(database)?;
    let _preview_shutdown_guard = PreviewShutdown(database.to_path_buf());
    let stop = Arc::new(AtomicBool::new(false));
    let mcp = crate::mcp_http::HttpMcp::new(mcp_session_timeout_seconds, tool_surface);
    let mcp_router = mcp.router(Arc::new(database.to_path_buf()), origins.clone());
    let _mcp_shutdown_guard = mcp.cancellation.clone().drop_guard();
    let browser_session = web_root
        .as_ref()
        .map(|_| crate::web_host::BrowserSession::new());
    let host = Host {
        database: Arc::new(database.to_path_buf()),
        automation: Arc::new(AutomationConnections::default()),
        token: Arc::new(token),
        browser_session: browser_session.clone(),
        origins: Arc::new(origins),
        media_root: Arc::new(media_root),
        artifact_root: Arc::new(artifact_root),
        shutdown: stop.clone(),
        identity: Identity::owner(),
        upload_slots: Arc::new(tokio::sync::Semaphore::new(
            opencut_automation::capabilities::UPLOAD_INFLIGHT_LIMIT,
        )),
    };
    let router = Router::new()
        .route(
            "/api/capabilities",
            get(|| async { Json(opencut_automation::response(Ok(capabilities()))) }),
        )
        .route(
            "/api/operations",
            get(|| async { Json(opencut_automation::response(Ok(json!(tool_specs())))) }),
        )
        .route("/api/domains", get(domain_catalog))
        .route("/api/domains/{domain}", get(domain_contract))
        .route("/api/projects", get(projects).post(create_project))
        .route("/api/projects/{project}/snapshot", get(snapshot))
        .route("/api/projects/{project}/revisions", get(history))
        .route(
            "/api/projects/{project}/revisions/{revision}",
            get(revision),
        )
        .route(
            "/api/projects/{project}/branches/main/restore",
            post(restore),
        )
        .route(
            "/api/projects/{project}/branches/main/snapshot",
            get(snapshot),
        )
        .route(
            "/api/projects/{project}/branches/main/edit-plans",
            post(plan),
        )
        .route(
            "/api/projects/{project}/branches/main/transactions",
            post(apply),
        )
        .route("/api/tools/{name}", post(tool))
        .route("/api/preview/stream", post(crate::preview_http::stream))
        .route(
            "/api/preview/audio/stream",
            post(crate::audio_preview_http::stream),
        )
        .route(
            "/api/preview/visual-font/stream",
            post(crate::media_http::read_visual_preview_font),
        )
        .route(
            "/api/preview/visual-resource/read",
            post(crate::media_http::read_visual_preview_resource),
        )
        .route(
            "/api/preview/picture/stream",
            post(crate::visual_preview_http::stream),
        )
        .route("/api/media/upload", post(crate::media_http::upload))
        .route(
            "/api/uploads/{upload_id}/chunks/{offset}",
            put(crate::upload_http::put_chunk),
        )
        .route("/api/media/{hash}", get(crate::media_http::read))
        .route(
            "/api/projects/{project}/media/{media_id}/content",
            get(crate::media_http::read_project),
        )
        .route(
            "/api/project-resources/content",
            get(crate::media_http::read_project_resource),
        )
        .route("/api/jobs", get(jobs).post(start_job))
        .route("/api/jobs/{job}", get(job))
        .route("/api/jobs/{job}/cancel", post(cancel_job))
        .route("/api/jobs/{job}/artifact", get(crate::media_http::artifact))
        .route(
            "/api/artifacts/{artifact}",
            get(crate::media_http::artifact_reference),
        )
        .route("/api/render/preflight", post(render_preflight))
        .route("/api/changes/stream", post(changes_stream))
        .merge(mcp_router.with_state(()))
        .method_not_allowed_fallback(|| async {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(opencut_automation::response(Err(EditorError::new(
                    "INVALID_ARGUMENT",
                    "HTTP method is not allowed",
                )))),
            )
        })
        .layer(DefaultBodyLimit::max(2_097_152))
        .layer(middleware::from_fn_with_state(host.clone(), authorize))
        .with_state(host);
    let router = crate::web_host::attach(router, web_root, listener.local_addr()?, browser_session);
    if let Some(service) = service {
        service.publish(listener.local_addr()?)?;
    }
    eprintln!(
        "OpenCut API listening on http://{}; bearer token file: {}",
        listener.local_addr()?,
        token_path.display()
    );
    let worker_stop = stop.clone();
    let worker_database = database.to_path_buf();
    let mut worker = tokio::task::spawn_blocking(move || {
        opencut_host_runtime::worker::run(&worker_database, false, worker_stop)
    });
    let signal_stop = stop.clone();
    let preview_database = database.to_path_buf();
    let service_shutdown = crate::server::shutdown_requested(service);
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = crate::signals::termination() => {},
                _ = service_shutdown => {},
            }
            signal_stop.store(true, Ordering::SeqCst);
            let _ = tokio::task::spawn_blocking(move || {
                opencut_host_runtime::transport_io::shutdown_previews(&preview_database)
            })
            .await;
            mcp.shutdown().await;
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result=&mut server=>{
            stop.store(true,Ordering::SeqCst);
            worker.await??;
            result?;
        },
        result=&mut worker=>{
            if !stop.load(Ordering::SeqCst) {
                result??;
                return Err("Job worker stopped unexpectedly".into());
            }
            result??;
            server.await?;
        }
    }
    Ok(())
}

fn read_or_create_token(path: &FsPath) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write;
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let token = fs::read_to_string(path)?.trim().to_string();
            if token.len() < 32 || token.chars().any(char::is_whitespace) {
                return Err("API token must contain at least 32 non-whitespace characters".into());
            }
            Ok(token)
        }
        Err(e) => Err(e.into()),
    }
}

async fn authorize(
    State(host): State<Host>,
    mut request: axum::extract::Request,
    next: Next,
) -> HttpResponse {
    use opencut_automation::{
        CallSource,
        denials::{DenialReason, DeniedOperation},
    };
    let source = if request.uri().path() == "/mcp" {
        CallSource::McpHttp
    } else {
        CallSource::Http
    };
    let operation = if source == CallSource::McpHttp {
        DeniedOperation::McpRequest
    } else {
        DeniedOperation::HttpRequest
    };
    if request.headers().get_all("origin").iter().count() > 1
        || request.headers().get_all("authorization").iter().count() > 1
    {
        return crate::denials::refuse(
            &host,
            source,
            DenialReason::AuthenticationRejected,
            "Duplicate authorization or Origin header",
        )
        .await;
    }
    let origin = request.headers().get("origin").cloned();
    if let Some(origin) = &origin {
        let allowed = origin
            .to_str()
            .ok()
            .is_some_and(|o| host.origins.iter().any(|a| a == o));
        if !allowed {
            return crate::denials::refuse(
                &host,
                source,
                DenialReason::OriginRejected,
                "Origin is not permitted",
            )
            .await;
        }
    }
    if request.method() == Method::OPTIONS && origin.is_some() {
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_cors(response.headers_mut(), origin.as_ref());
        return response;
    }
    // Header presence chooses the authentication route. A malformed Basic or
    // Bearer header must never fall back to a browser's owner cookie.
    let Some((secret, browser_grant)) =
        request_credentials(host.token.as_str(), host.browser_session.as_ref(), &request)
    else {
        return crate::denials::refuse(
            &host,
            source,
            DenialReason::AuthenticationRejected,
            "A valid bearer token is required",
        )
        .await;
    };
    let mut identity = if secret == host.token.as_str() {
        Identity::owner()
    } else {
        let database = host.database.clone();
        let secret = secret.to_owned();
        match tokio::task::spawn_blocking(move || {
            Identity::authenticate(database.as_ref(), &secret)
        })
        .await
        {
            Ok(Ok(identity)) => identity,
            _ => {
                return crate::denials::refuse(
                    &host,
                    source,
                    DenialReason::AuthenticationRejected,
                    "A valid bearer token is required",
                )
                .await;
            }
        }
    };
    identity.browser_grant = browser_grant.clone();
    request.extensions_mut().insert(identity.clone());
    if let Some(grant) = &browser_grant {
        request.extensions_mut().insert(grant.clone());
        let path = request.uri().path();
        let upload = path == "/api/media/upload" || path.starts_with("/api/uploads/");
        // Ordinary JSON/MCP bodies keep the original 2 MiB admission. Raw
        // uploads retain streaming; chunks keep their existing 60s deadline,
        // and a whole legacy upload has an explicit 15-minute receive budget.
        let limit = std::time::Duration::from_secs(if path == "/api/media/upload" {
            900
        } else if upload {
            opencut_automation::capabilities::UPLOAD_CHUNK_TIMEOUT_SECONDS
        } else {
            30
        });
        let (parts, body) = request.into_parts();
        let started = std::time::Instant::now();
        let body = grant.incoming(body, limit);
        let body = if upload {
            body
        } else {
            match axum::body::to_bytes(body, 2_097_152).await {
                Ok(bytes) => axum::body::Body::from(bytes),
                Err(_) => {
                    if let Err(error) = grant.check() {
                        return error_response(&error.code, &error.message);
                    }
                    let mut response = error_response(
                        "INVALID_ARGUMENT",
                        if started.elapsed() >= limit {
                            "Cookie request body deadline exceeded"
                        } else {
                            "Request body could not be admitted within 2 MiB"
                        },
                    );
                    if started.elapsed() >= limit {
                        *response.status_mut() = StatusCode::REQUEST_TIMEOUT;
                    }
                    return response;
                }
            }
        };
        request = axum::extract::Request::from_parts(parts, body);
        if let Err(error) = grant.check() {
            return error_response(&error.code, &error.message);
        }
    }
    let mut request_host = host;
    request_host.identity = identity;
    let response = crate::denials::http(
        request_host.database.clone(),
        Some(request_host.identity.caller.clone()),
        source,
        operation,
        DenialReason::AuthorizationDenied,
        next.run(request).await,
    )
    .await;
    let mut response = crate::http_identity::guard_stream(&request_host, response, source);
    if let Some(grant) = browser_grant {
        response = grant.wrap_response(response);
    }
    add_cors(response.headers_mut(), origin.as_ref());
    response
}

pub(crate) fn request_credentials<'a>(
    token: &'a str,
    sessions: Option<&crate::web_host::BrowserSession>,
    request: &'a axum::extract::Request,
) -> Option<(&'a str, Option<crate::web_host::CookieGrant>)> {
    if let Some(header) = request.headers().get("authorization") {
        let secret = header.to_str().ok()?.strip_prefix("Bearer ")?;
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return None;
        }
        return Some((secret, None));
    }
    Some((token, Some(sessions?.capture(request)?)))
}

fn add_cors(headers: &mut HeaderMap, origin: Option<&HeaderValue>) {
    if let Some(origin) = origin {
        headers.insert("access-control-allow-origin", origin.clone());
        headers.insert("vary", HeaderValue::from_static("Origin"));
        headers.insert(
            "access-control-expose-headers",
            HeaderValue::from_static("MCP-Session-Id, MCP-Protocol-Version, X-OpenCut-Request-Id, X-OpenCut-Audit-Warning, X-OpenCut-Resource-Offset, X-OpenCut-Resource-Eof"),
        );
        headers.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
        headers.insert(
            "access-control-allow-headers",
            HeaderValue::from_static(
                "Authorization, Content-Type, Last-Event-ID, MCP-Session-Id, MCP-Protocol-Version",
            ),
        );
    }
}

async fn invoke(host: Host, name: String, args: Value) -> HttpResponse {
    match tokio::task::spawn_blocking(move || {
        host.automation
            .call(host.database.as_ref(), &host.identity, &name, args)
    })
    .await
    {
        Ok(Ok(response)) => into_http(response),
        Ok(Err(error)) => into_http(opencut_automation::response(Err(error))),
        Err(error) => error_response("INTERNAL_ERROR", &error.to_string()),
    }
}
async fn domain_catalog() -> HttpResponse {
    into_http(opencut_automation::response(
        domains::catalog(None, false).map(|v| json!(v)),
    ))
}
async fn domain_contract(Path(domain): Path<String>) -> HttpResponse {
    into_http(opencut_automation::response(
        domains::catalog(Some(&domain), true).map(|v| json!(v)),
    ))
}
fn body(body: Result<Json<Value>, JsonRejection>) -> Result<Value, Box<HttpResponse>> {
    body.map(|Json(value)| value)
        .map_err(|error| Box::new(error_response("INVALID_ARGUMENT", &error.body_text())))
}
async fn projects(
    AuthorizedHost(host): AuthorizedHost,
    query: Result<
        Query<opencut_contracts::project_catalog::LibraryInput>,
        axum::extract::rejection::QueryRejection,
    >,
) -> HttpResponse {
    let input = match query {
        Ok(Query(input)) => input,
        Err(error) => return error_response("INVALID_ARGUMENT", &error.to_string()),
    };
    match serde_json::to_value(input) {
        Ok(input) => invoke(host, "opencut_project_list".into(), input).await,
        Err(_) => error_response("INTERNAL_ERROR", "Cannot encode project directory request"),
    }
}
async fn jobs(
    AuthorizedHost(host): AuthorizedHost,
    query: Result<
        Query<opencut_automation::job_views::JobQueryInput>,
        axum::extract::rejection::QueryRejection,
    >,
) -> HttpResponse {
    let input = match query {
        Ok(Query(input)) => input,
        Err(error) => return error_response("INVALID_ARGUMENT", &error.to_string()),
    };
    match serde_json::to_value(input) {
        Ok(input) => invoke(host, "opencut_job_query".into(), input).await,
        Err(_) => error_response("INTERNAL_ERROR", "Cannot encode task query"),
    }
}
async fn job(AuthorizedHost(host): AuthorizedHost, Path(job): Path<String>) -> HttpResponse {
    invoke(host, "opencut_job_view".into(), json!({"job_id":job})).await
}
async fn cancel_job(AuthorizedHost(host): AuthorizedHost, Path(job): Path<String>) -> HttpResponse {
    invoke(
        host,
        "opencut_job_request_cancel".into(),
        json!({"job_id":job}),
    )
    .await
}
async fn start_job(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => invoke(host, "opencut_job_submit".into(), args).await,
        Err(e) => *e,
    }
}
async fn render_preflight(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => invoke(host, "opencut_render_preflight".into(), args).await,
        Err(e) => *e,
    }
}
async fn snapshot(
    AuthorizedHost(host): AuthorizedHost,
    Path(project): Path<String>,
) -> HttpResponse {
    invoke(
        host,
        "opencut_project_inspect".into(),
        json!({"project_id":project}),
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    from_revision: Option<String>,
    limit: Option<usize>,
}

async fn history(
    AuthorizedHost(host): AuthorizedHost,
    Path(project): Path<String>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> HttpResponse {
    let query = match query {
        Ok(Query(query)) => query,
        Err(e) => return error_response("INVALID_ARGUMENT", &e.to_string()),
    };
    invoke(host,"opencut_project_history".into(),json!({"project_id":project,"from_revision":query.from_revision,"limit":query.limit.unwrap_or(50)})).await
}
async fn revision(
    AuthorizedHost(host): AuthorizedHost,
    Path((project, revision)): Path<(String, String)>,
) -> HttpResponse {
    invoke(
        host,
        "opencut_project_inspect".into(),
        json!({"project_id":project,"revision":revision}),
    )
    .await
}
async fn restore(
    AuthorizedHost(host): AuthorizedHost,
    Path(project): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => match crate::scoped_project_input(&project, args) {
            Ok(args) => invoke(host, "opencut_project_restore".into(), args).await,
            Err(error) => into_http(opencut_automation::response(Err(error))),
        },
        Err(error) => *error,
    }
}
async fn create_project(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => invoke(host, "opencut_project_create".into(), args).await,
        Err(e) => *e,
    }
}
async fn plan(
    AuthorizedHost(host): AuthorizedHost,
    Path(project): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => match crate::scoped_project_input(&project, args) {
            Ok(args) => invoke(host, "opencut_edit_plan".into(), args).await,
            Err(error) => into_http(opencut_automation::response(Err(error))),
        },
        Err(e) => *e,
    }
}
async fn apply(
    AuthorizedHost(host): AuthorizedHost,
    Path(project): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => match crate::scoped_project_input(&project, args) {
            Ok(args) => invoke(host, "opencut_edit_apply".into(), args).await,
            Err(error) => into_http(opencut_automation::response(Err(error))),
        },
        Err(e) => *e,
    }
}
async fn tool(
    AuthorizedHost(host): AuthorizedHost,
    Path(name): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> HttpResponse {
    match body(input) {
        Ok(args) => invoke(host, name, args).await,
        Err(e) => *e,
    }
}
pub(super) fn into_http(response: Response) -> HttpResponse {
    let status = response.error.as_ref().map_or(StatusCode::OK, |error| {
        StatusCode::from_u16(opencut_automation::error_policy(&error.code).http_status)
            .expect("shared error policy contains valid HTTP statuses")
    });
    let id = HeaderValue::from_str(&response.request_id).ok();
    let warning = response
        .warnings
        .iter()
        .any(|s| s == opencut_automation::denials::FAILURE_WARNING);
    let mut response = (status, Json(response)).into_response();
    if let Some(id) = id {
        response
            .headers_mut()
            .insert(crate::denials::REQUEST_HEADER, id);
    }
    if warning {
        response.headers_mut().insert(
            crate::denials::WARNING_HEADER,
            HeaderValue::from_static(opencut_automation::denials::FAILURE_WARNING),
        );
    }
    response
}
pub(super) fn error_response(code: &str, message: &str) -> HttpResponse {
    into_http(opencut_automation::response(Err(EditorError::new(
        code, message,
    ))))
}

/// Shared public project/job stream with the same opaque cursors and records as
/// CLI NDJSON. POST avoids encoding a tagged, potentially long scope/cursor in a URL.
async fn changes_stream(
    AuthorizedHost(host): AuthorizedHost,
    headers: HeaderMap,
    input: Result<Json<opencut_automation::event_stream::EventChangesInput>, JsonRejection>,
) -> HttpResponse {
    use opencut_automation::event_stream::{StreamRecord, StreamRecordData};
    let mut input = match input {
        Ok(Json(input)) => input,
        Err(error) => return error_response("INVALID_ARGUMENT", &error.body_text()),
    };
    if headers.get_all("last-event-id").iter().count() > 1 {
        return error_response("INVALID_CURSOR", "Duplicate Last-Event-ID header");
    }
    if let Some(cursor) = headers.get("last-event-id") {
        let Ok(cursor) = cursor.to_str() else {
            return error_response("INVALID_CURSOR", "Invalid Last-Event-ID header");
        };
        input.cursor = Some(cursor.into());
    }
    let page = match read_changes(&host, input.clone()).await {
        Ok(page) => page,
        Err(error) => return into_http(opencut_automation::response(Err(error))),
    };
    let state = (
        host,
        input,
        std::collections::VecDeque::from(page.events),
        false,
        false,
    );
    let stream = futures_util::stream::unfold(
        state,
        |(host, mut input, mut pending, mut ready, mut finished)| async move {
            loop {
                if finished || host.shutdown.load(Ordering::SeqCst) {
                    return None;
                }
                let (name, id, record) = if !ready {
                    ready = true;
                    (
                        "ready",
                        None,
                        StreamRecordData::Ready {
                            scope: input.scope.clone(),
                            cursor: input.cursor.clone(),
                        },
                    )
                } else if let Some(change) = pending.pop_front() {
                    input.cursor = Some(change.cursor.clone());
                    (
                        "change",
                        Some(change.cursor.clone()),
                        StreamRecordData::Event {
                            cursor: change.cursor,
                            event: change.event,
                        },
                    )
                } else {
                    match read_changes(&host, input.clone()).await {
                        Ok(page) => {
                            pending.extend(page.events);
                            if pending.is_empty() {
                                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            }
                            continue;
                        }
                        Err(error) => {
                            finished = true;
                            (
                                "error",
                                None,
                                StreamRecordData::Error {
                                    cursor: input.cursor.clone(),
                                    error,
                                },
                            )
                        }
                    }
                };
                let mut wire = Event::default()
                    .event(name)
                    .json_data(StreamRecord::new(record))
                    .expect("serializable public event");
                if let Some(id) = id {
                    wire = wire.id(id);
                }
                return Some((
                    Ok::<_, std::convert::Infallible>(wire),
                    (host, input, pending, ready, finished),
                ));
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
async fn read_changes(
    host: &Host,
    input: opencut_automation::event_stream::EventChangesInput,
) -> Result<opencut_automation::event_stream::EventChangesPage, EditorError> {
    let path = host.database.clone();
    let caller = host.identity.caller.clone();
    tokio::task::spawn_blocking(move || {
        let mut app = opencut_host_runtime::workspace::Workspace::open_managed(path.as_ref())?;
        app.set_caller(caller);
        app.event_changes(input)
    })
    .await
    .map_err(|_| EditorError::new("INTERNAL_ERROR", "Event query failed"))?
}

#[cfg(test)]
#[path = "http_resource_tests.rs"]
mod resource_stream_tests;
