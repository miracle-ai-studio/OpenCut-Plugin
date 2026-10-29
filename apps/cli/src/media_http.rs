use crate::blocking_owned::BlockingOwned;
use axum::{
    body::Body,
    extract::{Path, Query},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_LENGTH},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use opencut_contracts::{Asset, EditorError};
use opencut_host_runtime::transport_io::AdmittedFile;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;
use tokio_util::io::ReaderStream;

use crate::http::{Host, error_response, into_http};
use crate::http_identity::AuthorizedHost;

const UPLOAD_WRITE_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = opencut_editor_application::uploads::MAX_SOURCE_BYTES;

#[derive(Deserialize)]
pub(super) struct UploadQuery {
    name: String,
}

pub(super) async fn upload(
    AuthorizedHost(host): AuthorizedHost,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !host.identity.caller.is_owner() {
        return error_response(
            "NOT_AUTHORIZED",
            "Global media upload requires owner authority",
        );
    }
    let staging = host
        .media_root
        .join(format!(".upload-{}", uuid::Uuid::new_v4().simple()));
    let context = opencut_automation::ExecutionContext::for_caller(
        opencut_automation::CallSource::Http,
        host.identity.caller.clone(),
    );
    let request_id = context.request_id().to_owned();
    let result = match declared_upload_length(&headers) {
        Ok(declared_size) => {
            upload_inner(&host, query, body, &staging, context, declared_size).await
        }
        Err(error) => Err(error),
    };
    into_http(opencut_automation::Response::from_result_with_id(
        request_id, result,
    ))
}

async fn upload_inner(
    host: &Host,
    query: UploadQuery,
    body: Body,
    staging: &std::path::Path,
    context: opencut_automation::ExecutionContext,
    declared_size: Option<u64>,
) -> Result<serde_json::Value, EditorError> {
    if query.name.trim().is_empty() || query.name.len() > 512 {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "A filename of 1..512 bytes is required",
        ));
    }
    let database = host.database.clone();
    let temporary = staging.to_path_buf();
    let mut file = BlockingOwned::create(move || {
        AdmittedFile::create(
            &database,
            &temporary,
            "media_upload",
            declared_size.unwrap_or(0),
            false,
        )
    })
    .await?;
    let mut stream = body.into_data_stream();
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut pending = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(io_error)?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| EditorError::new("INVALID_ARGUMENT", "Upload exceeds size limit"))?;
        if size > MAX_SOURCE_BYTES {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "Upload exceeds exact source addressability",
            ));
        }
        hash.update(&chunk);
        let mut offset = 0;
        while offset < chunk.len() {
            let count = (UPLOAD_WRITE_BATCH_BYTES - pending.len()).min(chunk.len() - offset);
            pending.extend_from_slice(&chunk[offset..offset + count]);
            offset += count;
            if pending.len() == UPLOAD_WRITE_BATCH_BYTES {
                let batch = std::mem::take(&mut pending);
                (file, pending) = file
                    .run(move |file| {
                        file.append(&batch)?;
                        let mut batch = batch;
                        batch.clear();
                        Ok(batch)
                    })
                    .await?;
            }
        }
    }
    if declared_size.is_some_and(|expected| expected != size) {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Upload byte count does not match Content-Length",
        ));
    }
    if !pending.is_empty() {
        (file, ()) = file.run(move |file| file.append(&pending)).await?;
    }
    if size == 0 {
        return Err(EditorError::new("INVALID_ARGUMENT", "Empty upload"));
    }
    let hash = format!("{:x}", hash.finalize());
    let root = host.media_root.clone();
    let database = host.database.clone();
    let staging = staging.to_path_buf();
    let identity = host.identity.clone();
    let (_file, result) = file
        .run(move |file| {
            file.sync()?;
            // Staging bytes are not a published import. Check again in this
            // queued worker immediately before the durable operation starts.
            identity.check_lifetime()?;
            let asset = opencut_host_runtime::transport_io::import_uploaded_media(
                &database,
                context,
                &root,
                &staging,
                &query.name,
                &hash,
                size,
            )?;
            Ok(json!({"asset":asset,"media_url":format!("/api/media/{hash}")}))
        })
        .await?;
    Ok(result)
}

fn declared_upload_length(headers: &HeaderMap) -> Result<Option<u64>, EditorError> {
    let Some(value) = headers.get(CONTENT_LENGTH) else {
        return Ok(None);
    };
    let size = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| EditorError::new("INVALID_ARGUMENT", "Invalid Content-Length"))?;
    if !(1..=MAX_SOURCE_BYTES).contains(&size) {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Upload size is outside exact source addressability",
        ));
    }
    Ok(Some(size))
}

