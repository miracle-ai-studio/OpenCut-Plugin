//! Ephemeral local browser grants. Secrets never become public DTOs or logs.
use super::*;
use axum::body::HttpBody;
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSIONS: usize = 256;
#[derive(Clone)]
pub struct BrowserSession {
    sessions: Arc<Mutex<HashMap<String, Arc<Grant>>>>,
}
struct Grant {
    expires: Instant,
    revoked: AtomicBool,
}
/// Captured authentication lifetime. Debug/equality expose no cookie secret;
/// equality also keeps distinct browser grants isolated in MCP sessions.
#[derive(Clone)]
pub(crate) struct CookieGrant(Arc<Grant>);
impl std::fmt::Debug for CookieGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CookieGrant([redacted])")
    }
}
impl PartialEq for CookieGrant {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for CookieGrant {}
impl CookieGrant {
    pub(crate) fn check(&self) -> Result<(), opencut_contracts::EditorError> {
        if self.0.live(Instant::now()) {
            Ok(())
        } else {
            Err(opencut_contracts::EditorError::new(
                "NOT_AUTHORIZED",
                "The local browser session expired or ended",
            ))
        }
    }
    pub(crate) fn wrap_response(&self, response: Response) -> Response {
        ResponseGrant(self.0.clone()).wrap(response)
    }
    /// A fixed receive deadline, with revalidation even when no bytes arrive.
    /// Errors never look like successful EOF to an upload/body consumer.
    pub(crate) fn incoming(&self, body: Body, limit: Duration) -> Body {
        let deadline = Instant::now() + limit;
        let stream = futures_util::stream::unfold(
            (body.into_data_stream(), self.clone(), false),
            move |(mut source, grant, ended)| async move {
                if ended {
                    return None;
                }
                loop {
                    let failure = if grant.check().is_err() {
                        Some((
                            std::io::ErrorKind::PermissionDenied,
                            "Local browser session ended while receiving the request",
                        ))
                    } else if Instant::now() >= deadline {
                        Some((
                            std::io::ErrorKind::TimedOut,
                            "Cookie request body deadline exceeded",
                        ))
                    } else {
                        None
                    };
                    if let Some((kind, message)) = failure {
                        return Some((
                            Err(std::io::Error::new(kind, message)),
                            (source, grant, true),
                        ));
                    }
                    let chunk = tokio::select! {value=source.next()=>Some(value),_=tokio::time::sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())))=>None};
                    if grant.check().is_err() {
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "Local browser session ended while receiving the request",
                            )),
                            (source, grant, true),
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "Cookie request body deadline exceeded",
                            )),
                            (source, grant, true),
                        ));
                    }
                    match chunk {
                        None => continue,
                        Some(None) => return None,
                        Some(Some(value)) => {
                            return Some((
                                value.map_err(std::io::Error::other),
                                (source, grant, false),
                            ));
                        }
                    }
                }
            },
        );
        Body::from_stream(stream)
    }
}
#[derive(Clone)]
struct CookieResponseGuarded;
impl Grant {
    fn live(&self, now: Instant) -> bool {
        !self.revoked.load(Ordering::Acquire) && self.expires > now
    }
}
/// Body consumers retain only this revocable lifetime, never the cookie secret.
pub(super) struct ResponseGrant(Arc<Grant>);
impl ResponseGrant {
    pub(super) fn wrap(self, response: Response) -> Response {
        if !response.status().is_success()
            || response
                .extensions()
                .get::<CookieResponseGuarded>()
                .is_some()
        {
            return response;
        }
        let (mut parts, body) = response.into_parts();
        parts.extensions.insert(CookieResponseGuarded);
        // Preserve the exact length the HTTP server would infer for finite JSON
        // responses; long streams still use their original transfer semantics.
        if !parts.headers.contains_key(header::CONTENT_LENGTH)
            && !parts.headers.contains_key(header::TRANSFER_ENCODING)
            && let Some(length) = body.size_hint().exact()
        {
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        }
        // These API transports publish data frames, with no HTTP trailers. Like
        // the existing identity stream guard, BodyDataStream retains backpressure
        // and errors while dropping trailers; no body is buffered or prefetched.
        let stream = futures_util::stream::unfold(
            (body.into_data_stream(), self.0),
            |(mut source, grant)| async move {
                loop {
                    if !grant.live(Instant::now()) {
                        return None;
                    }
                    let chunk = tokio::select! {
                        value=source.next()=>Some(value),
                        _=tokio::time::sleep(Duration::from_millis(250))=>None,
                    };
                    if !grant.live(Instant::now()) {
                        return None;
                    }
                    if let Some(chunk) = chunk {
                        return chunk.map(|value| (value, (source, grant)));
                    }
                }
            },
        );
        Response::from_parts(parts, Body::from_stream(stream))
    }
}
#[derive(serde::Serialize)]
struct SessionStatus {
    active: bool,
    expires_in_seconds: u64,
}
impl BrowserSession {
    pub(crate) fn capture(&self, request: &Request) -> Option<CookieGrant> {
        self.grant(request, Instant::now()).map(CookieGrant)
    }
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn same_origin(request: &Request) -> bool {
        let h = request.headers();
        let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
        h.get_all(header::ORIGIN).iter().count() <= 1
            && h.get("sec-fetch-site").is_none_or(|v| v == "same-origin")
            && match (origin, host) {
                (Some(origin), Some(host)) => origin == format!("http://{host}"),
                (None, _) => matches!(*request.method(), Method::GET | Method::HEAD),
                _ => false,
            }
    }
    fn secret(request: &Request) -> Option<&str> {
        let mut values = request
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|v| v.trim().strip_prefix("opencut_session="));
        let value = values.next()?;
        (values.next().is_none()
            && value.len() == 64
            && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(value)
    }
    fn grant(&self, request: &Request, now: Instant) -> Option<Arc<Grant>> {
        if !Self::same_origin(request) {
            return None;
        }
        let secret = Self::secret(request)?;
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, grant| grant.live(now));
        sessions.get(secret).cloned()
    }
    fn remaining(&self, request: &Request, now: Instant) -> Option<Duration> {
        self.grant(request, now)?
            .expires
            .checked_duration_since(now)
    }
    pub(super) fn response_grant(&self, request: &Request) -> Option<ResponseGrant> {
        let path = request.uri().path();
        if request.headers().contains_key(header::AUTHORIZATION)
            || !(path.starts_with("/api/") || path == "/mcp")
        {
            return None;
        }
        self.grant(request, Instant::now()).map(ResponseGrant)
    }
    pub fn accepts(&self, request: &Request) -> bool {
        self.remaining(request, Instant::now()).is_some()
    }
    pub(super) fn handle(&self, request: Request) -> Response {
        if !Self::same_origin(&request)
            || (!matches!(*request.method(), Method::GET | Method::HEAD)
                && request
                    .headers()
                    .get("x-opencut-app")
                    .is_none_or(|v| v != "web"))
        {
            return StatusCode::FORBIDDEN.into_response();
        }
        let now = Instant::now();
        if matches!(*request.method(), Method::GET | Method::HEAD) {
            let remaining = self.remaining(&request, now);
            let mut response = axum::Json(SessionStatus {
                active: remaining.is_some(),
                expires_in_seconds: remaining.map_or(0, |d| d.as_secs()),
            })
            .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            if request.method() == Method::HEAD {
                *response.body_mut() = Body::empty();
            }
            return response;
        }
        let Ok(mut sessions) = self.sessions.lock() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        sessions.retain(|_, grant| grant.live(now));
        let cookie = if request.method() == Method::DELETE {
            if let Some(secret) = Self::secret(&request) {
                if let Some(grant) = sessions.remove(secret) {
                    grant.revoked.store(true, Ordering::Release);
                }
            }
            "opencut_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0".to_owned()
        } else if request.method() == Method::POST {
            let current = Self::secret(&request).filter(|secret| sessions.contains_key(*secret));
            let (secret, seconds) = if let Some(secret) = current {
                // Bootstrap never extends an existing grant's fixed expiry.
                (
                    secret.to_owned(),
                    sessions[secret].expires.duration_since(now).as_secs(),
                )
            } else {
                if sessions.len() >= MAX_SESSIONS {
                    return StatusCode::TOO_MANY_REQUESTS.into_response();
                }
                let secret = format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                sessions.insert(
                    secret.clone(),
                    Arc::new(Grant {
                        expires: now + TTL,
                        revoked: AtomicBool::new(false),
                    }),
                );
                (secret, TTL.as_secs())
            };
            format!(
                "opencut_session={secret}; HttpOnly; SameSite=Strict; Path=/; Max-Age={seconds}"
            )
        } else {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        };
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("bounded session cookie"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(method: Method, cookie: Option<&str>) -> Request {
        let mut r = Request::builder()
            .method(method)
            .uri("/app/session")
            .header("host", "localhost:8788")
            .header("origin", "http://localhost:8788")
            .header("sec-fetch-site", "same-origin")
            .header("x-opencut-app", "web");
        if let Some(cookie) = cookie {
            r = r.header("cookie", cookie);
        }
        r.body(Body::empty()).unwrap()
    }
    fn open(sessions: &BrowserSession) -> String {
        let response = sessions.handle(request(Method::POST, None));
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let header = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(
            header.contains("HttpOnly")
                && header.contains("SameSite=Strict")
                && header.contains("Max-Age=28800")
        );
        header.split(';').next().unwrap().to_owned()
    }
    #[test]
    fn authorization_header_never_falls_back_to_a_live_owner_cookie() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let r = request(Method::POST, Some(&cookie));
        let (secret, grant) =
            crate::http::request_credentials("fixture-owner", Some(&sessions), &r).unwrap();
        assert_eq!(secret, "fixture-owner");
        assert!(grant.is_some());
        for value in [
            "Basic ignored",
            "Bearer ",
            "Bearer fixture-owner ",
            "Bearer fixture owner",
            "bearer fixture-owner",
            "",
        ] {
            let mut r = request(Method::POST, Some(&cookie));
            r.headers_mut()
                .insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            assert!(
                crate::http::request_credentials("fixture-owner", Some(&sessions), &r).is_none()
            );
        }
        let mut r = request(Method::POST, Some(&cookie));
        r.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer fixture-owner"),
        );
        let (secret, grant) =
            crate::http::request_credentials("fixture-owner", Some(&sessions), &r).unwrap();
        assert_eq!(secret, "fixture-owner");
        assert!(grant.is_none());
    }
    #[tokio::test]
    async fn incoming_idle_body_revocation_is_an_error_not_successful_eof() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let grant = sessions
            .capture(&request(Method::POST, Some(&cookie)))
            .unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel::<axum::body::Bytes>(1);
        let body = Body::from_stream(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move {
                receiver
                    .recv()
                    .await
                    .map(|part| (Ok::<_, std::io::Error>(part), receiver))
            },
        ));
        let mut incoming = grant
            .incoming(body, Duration::from_secs(2))
            .into_data_stream();
        sender
            .send(axum::body::Bytes::from_static(b"partial"))
            .await
            .unwrap();
        assert_eq!(incoming.next().await.unwrap().unwrap(), "partial");
        sessions.handle(request(Method::DELETE, Some(&cookie)));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), incoming.next())
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(incoming.next().await.is_none());
        assert!(sender.is_closed());
    }
    #[tokio::test]
    async fn incoming_body_has_a_fixed_deadline_even_with_a_live_session() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let grant = sessions
            .capture(&request(Method::POST, Some(&cookie)))
            .unwrap();
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >());
        let mut incoming = grant
            .incoming(body, Duration::from_millis(20))
            .into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), incoming.next())
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(grant.check().is_ok());
        assert!(incoming.next().await.is_none());
    }
    #[test]
    fn captured_cookie_owner_is_rechecked_at_dispatch_and_does_not_revoke_bearer_owner() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let other = open(&sessions);
        let grant = sessions
            .capture(&request(Method::POST, Some(&cookie)))
            .unwrap();
        let mut identity = crate::http_identity::Identity::owner();
        identity.browser_grant = Some(grant.clone());
        assert_eq!(identity.clone(), identity);
        assert_ne!(
            Some(grant.clone()),
            sessions.capture(&request(Method::POST, Some(&other)))
        );
        let directory = tempfile::tempdir().unwrap();
        let mut app = opencut_host_runtime::workspace::Workspace::open_managed(
            directory.path().join("workspace.sqlite"),
        )
        .unwrap();
        assert!(identity.validate(&mut app).is_ok());
        sessions.handle(request(Method::DELETE, Some(&cookie)));
        assert_eq!(
            identity.check_lifetime().unwrap_err().code,
            "NOT_AUTHORIZED"
        );
        assert_eq!(
            identity.validate(&mut app).unwrap_err().code,
            "NOT_AUTHORIZED"
        );
        assert!(
            crate::http_identity::Identity::owner()
                .validate(&mut app)
                .is_ok()
        );
        assert!(!format!("{identity:?}").contains(cookie.split_once('=').unwrap().1));
    }
    #[tokio::test]
    async fn incoming_body_rechecks_expiry_after_waiting_for_a_chunk() {
        let grant = CookieGrant(Arc::new(Grant {
            expires: Instant::now() + Duration::from_millis(20),
            revoked: AtomicBool::new(false),
        }));
        let body = Body::from_stream(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok::<_, std::io::Error>("late write intent")
        }));
        assert!(
            axum::body::to_bytes(grant.incoming(body, Duration::from_secs(1)), 1024)
                .await
                .is_err()
        );
    }
    #[test]
    fn separate_browser_grants_expire_and_logout_only_revokes_original_cookie() {
        let sessions = BrowserSession::new();
        let a = open(&sessions);
        let b = open(&sessions);
        assert_ne!(a, b);
        assert!(sessions.accepts(&request(Method::GET, Some(&a))));
        assert!(!BrowserSession::new().accepts(&request(Method::GET, Some(&a))));
        let response = sessions.handle(request(Method::DELETE, Some(&a)));
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert!(!sessions.accepts(&request(Method::GET, Some(&a))));
        assert!(sessions.accepts(&request(Method::GET, Some(&b))));
        assert!(
            sessions
                .remaining(&request(Method::GET, Some(&b)), Instant::now() + TTL)
                .is_none()
        );
    }
    #[test]
    fn rejected_origin_duplicate_cookie_and_bounded_capacity_do_not_issue_grants() {
        let sessions = BrowserSession::new();
        let a = open(&sessions);
        let mut cross = request(Method::DELETE, Some(&a));
        cross.headers_mut().insert(
            "origin",
            HeaderValue::from_static("https://attacker.example"),
        );
        assert_eq!(sessions.handle(cross).status(), StatusCode::FORBIDDEN);
        assert!(sessions.accepts(&request(Method::GET, Some(&a))));
        assert!(!sessions.accepts(&request(Method::GET, Some(&format!("{a}; {a}")))));
        for _ in 1..MAX_SESSIONS {
            open(&sessions);
        }
        assert_eq!(
            sessions.handle(request(Method::POST, None)).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            sessions.handle(request(Method::POST, Some(&a))).status(),
            StatusCode::NO_CONTENT
        );
    }
    #[test]
    fn bootstrap_reuses_but_never_extends_a_live_grant() {
        let sessions = BrowserSession::new();
        let a = open(&sessions);
        let before = sessions
            .sessions
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .expires;
        sessions.handle(request(Method::POST, Some(&a)));
        assert_eq!(
            sessions
                .sessions
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .expires,
            before
        );
    }
    #[tokio::test]
    async fn status_is_read_only_no_store_and_never_exposes_the_secret() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let response = sessions.handle(request(Method::GET, Some(&cookie)));
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["active".into(), "expires_in_seconds".into()])
        );
        assert_eq!(value["active"], true);
        assert!(
            value["expires_in_seconds"]
                .as_u64()
                .is_some_and(|v| v <= TTL.as_secs())
        );
        assert!(
            !String::from_utf8(bytes.to_vec())
                .unwrap()
                .contains(cookie.split_once('=').unwrap().1)
        );
        sessions.handle(request(Method::DELETE, Some(&cookie)));
        let bytes = axum::body::to_bytes(
            sessions
                .handle(request(Method::GET, Some(&cookie)))
                .into_body(),
            1024,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["active"],
            false
        );
    }
    fn api_request(cookie: &str) -> Request {
        let mut r = request(Method::GET, Some(cookie));
        *r.uri_mut() = "/api/job-events/stream".parse().unwrap();
        r
    }
    #[test]
    fn response_fence_is_only_for_cookie_authenticated_api_requests() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        assert!(sessions.response_grant(&api_request(&cookie)).is_some());
        assert!(
            sessions
                .response_grant(&request(Method::GET, Some(&cookie)))
                .is_none()
        );
        let mut public = api_request(&cookie);
        *public.uri_mut() = "/".parse().unwrap();
        assert!(sessions.response_grant(&public).is_none());
        let mut bearer = api_request(&cookie);
        bearer.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer independent"),
        );
        assert!(sessions.response_grant(&bearer).is_none());
        let mut mcp = api_request(&cookie);
        *mcp.uri_mut() = "/mcp".parse().unwrap();
        assert!(sessions.response_grant(&mcp).is_some());
        assert!(
            sessions
                .response_grant(&api_request("opencut_session=unknown"))
                .is_none()
        );
    }
    #[tokio::test]
    async fn logout_revokes_an_already_captured_grant_before_queued_bytes_are_yielded() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let identity = crate::http_identity::Identity {
            browser_grant: sessions.capture(&api_request(&cookie)),
            ..crate::http_identity::Identity::owner()
        };
        assert!(identity.browser_grant.is_some());
        let directory = tempfile::tempdir().unwrap();
        let mut picture_checkpoint = crate::visual_preview_http::picture_authority(
            &identity,
            &directory.path().join("unused.sqlite"),
        )
        .unwrap();
        picture_checkpoint().unwrap();
        let response = Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header("content-range", "bytes 0-5/6")
            .body(Body::from("secret"))
            .unwrap();
        let response = sessions
            .response_grant(&api_request(&cookie))
            .unwrap()
            .wrap(response);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], "bytes 0-5/6");
        sessions.handle(request(Method::DELETE, Some(&cookie)));
        assert_eq!(picture_checkpoint().unwrap_err().code, "NOT_AUTHORIZED");
        assert!(!directory.path().join("unused.sqlite").exists());
        assert!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn idle_cookie_stream_closes_on_logout_and_drops_the_underlying_producer() {
        let sessions = BrowserSession::new();
        let cookie = open(&sessions);
        let (sender, receiver) = tokio::sync::mpsc::channel::<axum::body::Bytes>(1);
        let body = Body::from_stream(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move {
                receiver
                    .recv()
                    .await
                    .map(|bytes| (Ok::<_, std::io::Error>(bytes), receiver))
            },
        ));
        let mut stream = sessions
            .response_grant(&api_request(&cookie))
            .unwrap()
            .wrap(Response::new(body))
            .into_body()
            .into_data_stream();
        sender
            .send(axum::body::Bytes::from_static(b"first"))
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "first");
        let revoke = sessions.clone();
        let cookie_copy = cookie.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            revoke.handle(request(Method::DELETE, Some(&cookie_copy)));
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .unwrap()
                .is_none()
        );
        task.await.unwrap();
        assert!(sender.is_closed());
    }
    #[tokio::test]
    async fn ttl_is_checked_again_after_waiting_for_a_body_chunk() {
        let grant = Arc::new(Grant {
            expires: Instant::now() + Duration::from_millis(20),
            revoked: AtomicBool::new(false),
        });
        let body = Body::from_stream(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok::<_, std::io::Error>("too late")
        }));
        let response = ResponseGrant(grant).wrap(Response::new(body));
        assert!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn body_guard_does_not_prefetch_and_preserves_transport_errors() {
        use std::sync::atomic::AtomicUsize;
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = polls.clone();
        let body = Body::from_stream(futures_util::stream::poll_fn(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Ready(Some(Err::<axum::body::Bytes, _>(std::io::Error::other(
                "transport failed",
            ))))
        }));
        let grant = Arc::new(Grant {
            expires: Instant::now() + TTL,
            revoked: AtomicBool::new(false),
        });
        let response = ResponseGrant(grant).wrap(Response::new(body));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        let mut stream = response.into_body().into_data_stream();
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }
}
