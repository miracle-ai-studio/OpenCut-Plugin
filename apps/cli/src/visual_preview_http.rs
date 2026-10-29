//! Binary presentation bytes from an already completed native scene frame.
use crate::http_identity::AuthorizedHost;
use axum::{
    Json,
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use opencut_host_runtime::visual_preview_contracts::VisualPreviewPictureRead;
use std::time::Duration;

type PictureChunk = Result<Bytes, std::io::Error>;

async fn reserve_picture_chunk(
    send: &tokio::sync::mpsc::Sender<PictureChunk>,
    mut checkpoint: impl FnMut() -> bool,
) -> Option<tokio::sync::mpsc::Permit<'_, PictureChunk>> {
    // A slot is not an authority grant. The caller must perform its guarded
    // read after reservation, then revalidate transport identity before send.
    match send.try_reserve() {
        Ok(permit) => return Some(permit),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return None,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline || send.is_closed() || !checkpoint() {
            return None;
        }
        // Capacity wakes this future immediately. A blocked reader still
        // triggers authority/generation checks at the original 10ms bound.
        let until = deadline.min(now + Duration::from_millis(10));
        match tokio::time::timeout_at(until, send.reserve()).await {
            Ok(Ok(permit)) => {
                if !checkpoint() {
                    return None;
                }
                return Some(permit);
            }
            Ok(Err(_)) => return None,
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
    let mut reader = match opened {
        Ok(Ok(reader)) => reader,
        Ok(Err(error)) => return crate::http::error_response(&error.code, &error.message),
        Err(_) => {
            return crate::http::error_response("INTERNAL_ERROR", "Native picture could not start");
        }
    };
    let length = reader.byte_length();
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    let runtime = tokio::runtime::Handle::current();
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
            let Some(permit) = runtime.block_on(reserve_picture_chunk(&send, || {
                reader.checkpoint().is_ok() && host.identity.validate(&mut authority).is_ok()
            })) else {
                return;
            };
            // The reader checks live project/session authority both before
            // and after copying this bounded chunk, after any capacity wait.
            let chunk = match reader.read() {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return,
                Err(_) => {
                    permit.send(Err(std::io::Error::other(
                        "Native picture authority or generation ended",
                    )));
                    return;
                }
            };
            if host.identity.validate(&mut authority).is_err() {
                return;
            }
            permit.send(Ok(Bytes::from(chunk)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn released_capacity_delivers_without_waiting_for_a_poll_tick() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let checks = Cell::new(0);
        let delivery = reserve_picture_chunk(&send, || {
            checks.set(checks.get() + 1);
            true
        });
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        assert_eq!(receive.recv().await.unwrap().unwrap(), "first");
        // No timer advance or sleep: the released slot itself is sufficient.
        let std::task::Poll::Ready(Some(permit)) = futures_util::poll!(delivery.as_mut()) else {
            panic!("Released capacity must be immediately usable");
        };
        permit.send(Ok(Bytes::from_static(b"second")));
        assert_eq!(receive.recv().await.unwrap().unwrap(), "second");
        assert!(checks.get() >= 2);
    }

    #[tokio::test]
    async fn revoked_generation_cannot_use_a_slot_released_during_the_wait() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let current = Cell::new(true);
        let delivery = reserve_picture_chunk(&send, || current.get());
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        current.set(false);
        receive.recv().await.unwrap().unwrap();
        assert!(matches!(
            futures_util::poll!(delivery.as_mut()),
            std::task::Poll::Ready(None)
        ));
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_closed_reader_wakes_and_releases_the_pending_chunk() {
        let (send, receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let delivery = reserve_picture_chunk(&send, || true);
        tokio::pin!(delivery);
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        drop(receive);
        assert!(matches!(
            futures_util::poll!(delivery.as_mut()),
            std::task::Poll::Ready(None)
        ));
    }

    #[tokio::test]
    async fn stalled_capacity_keeps_rechecking_and_expires_without_sending() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        send.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let checks = Cell::new(0);
        assert!(
            reserve_picture_chunk(&send, || {
                checks.set(checks.get() + 1);
                true
            })
            .await
            .is_none()
        );
        assert!(checks.get() > 1);
        assert_eq!(receive.recv().await.unwrap().unwrap(), "first");
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test]
    async fn available_capacity_is_claimed_before_reading_any_picture_bytes() {
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let permit = reserve_picture_chunk(&send, || panic!("No backpressure to validate"))
            .await
            .unwrap();
        assert!(
            receive.try_recv().is_err(),
            "Claiming capacity does not read or publish pixels"
        );
        // A failed guarded read drops the permit and leaves the stream empty.
        drop(permit);
        assert!(send.try_reserve().is_ok());
    }
}