#[cfg(test)]
mod upload_length_tests {
    use super::*;

    #[test]
    fn declared_length_reserves_only_nonempty_bounded_requests() {
        let headers = HeaderMap::new();
        assert_eq!(declared_upload_length(&headers).unwrap(), None);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4194304"));
        assert_eq!(declared_upload_length(&headers).unwrap(), Some(4_194_304));

        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&MAX_SOURCE_BYTES.to_string()).unwrap(),
        );
        assert_eq!(
            declared_upload_length(&headers).unwrap(),
            Some(MAX_SOURCE_BYTES)
        );
        let oversized = (MAX_SOURCE_BYTES + 1).to_string();
        for value in ["0", oversized.as_str(), "not-a-number"] {
            headers.insert(CONTENT_LENGTH, HeaderValue::from_str(value).unwrap());
            assert_eq!(
                declared_upload_length(&headers).unwrap_err().code,
                "INVALID_ARGUMENT"
            );
        }
    }
}

pub(super) async fn read(
    AuthorizedHost(host): AuthorizedHost,
    Path(hash): Path<String>,
) -> Response {
    if !host.identity.caller.is_owner() {
        return error_response(
            "NOT_AUTHORIZED",
            "Global media reads require owner authority",
        );
    }
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return error_response("NOT_FOUND", "Unknown media ID");
    }
    let metadata = match tokio::fs::read(host.media_root.join(format!("{hash}.json"))).await {
        Ok(bytes) => serde_json::from_slice::<Asset>(&bytes).ok(),
        Err(_) => None,
    };
    let Some(asset) = metadata else {
        return error_response("NOT_FOUND", "Media is unavailable");
    };
    let file = match tokio::fs::File::open(host.media_root.join(&hash)).await {
        Ok(file) => file,
        Err(_) => return error_response("NOT_FOUND", "Media file is missing"),
    };
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut response = (StatusCode::OK, Body::from_stream(ReaderStream::new(file))).into_response();
    if let Some(mime) = asset
        .metadata
        .get("mime_type")
        .and_then(|v| v.as_str())
        .and_then(|v| HeaderValue::from_str(v).ok())
    {
        response.headers_mut().insert("content-type", mime);
    }
    if let Ok(size) = HeaderValue::from_str(&size.to_string()) {
        response.headers_mut().insert("content-length", size);
    }
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Read project media only after resolving the opaque resource within the
/// caller's current project grant. The returned file descriptor is verified
/// against its fixed CAS version before any bytes reach the response.
pub(super) async fn read_project(
    AuthorizedHost(host): AuthorizedHost,
    Path((project, media_id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    let database = host.database.clone();
    let identity = host.identity.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let mut app = opencut_host_runtime::workspace::Workspace::open_managed(database.as_ref())?;
        identity.validate(&mut app)?;
        app.set_caller(identity.caller);
        let imported = app
            .project_media(&opencut_editor_application::uploads::ProjectMediaInput {
                project_id: project,
                media_id,
            })
            .map_err(opencut_editor_application::uploads::public_error)?;
        let mime = imported
            .asset
            .metadata
            .get("mime_type")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let (file, size) =
            opencut_host_runtime::transport_io::open_verified_media(&database, &imported.asset)?;
        Ok::<_, EditorError>((file, size, mime))
    })
    .await;
    let (file, size, mime) = match opened {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return into_http(opencut_automation::response(Err(error))),
        Err(_) => return error_response("INTERNAL_ERROR", "Cannot read project media"),
    };
    serve_verified_file(file, size, mime, headers).await
}

