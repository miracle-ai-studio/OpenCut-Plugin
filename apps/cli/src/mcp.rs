use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use opencut_automation::{domains, tool_spec, tool_specs};
use opencut_host_runtime::Automation;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub(super) enum ToolSurface {
    #[default]
    Domains,
    Legacy,
}

// Transport selection only: every call still reaches the existing registered use case.
// Remote sessions never construct a Workspace or acquire a database/service lock.
#[derive(Clone)]
pub(super) enum Backend {
    Local(Arc<PathBuf>),
    Remote(Arc<opencut_sdk::Client>),
}
impl Backend {
    pub(super) fn call(
        &self,
        source: opencut_automation::CallSource,
        caller: opencut_automation::Caller,
        name: &str,
        input: Value,
    ) -> opencut_automation::Response {
        match self {
            Self::Local(database) => match Automation::open(database.as_ref()) {
                Ok(app) => app
                    .with_source(source)
                    .with_caller(caller)
                    .call(name, input),
                Err(error) => opencut_automation::response(Err(error)),
            },
            Self::Remote(client) => {
                crate::connection::response(client.call_registered(name, &input))
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct EditorMcp {
    backend: Backend,
    subscriptions: crate::mcp_context::Subscriptions,
    source: opencut_automation::CallSource,
    caller: opencut_automation::Caller,
    tool_surface: ToolSurface,
}

impl EditorMcp {
    fn request_caller(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<opencut_automation::Caller, McpError> {
        if self.source == opencut_automation::CallSource::McpHttp {
            return context
                .extensions
                .get::<axum::http::request::Parts>()
                .and_then(|parts| parts.extensions.get::<crate::http_identity::Identity>())
                .map(|identity| -> Result<_, McpError> {
                    identity.check_lifetime().map_err(|_| {
                        McpError::invalid_params(
                            "The local browser session expired or ended",
                            Some(json!({"code":"NOT_AUTHORIZED"})),
                        )
                    })?;
                    Ok(identity.caller.clone())
                })
                .transpose()?
                .ok_or_else(|| {
                    McpError::invalid_params(
                        "Authenticated request identity is missing",
                        Some(json!({"code":"NOT_AUTHORIZED"})),
                    )
                });
        }
        Ok(self.caller.clone())
    }

    async fn authorized_caller(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<opencut_automation::Caller, McpError> {
        let caller = self.request_caller(context)?;
        if let Backend::Remote(client) = &self.backend {
            let client = client.clone();
            // The host authenticates this token on every HTTP request. The local
            // caller value is unused by Remote and cannot grant owner authority.
            let response = tokio::task::spawn_blocking(move || {
                crate::connection::response(
                    client.call_registered("opencut_capabilities", &json!({})),
                )
            })
            .await
            .map_err(|_| McpError::internal_error("Remote identity check failed", None))?;
            if let Some(error) = response.error {
                return Err(McpError::invalid_params(
                    error.message,
                    Some(json!({
                        "code": error.code, "request_id": response.request_id, "warnings": response.warnings
                    })),
                ));
            }
            return Ok(caller);
        }
        if caller.is_owner() {
            return Ok(caller);
        }
        let Backend::Local(database) = &self.backend else {
            unreachable!()
        };
        let database = database.clone();
        let source = self.source;
        let checked = caller.clone();
        tokio::task::spawn_blocking(move|| {
            let mut app=opencut_host_runtime::workspace::Workspace::open_managed(database.as_ref()).map_err(|_|McpError::internal_error("Identity check unavailable",None))?;
            app.set_caller(checked.clone());
            if let Err(error)=app.access_view() {
                let response=opencut_automation::denials::observe_response(&app,Some(&checked),source,opencut_automation::denials::DeniedOperation::McpRequest,opencut_automation::denials::DenialReason::AuthorizationDenied,opencut_automation::response(Err(error)));
                return Err(McpError::invalid_params("Current identity is not authorized",Some(json!({"code":response.error.as_ref().map(|e|&e.code),"request_id":response.request_id,"warnings":response.warnings}))));
            }
            Ok(())
        }).await.map_err(|_|McpError::internal_error("Identity check failed",None))??;
        Ok(caller)
    }

    fn with_caller(mut self, caller: opencut_automation::Caller) -> Self {
        self.caller = caller;
        self
    }
    pub(super) fn new(
        database: Arc<PathBuf>,
        source: opencut_automation::CallSource,
        tool_surface: ToolSurface,
    ) -> Self {
        Self {
            backend: Backend::Local(database),
            subscriptions: Default::default(),
            source,
            caller: opencut_automation::Caller::LocalOwner,
            tool_surface,
        }
    }
}

fn tool(spec: opencut_automation::ToolSpec) -> Tool {
    Tool::new(spec.name, spec.description, mcp_schema(spec.input_schema))
        .with_raw_output_schema(Arc::new(mcp_schema(spec.output_schema)))
        .with_annotations(
            ToolAnnotations::new()
                .read_only(spec.read_only)
                .idempotent(true)
                .open_world(false),
        )
}

fn tools(surface: ToolSurface) -> Vec<Tool> {
    let specs = match surface {
        ToolSurface::Domains => domains::tool_specs_compact(),
        ToolSurface::Legacy => tool_specs(),
    };
    specs.into_iter().map(tool).collect()
}

/// MCP requires a root object type and object-valued top-level property schemas.
/// Keep the canonical v1 schema unchanged; these are equivalent wire forms for
/// our object DTOs (including unions whose alternatives are all objects).
fn mcp_schema(schema: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut schema = schema.as_object().expect("object schema").clone();
    schema.insert("type".into(), serde_json::json!("object"));
    if let Some(properties) = schema.get_mut("properties").and_then(|v| v.as_object_mut()) {
        for property in properties.values_mut() {
            match property {
                serde_json::Value::Bool(true) => *property = serde_json::json!({}),
                serde_json::Value::Bool(false) => *property = serde_json::json!({"not": {}}),
                _ => {}
            }
        }
    }
    schema
}

impl ServerHandler for EditorMcp {
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Owned(vec![rmcp::model::ProtocolVersion::V_2025_11_25])
    }
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, McpError> {
        self.authorized_caller(&context).await?;
        crate::mcp_context::first_page(request)?;
        Ok(crate::mcp_context::resources_list())
    }
    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, McpError> {
        self.authorized_caller(&context).await?;
        crate::mcp_context::first_page(request)?;
        Ok(crate::mcp_context::templates_list())
    }
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, McpError> {
        let data = crate::mcp_context::read(
            self.backend.clone(),
            self.authorized_caller(&context).await?,
            self.source,
            request.uri.clone(),
        )
        .await?;
        Ok(
            rmcp::model::ReadResourceResult::new(vec![crate::mcp_context::content(
                request.uri,
                &data,
            )])
            .into(),
        )
    }
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListPromptsResult, McpError> {
        self.authorized_caller(&context).await?;
        crate::mcp_context::first_page(request)?;
        Ok(crate::mcp_context::prompts_list())
    }
    async fn get_prompt(
        &self,
        request: rmcp::model::GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetPromptResponse, McpError> {
        crate::mcp_context::prompt(
            self.backend.clone(),
            self.authorized_caller(&context).await?,
            self.source,
            request,
        )
        .await
    }
    async fn subscribe(
        &self,
        request: rmcp::model::SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.subscriptions
            .subscribe(
                self.backend.clone(),
                self.authorized_caller(&context).await?,
                self.source,
                request.uri,
                context.peer,
            )
            .await
    }
    async fn unsubscribe(
        &self,
        request: rmcp::model::UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.authorized_caller(&context).await?;
        self.subscriptions.unsubscribe(&request.uri).await
    }
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().enable_resources_subscribe().enable_prompts().build())
            .with_server_info(Implementation::new("opencut", env!("CARGO_PKG_VERSION")))
            .with_instructions("Read opencut://schemas/domains for common actions and opencut://schemas/domains/{domain} for all actions and exact schemas in that domain. Domain tools and named operations use the same application contract and authorization. Inspect capabilities and the fixed project revision before editing. Plan edits before applying them. Retry writes with the original idempotency key and unchanged payload. Read task results and published artifacts before reporting output success.")
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.authorized_caller(&context).await?;
        if request.and_then(|r| r.cursor).is_some() {
            return Err(McpError::invalid_params("Unknown tools cursor", None));
        }
        Ok(ListToolsResult::with_all_items(tools(self.tool_surface)))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        domains::domain_tool_spec(name)
            .or_else(|| tool_spec(name))
            .map(tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if self.get_tool(&request.name).is_none() {
            return Err(McpError::invalid_params(
                "Unknown OpenCut tool",
                Some(json!({"name":request.name})),
            ));
        }
        let backend = self.backend.clone();
        let source = self.source;
        let caller = self.request_caller(&context)?;
        let identity = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<crate::http_identity::Identity>())
            .cloned();
        let response = tokio::task::spawn_blocking(move || {
            let dispatch = || -> Result<_, opencut_contracts::EditorError> {
                if let Some(identity) = &identity {
                    identity.check_lifetime()?;
                }
                // HTTP browser lifetime is checked before and after opening the
                // local host. Remote calls retain the service's own fences.
                if let Backend::Local(database) = &backend {
                    let automation = Automation::open(database.as_ref())?;
                    if let Some(identity) = &identity {
                        identity.check_lifetime()?;
                    }
                    Ok(automation.with_source(source).with_caller(caller).call(
                        &request.name,
                        Value::Object(request.arguments.unwrap_or_default()),
                    ))
                } else {
                    Ok(backend.call(
                        source,
                        caller,
                        &request.name,
                        Value::Object(request.arguments.unwrap_or_default()),
                    ))
                }
            };
            match dispatch() {
                Ok(response) => response,
                Err(error) => opencut_automation::response(Err(error)),
            }
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let is_error = response.error.is_some();
        let value = serde_json::to_value(response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(if is_error {
            CallToolResult::structured_error(value)
        } else {
            CallToolResult::structured(value)
        }
        .into())
    }
}

pub async fn serve(
    database: &Path,
    caller: opencut_automation::Caller,
    tool_surface: ToolSurface,
    with_worker: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if with_worker && !caller.is_owner() {
        return Err(opencut_editor_application::access::denied().into());
    }
    drop(Automation::open(database)?);
    let handler = EditorMcp::new(
        Arc::new(database.to_path_buf()),
        opencut_automation::CallSource::McpStdio,
        tool_surface,
    )
    .with_caller(caller);
    run_stdio(handler, with_worker.then(|| database.to_path_buf())).await
}

pub async fn serve_remote(
    client: Arc<opencut_sdk::Client>,
    tool_surface: ToolSurface,
) -> Result<(), Box<dyn std::error::Error>> {
    let handler = EditorMcp {
        backend: Backend::Remote(client),
        subscriptions: Default::default(),
        source: opencut_automation::CallSource::McpStdio,
        caller: opencut_automation::Caller::LocalOwner,
        tool_surface,
    };
    run_stdio(handler, None).await
}

async fn run_stdio(
    handler: EditorMcp,
    worker_database: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let subscriptions = handler.subscriptions.clone();
    let disconnected = tokio_util::sync::CancellationToken::new();
    let (input, output) = stdio();
    let input = DisconnectReader {
        input,
        disconnected: disconnected.clone(),
    };
    let server = handler.serve((input, output)).await?;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut worker = worker_database.map(|worker_database| {
        let worker_stop = stop.clone();
        tokio::task::spawn_blocking(move || {
            opencut_host_runtime::worker::run(&worker_database, false, worker_stop)
        })
    });
    let mut worker_finished = false;
    let result: Result<(), Box<dyn std::error::Error>> = tokio::select! {
        result=server.waiting()=>result.map(|_| ()).map_err(Into::into),
        _=disconnected.cancelled()=>Ok(()),
        _=crate::signals::termination()=>Ok(()),
        result=async {
            match worker.as_mut() {
                Some(task) => task.await.map_err(|error| -> Box<dyn std::error::Error> {error.into()})?
                    .map_err(|error| -> Box<dyn std::error::Error> {error.into()}),
                None => std::future::pending::<Result<(), Box<dyn std::error::Error>>>().await,
            }
        } => {
            worker_finished = true;
            result.and_then(|_| Err("Managed Worker stopped unexpectedly".into()))
        },
    };
    // Disconnect is a host stop, never a durable job cancellation. The scheduler
    // fences unfinished attempts and a later Worker recovers their leases.
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    subscriptions.shutdown().await;
    if !worker_finished && let Some(worker) = worker {
        worker.await??;
    }
    result
}

// EOF must be observable independently of the SDK flushing its writer during
// transport.close(). A peer that stopped reading may otherwise block teardown.
struct DisconnectReader<R> {
    input: R,
    disconnected: tokio_util::sync::CancellationToken,
}
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for DisconnectReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = std::pin::Pin::new(&mut self.input).poll_read(cx, buffer);
        if matches!(&result, std::task::Poll::Ready(Err(_)))
            || (matches!(&result, std::task::Poll::Ready(Ok(())))
                && buffer.filled().len() == before
                && buffer.remaining() > 0)
        {
            self.disconnected.cancel();
        }
        result
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    fn unavailable() -> Backend {
        Backend::Remote(Arc::new(
            opencut_sdk::Client::with_options(
                "http://127.0.0.1:1",
                "synthetic-test-token",
                opencut_sdk::Options {
                    request_timeout: std::time::Duration::from_millis(100),
                    max_response_bytes: 4096,
                },
            )
            .unwrap(),
        ))
    }

    #[test]
    fn remote_calls_fail_as_transport_errors_without_local_owner_fallback() {
        let response = unavailable().call(
            opencut_automation::CallSource::McpStdio,
            opencut_automation::Caller::LocalOwner,
            "opencut_project",
            json!({"action":"list","input":{"limit":1}}),
        );
        assert!(response.data.is_none());
        assert_eq!(response.error.unwrap().code, "TRANSPORT_ERROR");
        assert!(!response.request_id.is_empty());
    }

    #[tokio::test]
    async fn remote_resource_failures_keep_transport_code_and_request_identity() {
        let error = crate::mcp_context::read(
            unavailable(),
            opencut_automation::Caller::LocalOwner,
            opencut_automation::CallSource::McpStdio,
            "opencut://system/capabilities".into(),
        )
        .await
        .unwrap_err();
        let data = error.data.unwrap();
        assert_eq!(data["code"], "TRANSPORT_ERROR");
        assert!(!data["request_id"].as_str().unwrap().is_empty());
    }
}
