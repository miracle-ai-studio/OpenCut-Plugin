//! Binary presentation bytes from an already completed native scene frame.
use crate::http_identity::AuthorizedHost;
use axum::{
    Json,
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use opencut_contracts::EditorError;
use opencut_host_runtime::{audio_preview::VisualPictureReader, visual_preview_contracts::*};
use std::{io::Write, time::Duration};

/// One bounded control header, followed by the unchanged RGBA payload.
fn prepared_header(
    header: &impl serde::Serialize,
    pixel_length: u64,
) -> Result<Vec<u8>, EditorError> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len()
                > PICTURE_PREPARE_PREFIX_BYTES + PICTURE_PREPARE_MAX_HEADER_BYTES - self.0.len()
            {
                return Err(std::io::Error::other(
                    "Native picture header exceeds its bound",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if pixel_length == 0 || pixel_length > 64 * 1024 * 1024 {
        return Err(EditorError::new(
            "INVALID_VISUAL_PLAN",
            "Native picture payload exceeds its bound",
        ));
    }
    let mut bytes = Bounded(vec![0; PICTURE_PREPARE_PREFIX_BYTES]);
    serde_json::to_writer(&mut bytes, header).map_err(|_| {
        EditorError::new(
            "VISUAL_BUDGET_EXCEEDED",
            "Native picture header exceeds its bound",
        )
    })?;
    let length = (bytes.0.len() - PICTURE_PREPARE_PREFIX_BYTES) as u32;
    bytes.0[..4].copy_from_slice(PICTURE_PREPARE_MAGIC);
    bytes.0[4..8].copy_from_slice(&length.to_le_bytes());
    bytes.0[8..12].copy_from_slice(&(pixel_length as u32).to_le_bytes());
    Ok(bytes.0)
}

async fn send_picture_chunk(
    send: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    bytes: Bytes,
    mut checkpoint: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline || send.is_closed() || !checkpoint() {
            return false;
        }
        // Capacity wakes this future immediately. A blocked reader still
        // triggers authority/generation checks at the original 10ms bound.
        let until = deadline.min(now + Duration::from_millis(10));
        match tokio::time::timeout_at(until, send.reserve()).await {
            Ok(Ok(permit)) => {
                if !checkpoint() {
                    return false;
                }
                permit.send(Ok(bytes));
                return true;
            }
            Ok(Err(_)) => return false,
            Err(_) => {}
        }
    }
}

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
    let reader = match opened {
        Ok(Ok(reader)) => reader,
        Ok(Err(error)) => return crate::http::error_response(&error.code, &error.message),
        Err(_) => {
            return crate::http::error_response("INTERNAL_ERROR", "Native picture could not start");
        }
    };
    picture_response(host, reader, None)
}

pub(super) async fn prepare(
    AuthorizedHost(host): AuthorizedHost,
    input: Result<Json<VisualPreviewPicturePrepare>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let input = match input {
        Ok(Json(input)) => input,
        Err(_) => {
            return crate::http::error_response(
                "INVALID_ARGUMENT",
                "Invalid native picture preparation",
            );
        }
    };
    let identity = host.identity.clone();
    let opened_host = host.clone();
    let opened = tokio::task::spawn_blocking(move || {
        identity.check_lifetime()?;
        opened_host.with_automation(|app| {
            let (header, reader) = app.prepare_visual_preview_picture(input)?;
            let metadata = prepared_header(&header, reader.byte_length())?;
            Ok((metadata, reader))
        })
    })
    .await;
    match opened {
        Ok(Ok((metadata, reader))) => picture_response(host, reader, Some(metadata)),
        Ok(Err(error)) => crate::http::error_response(&error.code, &error.message),
        Err(_) => crate::http::error_response("INTERNAL_ERROR", "Native picture could not prepare"),
    }
}

fn picture_response(
    host: crate::http::Host,
    mut reader: VisualPictureReader,
    metadata: Option<Vec<u8>>,
) -> Response {
    let content_type = if metadata.is_some() {
        PICTURE_PREPARE_CONTENT_TYPE
    } else {
        "application/vnd.opencut.rgba"
    };
    let length = reader.byte_length() + metadata.as_ref().map_or(0, |bytes| bytes.len() as u64);
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut authority = match opencut_host_runtime::workspace::Workspace::open_managed(
            host.database.as_ref(),
        ) {
            Ok(authority) => authority,
            Err(_) => return,
        };
        if let Some(metadata) = metadata {
            for chunk in metadata.chunks(PICTURE_CHUNK_BYTES) {
                if host.identity.validate(&mut authority).is_err() || reader.checkpoint().is_err() {
                    return;
                }
                let bytes = Bytes::copy_from_slice(chunk);
                if reader.checkpoint().is_err() || host.identity.validate(&mut authority).is_err() {
                    return;
                }
                if !runtime.block_on(send_picture_chunk(&send, bytes, || {
                    reader.checkpoint().is_ok() && host.identity.validate(&mut authority).is_ok()
                })) {
                    return;
                }
            }
        }
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
            if !runtime.block_on(send_picture_chunk(&send, Bytes::from(chunk), || {
                reader.checkpoint().is_ok() && host.identity.validate(&mut authority).is_ok()
            })) {
                return;
            }
        }
    });
    let body = Body::from_stream(futures_util::stream::unfold(
        receive,
        |mut receive| async move { receive.recv().await.map(|chunk| (chunk, receive)) },
    ));
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn prepared_prefix_has_exact_lengths_and_bounded_metadata() {
        let value = serde_json::json!({"kind":"scene","frame":{"fixed":true}});
        let encoded = prepared_header(&value, 16).unwrap();
        assert_eq!(&encoded[..4], b"OCVP");
        assert_eq!(
            u32::from_le_bytes(encoded[4..8].try_into().unwrap()) as usize,
            encoded.len() - 12
        );
        assert_eq!(u32::from_le_bytes(encoded[8..12].try_into().unwrap()), 16);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded[12..]).unwrap(),
            value
        );
        assert!(prepared_header(&value, 0).is_err());
        assert!(prepared_header(&value, 64 * 1024 * 1024 + 1).is_err());
        let exact = "x".repeat(PICTURE_PREPARE_MAX_HEADER_BYTES - 2);
        assert_eq!(
            prepared_header(&exact, 4).unwrap().len(),
            PICTURE_PREPARE_MAX_HEADER_BYTES + 12
        );
        assert!(prepared_header(&(exact + "x"), 4).is_err());
    }

    #[tokio::test]
    async fn available_capacity_requires_fresh_authority_before_publishing() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        assert!(!send_picture_chunk(&send, Bytes::from_static(b"revoked"), || false).await);
        assert!(receive.try_recv().is_err());
        assert!(send_picture_chunk(&send, Bytes::from_static(b"current"), || true).await);
        assert_eq!(receive.recv().await.unwrap().unwrap(), "current");
    }

    #[tokio::test]
    async fn released_capacity_delivers_without_waiting_for_a_poll_tick() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let checks = Cell::new(0);
        let delivery = send_picture_chunk(&send, Bytes::from_static(b"second"), || {
            checks.set(checks.get() + 1);
            true
        });
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        assert_eq!(receive.recv().await.unwrap().unwrap(), "first");
        // No timer advance or sleep: the released slot itself is sufficient.
        assert_eq!(
            futures_util::poll!(delivery.as_mut()),
            std::task::Poll::Ready(true)
        );
        assert_eq!(receive.recv().await.unwrap().unwrap(), "second");
        assert!(checks.get() >= 2);
    }

    #[tokio::test]
    async fn revoked_generation_cannot_use_a_slot_released_during_the_wait() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let current = Cell::new(true);
        let delivery = send_picture_chunk(&send, Bytes::from_static(b"revoked"), || current.get());
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        current.set(false);
        receive.recv().await.unwrap().unwrap();
        assert_eq!(
            futures_util::poll!(delivery.as_mut()),
            std::task::Poll::Ready(false)
        );
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_closed_reader_wakes_and_releases_the_pending_chunk() {
        let (send, receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let delivery = send_picture_chunk(&send, Bytes::from_static(b"unused"), || true);
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        drop(receive);
        assert_eq!(
            futures_util::poll!(delivery.as_mut()),
            std::task::Poll::Ready(false)
        );
    }

    #[tokio::test]
    async fn stalled_capacity_keeps_rechecking_and_expires_without_sending() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let checks = Cell::new(0);
        assert!(
            !send_picture_chunk(&send, Bytes::from_static(b"unused"), || {
                checks.set(checks.get() + 1);
                true
            })
            .await
        );
        assert!(checks.get() > 1);
        assert_eq!(receive.recv().await.unwrap().unwrap(), "first");
        assert!(receive.try_recv().is_err());
    }
}