pub(super) async fn read_project_resource(
    AuthorizedHost(host): AuthorizedHost,
    Query(input): Query<opencut_contracts::project_resources::InspectResource>,
    headers: HeaderMap,
) -> Response {
    let database = host.database.clone();
    let identity = host.identity.clone();
    let scope = input.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let mut app = opencut_host_runtime::workspace::Workspace::open_managed(database.as_ref())?;
        identity.validate(&mut app)?;
        app.set_caller(identity.caller.clone());
        let resource = app.project_resource(&input)?;
        let (file, size) = opencut_host_runtime::transport_io::open_verified_project_resource(
            &database,
            &resource.version,
        )?;
        // Hashing may take time. Recheck current authority before response bytes.
        identity.validate(&mut app)?;
        let current = app.project_resource(&input)?;
        if current != resource {
            return Err(EditorError::new(
                "ARTIFACT_VERIFICATION_FAILED",
                "Fixed resource changed",
            ));
        }
        Ok::<_, EditorError>((file, size, resource.mime_type))
    })
    .await;
    let (file, size, mime) = match opened {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return into_http(opencut_automation::response(Err(error))),
        Err(_) => return error_response("INTERNAL_ERROR", "Cannot read project resource"),
    };
    // A typed metadata field is not permission to serve active HTML/SVG content.
    let mime = mime
        .filter(|value| {
            matches!(
                value.as_str(),
                "image/png"
                    | "image/jpeg"
                    | "image/webp"
                    | "image/gif"
                    | "image/avif"
                    | "video/mp4"
                    | "video/webm"
                    | "video/quicktime"
                    | "audio/mpeg"
                    | "audio/mp4"
                    | "audio/ogg"
                    | "audio/wav"
                    | "audio/x-wav"
                    | "audio/flac"
                    | "audio/webm"
                    | "font/ttf"
                    | "font/otf"
                    | "font/woff"
                    | "font/woff2"
            )
        })
        .unwrap_or_else(|| "application/octet-stream".into());
    // Use the same bounded chunk size as fixed preview fonts. A 4KiB default
    // otherwise reopens/revalidates the workspace over 1500 times for a 6MiB
    // source, which can exhaust preparation before any frame is drawn.
    // The existing stream guard still checks live authority before every chunk.
    let mut response =
        serve_verified_file_with_capacity(file, size, Some(mime), headers, Some(64 * 1024)).await;
    response
        .extensions_mut()
        .insert(crate::http_identity::RevalidateProjectResource(scope));
    response
}

/// Session-scoped fixed font bytes for complete V4 live frames. The response
/// carries the full typed scope so every chunk checks the original session.
pub(super) async fn read_visual_preview_font(
    AuthorizedHost(host): AuthorizedHost,
    headers: HeaderMap,
    input: Result<
        axum::Json<opencut_host_runtime::visual_preview_contracts::VisualPreviewFont>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let input = match input {
        Ok(axum::Json(input)) => input,
        Err(_) => return error_response("INVALID_ARGUMENT", "Invalid visual preview font request"),
    };
    let database = host.database.clone();
    let identity = host.identity.clone();
    let scope = input.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let mut workspace =
            opencut_host_runtime::workspace::Workspace::open_managed(database.as_ref())?;
        identity.validate(&mut workspace)?;
        let mut app = opencut_host_runtime::Automation::open(database.as_ref())?
            .with_source(opencut_automation::CallSource::Http)
            .with_caller(identity.caller.clone());
        let result = app.open_visual_preview_font(input.clone())?;
        identity.validate(&mut workspace)?;
        app.authorize_visual_preview_font(&input)?;
        Ok::<_, EditorError>(result)
    })
    .await;
    let (file, size) = match opened {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return into_http(opencut_automation::response(Err(error))),
        Err(_) => return error_response("INTERNAL_ERROR", "Cannot read visual preview font"),
    };
    let mut response = serve_verified_file_with_capacity(
        file,
        size,
        Some("application/octet-stream".into()),
        headers,
        Some(64 * 1024),
    )
    .await;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
        .extensions_mut()
        .insert(crate::http_identity::RevalidateVisualPreviewFont(scope));
    response
}

async fn serve_verified_file(
    file: std::fs::File,
    size: u64,
    mime: Option<String>,
    headers: HeaderMap,
) -> Response {
    serve_verified_file_with_capacity(file, size, mime, headers, None).await
}

async fn serve_verified_file_with_capacity(
    file: std::fs::File,
    size: u64,
    mime: Option<String>,
    headers: HeaderMap,
    capacity: Option<usize>,
) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let range = match headers.get("range") {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| byte_range(value, size))
        {
            Some(range) => Some(range),
            None => {
                let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                response.headers_mut().insert(
                    "content-range",
                    HeaderValue::from_str(&format!("bytes */{size}")).unwrap(),
                );
                return response;
            }
        },
        None => None,
    };
    let (start, end) = range.unwrap_or((0, size.saturating_sub(1)));
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut file = tokio::fs::File::from_std(file);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return error_response("STORAGE_ERROR", "Cannot seek media");
    }
    // Fixed V4 resources and preview fonts use bounded 64KiB chunks so live
    // authorization does not reopen the workspace once per 4KiB. Other routes
    // retain ReaderStream's default; every guarded chunk is still authorized.
    let stream = match capacity {
        Some(capacity) => ReaderStream::with_capacity(file.take(length), capacity),
        None => ReaderStream::new(file.take(length)),
    };
    let mut response = (
        if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        Body::from_stream(stream),
    )
        .into_response();
    response
        .headers_mut()
        .insert("accept-ranges", HeaderValue::from_static("bytes"));
    if range.is_some() {
        response.headers_mut().insert(
            "content-range",
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap(),
        );
    }
    response
        .extensions_mut()
        .insert(crate::http_identity::RevalidateStream);
    if let Some(mime) = mime.and_then(|value| HeaderValue::from_str(&value).ok()) {
        response.headers_mut().insert("content-type", mime);
    }
    if let Ok(size) = HeaderValue::from_str(&length.to_string()) {
        response.headers_mut().insert("content-length", size);
    }
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

