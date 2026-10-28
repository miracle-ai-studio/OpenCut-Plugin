//! Binary presentation bytes from an already completed native scene frame.
use crate::http_identity::AuthorizedHost;
use axum::{
    Json,
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use opencut_host_runtime::visual_preview_contracts::VisualPreviewPictureRead;
use std::time::{Duration, Instant};

pub(super) async fn stream(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<Json<VisualPreviewPictureRead>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let input = match input {
        Ok(Json(input)) => input,
        Err(_) => {
            return crate::http::error_response(
                "INVALID_ARGUMENT",
                "Invalid native picture request",
            );
        }
    };
    let identity = host.identity.clone();
    let opened_host = host.clone();
    let opened = tokio::task::spawn_blocking(move || {
        identity.check_lifetime()?;
        opened_host.with_automation(|app| app.open_visual_preview_picture(input))
    })
    .await;
    let mut reader = match opened {
        Ok(Ok(reader)) => reader,
        Ok(Err(error)) => return crate::http::error_response(&error.code, &error.message),
        Err(_) => {
            return crate::http::error_response("INTERNAL_ERROR", "Native picture could not start");
        }
    };
    let length = reader.byte_length();
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    tokio::task::spawn_blocking(move || {
        let mut authority = match opencut_host_runtime::workspace::Workspace::open_managed(
            host.database.as_ref(),
        ) {
            Ok(authority) => authority,
            Err(_) => return,
        };
        loop {
            if send.is_closed() || host.identity.validate(&mut authority).is_err() {
                return;
            }
            let chunk = match reader.read() {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return,
                Err(_) => {
                    let _ = send.try_send(Err(std::io::Error::other(
                        "Native picture authority or generation ended",
                    )));
                    return;
                }
            };
            if host.identity.validate(&mut authority).is_err() {
                return;
            }
            let mut pending = Ok(Bytes::from(chunk));
            let wait = Instant::now();
            loop {
                if reader.checkpoint().is_err() || host.identity.validate(&mut authority).is_err() {
                    return;
                }
                match send.try_send(pending) {
                    Ok(()) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(value)) => {
                        if wait.elapsed() >= Duration::from_secs(1) {
                            return;
                        }
                        pending = value;
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
    });
    let body = Body::from_stream(futures_util::stream::unfold(
        receive,
        |mut receive| async move { receive.recv().await.map(|chunk| (chunk, receive)) },
    ));
    let mut response = (
        [
            (header::CONTENT_TYPE, "application/vnd.opencut.rgba"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        axum::http::HeaderValue::from_str(&length.to_string()).expect("bounded picture length"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response
}