fn io_error(error: impl std::fmt::Display) -> EditorError {
    EditorError::new("STORAGE_ERROR", error.to_string())
}

/// Only the committed successful attempt can publish an artifact. The caller
/// supplies a job identity, never a filesystem path.
pub(super) async fn artifact(
    AuthorizedHost(host): AuthorizedHost,
    Path(job): Path<String>,
) -> Response {
    serve_artifact(host, job, None).await
}
pub(super) async fn artifact_reference(
    AuthorizedHost(host): AuthorizedHost,
    Path(id): Path<String>,
) -> Response {
    match opencut_editor_application::job_views::parse_artifact_id(&id) {
        Ok((job, hash)) => serve_artifact(host, job.into(), Some(hash.into())).await,
        Err(error) => error_response(&error.code, &error.message),
    }
}
async fn serve_artifact(host: Host, job: String, requested_hash: Option<String>) -> Response {
    let database = host.database.clone();
    let caller = host.identity.caller.clone();
    let record = match tokio::task::spawn_blocking(move || {
        let mut app = opencut_host_runtime::workspace::Workspace::open_managed(database.as_ref())?;
        app.set_caller(caller);
        app.artifact_job(&job)
    })
    .await
    {
        Ok(Ok(data)) => data,
        Ok(Err(error)) => return into_http(opencut_automation::response(Err(error))),
        Err(_) => return error_response("INTERNAL_ERROR", "Cannot read task state"),
    };
    if record.state != opencut_api_contracts::JobState::Succeeded {
        return error_response(
            "CONSTRAINT_VIOLATION",
            "Artifact is only available after the task succeeds",
        );
    }
    let view = opencut_editor_application::job_views::project(record.clone());
    let reference = view.artifacts.iter().find(|reference| {
        requested_hash.as_deref().map_or_else(
            || {
                matches!(
                    reference.role,
                    opencut_api_contracts::job_views::ArtifactRole::Primary
                )
            },
            |hash| reference.content_hash == hash,
        )
    });
    let Some(reference) = reference else {
        return error_response("NOT_FOUND", "Task has no downloadable artifact");
    };
    let path =
        match opencut_editor_application::job_views::private_artifact_path(&record, reference) {
            Ok(path) => path,
            Err(error) => return into_http(opencut_automation::response(Err(error))),
        };
    let job_id = record.job_id.clone();
    let attempt = record.attempt;
    // Preserve the published name for the native adapter. Canonicalizing it
    // here would erase a final-component symlink before openat can reject it.
    let path = std::path::PathBuf::from(path);
    let Ok(expected_size) = reference.size_bytes.parse::<u64>() else {
        return error_response("ARTIFACT_VERIFICATION_FAILED", "Artifact size is missing");
    };
    let expected_hash = reference.content_hash.clone();
    let mime = reference.mime_type.clone();
    let artifact_root = host.artifact_root.clone();
    let verified_path = host
        .artifact_root
        .join(format!(".download-{}", uuid::Uuid::new_v4().simple()));
    let database = host.database.clone();
    let cancelled = Cancellation::default();
    let signal = cancelled.0.clone();
    let verified = match BlockingOwned::create(move || {
        let mut source = opencut_host_runtime::transport_io::ArtifactReader::open(
            artifact_root.as_ref(),
            &job_id,
            attempt,
            &path,
        )?;
        let metadata = source.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() != expected_size {
            return Err(EditorError::new(
                "ARTIFACT_VERIFICATION_FAILED",
                "Artifact size does not match the published result",
            ));
        }
        let mut verified = AdmittedFile::create(
            &database,
            &verified_path,
            "artifact_download",
            expected_size,
            true,
        )?;
        let mut digest = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 65536];
        loop {
            if signal.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(EditorError::new(
                    "CANCELLED",
                    "Download request was cancelled",
                ));
            }
            let count = source.read(&mut buffer).map_err(io_error)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| io_error("File size overflow"))?;
            if copied > expected_size {
                return Err(EditorError::new(
                    "ARTIFACT_VERIFICATION_FAILED",
                    "Artifact changed during verification",
                ));
            }
            digest.update(&buffer[..count]);
            verified.append(&buffer[..count])?;
        }
        if copied != expected_size || format!("{:x}", digest.finalize()) != expected_hash {
            return Err(EditorError::new(
                "ARTIFACT_VERIFICATION_FAILED",
                "Artifact hash does not match the published result",
            ));
        }
        verified.rewind()?;
        Ok(verified)
    })
    .await
    {
        Ok(file) => file,
        Err(error) => return into_http(opencut_automation::response(Err(error))),
    };
    // The stream owns the anonymous file and its reservation until EOF or drop.
    // Moving ownership into every blocking read also fences cancelled reads.
    let stream = futures_util::stream::try_unfold(verified, |file| async move {
        let (file, bytes) = file
            .run(|file| file.read_chunk())
            .await
            .map_err(|error| std::io::Error::other(error.message))?;
        Ok::<_, std::io::Error>(if bytes.is_empty() {
            None
        } else {
            Some((bytes, file))
        })
    });
    let mut response = (StatusCode::OK, Body::from_stream(stream)).into_response();
    // Artifact authorization must not depend on its declared MIME type; JSON
    // analysis artifacts are still byte streams requiring ongoing authorization.
    response
        .extensions_mut()
        .insert(crate::http_identity::RevalidateStream);
    if let Ok(value) = HeaderValue::from_str(&mime) {
        response.headers_mut().insert("content-type", value);
    }
    if let Ok(value) = HeaderValue::from_str(&expected_size.to_string()) {
        response.headers_mut().insert("content-length", value);
    }
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[derive(Default)]
struct Cancellation(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        return (suffix > 0).then_some((size.saturating_sub(suffix), size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && start <= end).then_some((start, end))
}
#[cfg(test)]
mod range_tests {
    use super::byte_range;
    #[test]
    fn media_ranges_are_bounded_and_reject_ambiguous_requests() {
        assert_eq!(byte_range("bytes=2-4", 10), Some((2, 4)));
        assert_eq!(byte_range("bytes=2-", 10), Some((2, 9)));
        assert_eq!(byte_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(byte_range("bytes=-30", 10), Some((0, 9)));
        assert_eq!(byte_range("bytes=2-40", 10), Some((2, 9)));
        for value in [
            "bytes=10-",
            "bytes=4-2",
            "bytes=-0",
            "bytes=0-1,4-5",
            "bytes=a-b",
        ] {
            assert_eq!(byte_range(value, 10), None);
        }
        assert_eq!(byte_range("bytes=0-", 0), None);
    }
}

/// Binary adaptation of the same typed bounded-read operation used by MCP.
pub(super) async fn read_visual_preview_resource(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<
        axum::Json<opencut_host_runtime::visual_preview_contracts::VisualPreviewResourceRead>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let input = match input {
        Ok(axum::Json(input)) => input,
        Err(_) => return error_response("INVALID_ARGUMENT", "Invalid visual resource range"),
    };
    let result = tokio::task::spawn_blocking(move || {
        // Local-owner identity has no principal policy revision to read. A
        // delegated reader keeps both live policy checks around the byte read.
        let mut workspace = if host.identity.caller.is_owner() {
            None
        } else {
            Some(opencut_host_runtime::workspace::Workspace::open_managed(
                host.database.as_ref(),
            )?)
        };
        let validate =
            |workspace: &mut Option<opencut_host_runtime::workspace::Workspace>| match workspace {
                Some(app) => host.identity.validate(app),
                None => host.identity.check_lifetime(),
            };
        validate(&mut workspace)?;
        let (bytes, eof) = host.with_automation(|app| app.read_visual_preview_resource(&input))?;
        validate(&mut workspace)?;
        Ok::<_, EditorError>((bytes, eof, input.offset))
    })
    .await;
    match result {
        Ok(Ok((bytes, eof, offset))) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            let headers = response.headers_mut();
            headers.insert(
                "content-type",
                HeaderValue::from_static("application/octet-stream"),
            );
            headers.insert("cache-control", HeaderValue::from_static("no-store"));
            headers.insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            headers.insert(
                "x-opencut-resource-offset",
                HeaderValue::from_str(&offset).expect("canonical decimal"),
            );
            headers.insert(
                "x-opencut-resource-eof",
                HeaderValue::from_static(if eof { "true" } else { "false" }),
            );
            response
        }
        Ok(Err(error)) => into_http(opencut_automation::response(Err(error))),
        Err(_) => error_response("INTERNAL_ERROR", "Cannot read visual resource range"),
    }
}
