//! Native HTTP and secure-WebSocket transport boundary.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{Path, Query, State, WebSocketUpgrade, ws::rejection::WebSocketUpgradeRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};

use crate::{
    AccountId, ChallengeId, ConnectionId, GoogleOidcClient, Handle, InvitationId, LobbyId, MatchId,
    NewOidcAttempt, Participants, SessionCookiePolicy, SwitchyCommandJournal,
    accept_challenge_into_lobby, account_for_handle, assign_handle, cancel_lobby,
    claim_oidc_attempt, create_challenge, create_oidc_attempt, create_stored_session,
    generate_invitation, handle_for_account, link_google_identity, load_lobby, lobbies_for_account,
    lobby_readiness, offer_rematch, pending_challenges_for, pending_rematches_for, realtime::Hub,
    redeem_invitation_token_into_lobby, resolve_session_token, revoke_session_token,
};
use pwmtf_protocol::MAX_CLIENT_MESSAGE_BYTES;
use switchy_database::Database;
use thiserror::Error;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::{SetResponseHeader, SetResponseHeaderLayer},
};

/// Canonical production web origin.
pub const CANONICAL_ORIGIN: &str = "https://pwmtf.hyperchad.dev";
/// OIDC callback path under the canonical origin.
pub const OIDC_CALLBACK_PATH: &str = "/auth/google/callback";
const OIDC_BINDING_COOKIE: &str = "__Host-pwmtf_oidc";
const OIDC_MAX_CODE_BYTES: usize = 1_024;
const OIDC_MAX_STATE_BYTES: usize = 256;
const OIDC_ATTEMPT_LIFETIME_MS: u64 = 10 * 60 * 1_000;
const SESSION_LIFETIME_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const INVITATION_LIFETIME_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_JSON_BYTES: usize = 1_024;
const CACHE_CONTROL_DYNAMIC: &str = "no-store";
const CACHE_CONTROL_ASSET: &str = "no-cache";
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'self'; media-src 'self'; worker-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'self' https://accounts.google.com; frame-ancestors 'none'";
const PERMISSIONS_POLICY: &str = "camera=(), geolocation=(), microphone=()";
const READINESS_MAX_AGE_MS: u64 = 5_000;

/// Runtime services required by native HTTP/OIDC/WebSocket entry points.
pub struct HttpState {
    pub(crate) db: Arc<dyn Database>,
    oidc: Option<Arc<GoogleOidcClient>>,
    sessions: SessionCookiePolicy,
    origins: BTreeSet<String>,
    public_origin: String,
    development_login: bool,
    pub(crate) hub: Hub,
    scheduler_healthy: AtomicBool,
    scheduler_last_success_ms: AtomicU64,
    web_root: Option<PathBuf>,
}

impl HttpState {
    /// Polls and durably applies every currently due authoritative deadline.
    ///
    /// Each due match is applied under its own per-match authority lock
    /// concurrently with the others, and its resulting state (with a `Timeout`
    /// transition description) is broadcast to that match's subscribers before
    /// another command can advance it. Stale or already-applied deadlines are
    /// harmless no-ops, so each timeout applies exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when any due timeout cannot be durably
    /// accepted or its resulting state cannot be encoded; other matches'
    /// timeouts are still applied.
    pub async fn poll_due_deadlines(
        &self,
        now: crate::DeadlineMillis,
    ) -> Result<usize, TransportError> {
        let mut tasks = tokio::task::JoinSet::new();
        for unit in self.hub.units() {
            if unit
                .deadline()
                .is_some_and(|deadline| deadline.due_at <= now)
            {
                tasks.spawn(async move { unit.apply_due_deadline(now).await });
            }
        }
        let mut count = 0;
        let mut failed = false;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(true)) => count += 1,
                Ok(Ok(false)) => {}
                Ok(Err(_)) | Err(_) => failed = true,
            }
        }
        if failed {
            return Err(TransportError::Recovery);
        }
        self.scheduler_last_success_ms
            .store(now.value(), Ordering::Release);
        self.scheduler_healthy.store(true, Ordering::Release);
        Ok(count)
    }

    /// Deletes expired durable sessions and OIDC attempts.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when either stored-record cleanup fails.
    pub async fn cleanup_expired_auth_records(&self, now: u64) -> Result<(), TransportError> {
        crate::cleanup_expired_sessions(&*self.db, now).await?;
        crate::cleanup_oidc_attempts(&*self.db, now).await?;
        Ok(())
    }

    /// Removes every durable lobby readiness row. Lobby presence is in-memory,
    /// so no presence can be live when the process starts.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when the delete fails.
    pub async fn reset_lobby_presence(&self) -> Result<(), TransportError> {
        crate::clear_lobby_presence(&*self.db).await?;
        Ok(())
    }

    /// Returns the earliest currently scheduled deadline.
    #[allow(clippy::unused_async)]
    pub async fn next_deadline(&self) -> Option<crate::ScheduledDeadline> {
        self.hub
            .units()
            .iter()
            .filter_map(|unit| unit.deadline())
            .min_by_key(|deadline| deadline.due_at)
    }

    /// Recovers every durable match before serving traffic.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when any canonical row or command history is
    /// absent, malformed, or incompatible.
    pub async fn recover_all_matches(&self) -> Result<usize, TransportError> {
        let journal = SwitchyCommandJournal::new(Arc::clone(&self.db));
        let ids = journal
            .match_ids()
            .await
            .map_err(|_| TransportError::Recovery)?;
        let mut recovered = 0;
        for match_id in ids {
            let participants = journal
                .participants(match_id)
                .await
                .map_err(|_| TransportError::Recovery)?;
            self.match_unit(match_id, participants).await?;
            recovered += 1;
        }
        Ok(recovered)
    }

    /// Marks the authoritative scheduler unhealthy after a failed poll.
    pub fn mark_scheduler_unhealthy(&self) {
        self.scheduler_healthy.store(false, Ordering::Release);
    }

    fn scheduler_is_ready(&self, now: u64) -> bool {
        scheduler_is_ready(
            &self.scheduler_healthy,
            &self.scheduler_last_success_ms,
            now,
        )
    }

    /// Returns whether this state's durable database and recovered in-process
    /// authority are ready to receive traffic.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when durable match rows are unavailable,
    /// malformed, or do not exactly match the authority loaded at startup.
    pub async fn verify_readiness(&self, now: u64) -> Result<(), TransportError> {
        if !self.scheduler_is_ready(now) {
            return Err(TransportError::Recovery);
        }
        let journal = SwitchyCommandJournal::new(Arc::clone(&self.db));
        let mut durable_ids = journal
            .match_ids()
            .await
            .map_err(|_| TransportError::Recovery)?;
        durable_ids.sort_unstable();
        if durable_ids != self.hub.match_ids() {
            return Err(TransportError::Recovery);
        }
        let mut loaded_heads = Vec::new();
        for match_id in durable_ids {
            let unit = self.hub.unit(match_id).ok_or(TransportError::Recovery)?;
            let head = unit.head().await.ok_or(TransportError::Recovery)?;
            loaded_heads.push((match_id, head));
        }
        for (match_id, (participants, revision, state, deadline)) in loaded_heads {
            let (head_participants, head_revision, head_state, head_deadline) = journal
                .canonical_head(match_id)
                .await
                .map_err(|_| TransportError::Recovery)?;
            if participants != head_participants
                || revision != head_revision
                || state != head_state
                || deadline != head_deadline
            {
                return Err(TransportError::Recovery);
            }
        }
        Ok(())
    }

    /// Creates production transport state with exact origin validation.
    #[must_use]
    pub fn production(db: Arc<dyn Database>, oidc: Arc<GoogleOidcClient>) -> Self {
        Self::new(db, oidc, [CANONICAL_ORIGIN])
    }

    /// Creates explicitly insecure localhost state for username-only development login.
    #[cfg(feature = "insecure")]
    #[must_use]
    pub fn development(db: Arc<dyn Database>, origin: &str) -> Self {
        Self {
            db,
            oidc: None,
            sessions: SessionCookiePolicy::development(),
            origins: BTreeSet::from([origin.to_owned()]),
            public_origin: origin.to_owned(),
            development_login: true,
            hub: Hub::default(),
            scheduler_healthy: AtomicBool::new(false),
            scheduler_last_success_ms: AtomicU64::new(0),
            web_root: None,
        }
    }

    /// Creates state with an explicit exact-origin allowlist for tests/local adapters.
    #[must_use]
    pub fn new(
        db: Arc<dyn Database>,
        oidc: Arc<GoogleOidcClient>,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::with_identity_provider(db, Some(oidc), origins)
    }

    /// Creates production-policy state without any identity provider, for
    /// native integration tests and smoke adapters that seed hash-only
    /// sessions directly. Google login routes respond with 404 and no
    /// development login exists.
    #[must_use]
    pub fn without_identity_provider(
        db: Arc<dyn Database>,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::with_identity_provider(db, None, origins)
    }

    fn with_identity_provider(
        db: Arc<dyn Database>,
        oidc: Option<Arc<GoogleOidcClient>>,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            db,
            oidc,
            sessions: SessionCookiePolicy::production(),
            origins: origins.into_iter().map(Into::into).collect(),
            public_origin: CANONICAL_ORIGIN.to_owned(),
            development_login: false,
            hub: Hub::default(),
            scheduler_healthy: AtomicBool::new(false),
            scheduler_last_success_ms: AtomicU64::new(0),
            web_root: None,
        }
    }

    /// Configures the verified production web-bundle directory.
    #[must_use]
    pub fn with_web_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.web_root = Some(path.into());
        self
    }

    /// Returns the configured production web-bundle directory.
    #[must_use]
    pub fn web_root(&self) -> Option<&std::path::Path> {
        self.web_root.as_deref()
    }
}

/// Builds the native HTTP/OIDC/WebSocket router.
#[allow(clippy::too_many_lines)]
pub fn router(state: Arc<HttpState>) -> Router {
    let web_root = state.web_root().map(PathBuf::from);
    let mut router = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route("/pwmtf-bundle-manifest.json", get(hidden_bundle_manifest))
        .route("/api/auth/runtime", get(runtime_auth))
        .route("/api/session", get(session_profile).delete(logout))
        .route("/api/profile/handle", axum::routing::put(set_handle))
        .route(
            "/api/challenges",
            get(list_pending_challenges).post(challenge_handle),
        )
        .route(
            "/api/challenges/{challenge_id}/accept",
            axum::routing::post(accept_challenge),
        )
        .route(
            "/api/invitations",
            axum::routing::post(create_invitation_link),
        )
        .route(
            "/api/invitations/redeem",
            axum::routing::post(redeem_invitation_link),
        )
        .route("/api/lobbies", get(list_account_lobbies))
        .route(
            "/api/lobbies/{lobby_id}",
            get(lobby_status).delete(cancel_waiting_lobby),
        )
        .route(
            "/api/lobbies/{lobby_id}/ready",
            axum::routing::post(ready_waiting_lobby),
        )
        .route("/api/rematches", get(list_pending_rematches))
        .route("/api/matches/{match_id}", get(match_access))
        .route(
            "/api/matches/{match_id}/rematch",
            axum::routing::post(create_rematch_offer),
        )
        .route(
            "/api/matches/{match_id}/rematch/accept",
            axum::routing::post(accept_rematch_offer),
        )
        .route("/api/{*path}", axum::routing::any(api_not_found))
        .route("/auth/{*path}", axum::routing::any(api_not_found))
        .route("/ws", get(websocket));
    #[cfg(feature = "insecure")]
    if state.development_login {
        router = router.route("/auth/development", axum::routing::post(development_login));
    } else {
        router = router
            .route("/auth/google/start", axum::routing::post(google_start))
            .route(OIDC_CALLBACK_PATH, get(google_callback));
    }
    #[cfg(not(feature = "insecure"))]
    {
        router = router
            .route("/auth/google/start", axum::routing::post(google_start))
            .route(OIDC_CALLBACK_PATH, get(google_callback));
    }
    let router = router;
    let router = if let Some(root) = web_root {
        let index = ServeFile::new(root.join("index.html"));
        let assets = SetResponseHeader::overriding(
            ServeDir::new(&root)
                .append_index_html_on_directories(true)
                .fallback(index),
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL_ASSET),
        );
        router.fallback_service(assets)
    } else {
        router
    };
    router
        .layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=63072000; includeSubDomains"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL_DYNAMIC),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static(PERMISSIONS_POLICY),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

fn scheduler_is_ready(healthy: &AtomicBool, last_success_ms: &AtomicU64, now: u64) -> bool {
    healthy.load(Ordering::Acquire)
        && now.saturating_sub(last_success_ms.load(Ordering::Acquire)) <= READINESS_MAX_AGE_MS
}

async fn readiness(State(state): State<Arc<HttpState>>) -> Result<&'static str, TransportError> {
    tokio::time::timeout(
        Duration::from_secs(2),
        state.verify_readiness(unix_millis()?),
    )
    .await
    .map_err(|_| TransportError::Internal)??;
    Ok("ready")
}

async fn hidden_bundle_manifest() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[derive(Debug, serde::Serialize)]
struct RuntimeAuth {
    development_login: bool,
    google_login: bool,
}

async fn runtime_auth(State(state): State<Arc<HttpState>>) -> axum::Json<RuntimeAuth> {
    axum::Json(RuntimeAuth {
        development_login: state.development_login,
        google_login: state.oidc.is_some(),
    })
}

#[derive(Debug, serde::Serialize)]
struct SessionProfile {
    handle: Option<String>,
    development_login: bool,
}

async fn session_profile(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<axum::Json<SessionProfile>, TransportError> {
    let account = authenticated_account(&state, &headers).await?;
    let handle = handle_for_account(&*state.db, account).await?;
    Ok(axum::Json(session_profile_response(
        handle,
        state.development_login,
    )))
}

async fn logout(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Response, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .ok_or(TransportError::Unauthenticated)?;
    let token = state.sessions.parse_request(cookie)?;
    revoke_session_token(&*state.db, token.expose()).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.sessions.clear_cookie())
            .map_err(|_| TransportError::Internal)?,
    );
    Ok(response)
}

async fn authenticated_account(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<AccountId, TransportError> {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .ok_or(TransportError::Unauthenticated)?;
    let token = state.sessions.parse_request(cookie)?;
    resolve_session_token(&*state.db, token.expose(), unix_millis()?)
        .await?
        .ok_or(TransportError::Unauthenticated)
}

#[cfg(feature = "insecure")]
#[derive(Debug, serde::Deserialize)]
struct DevelopmentLoginRequest {
    username: String,
}

#[cfg(feature = "insecure")]
async fn development_login(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    payload: Result<axum::Json<DevelopmentLoginRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, TransportError> {
    if !state.development_login || state.oidc.is_some() {
        return Err(TransportError::MatchNotFound);
    }
    ensure_json_bound(&headers)?;
    validate_state_change_origin(&headers, &state.origins)?;
    let request = payload.map_err(|_| TransportError::InvalidRequest)?.0;
    let handle = Handle::new(&request.username).map_err(|_| TransportError::InvalidRequest)?;
    let account = development_account_id(&handle);
    assign_handle(&*state.db, account, &handle).await?;
    let now = unix_millis()?;
    let token = create_stored_session(
        &*state.db,
        crate::Session {
            account,
            expires_at: now.saturating_add(SESSION_LIFETIME_MS),
            last_used_at: now,
        },
    )
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(
            &state
                .sessions
                .set_cookie(&token, SESSION_LIFETIME_MS / 1_000)?,
        )
        .map_err(|_| TransportError::Internal)?,
    );
    Ok(response)
}

async fn google_start(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Response, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let now = unix_millis()?;
    let attempt = create_oidc_attempt(
        &*state.db,
        now,
        now.saturating_add(OIDC_ATTEMPT_LIFETIME_MS),
    )
    .await?;
    let oidc = state.oidc.as_ref().ok_or(TransportError::MatchNotFound)?;
    let location = oidc.authorization_url(attempt.attempt());
    let mut response = Redirect::to(&location).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&oidc_binding_cookie(&attempt))
            .map_err(|_| TransportError::Internal)?,
    );
    Ok(response)
}

#[derive(Debug, serde::Deserialize)]
struct OidcCallbackQuery {
    code: String,
    state: String,
}

fn validate_oidc_callback(query: &OidcCallbackQuery) -> Result<(), TransportError> {
    if query.code.is_empty()
        || query.code.len() > OIDC_MAX_CODE_BYTES
        || query.state.is_empty()
        || query.state.len() > OIDC_MAX_STATE_BYTES
        || query.code.chars().any(char::is_control)
        || query.state.chars().any(char::is_control)
    {
        Err(TransportError::InvalidRequest)
    } else {
        Ok(())
    }
}

async fn google_callback(
    State(state): State<Arc<HttpState>>,
    Query(query): Query<OidcCallbackQuery>,
    headers: HeaderMap,
) -> Result<Response, TransportError> {
    validate_oidc_callback(&query)?;
    let binding = parse_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .ok_or(TransportError::Unauthenticated)?,
        OIDC_BINDING_COOKIE,
    )?;
    let (attempt_id, binding) = binding
        .split_once('.')
        .ok_or(TransportError::Unauthenticated)?;
    let claimed = claim_oidc_attempt(
        &*state.db,
        attempt_id,
        &query.state,
        binding,
        unix_millis()?,
    )
    .await?;
    let oidc = state.oidc.as_ref().ok_or(TransportError::MatchNotFound)?;
    let identity = oidc
        .exchange_callback(&query.code, &query.state, claimed.attempt())
        .await?;
    let account = account_id(&identity);
    link_google_identity(&*state.db, &identity, account).await?;
    let now = unix_millis()?;
    let token = create_stored_session(
        &*state.db,
        crate::Session {
            account,
            expires_at: now.saturating_add(SESSION_LIFETIME_MS),
            last_used_at: now,
        },
    )
    .await?;
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(
            &state
                .sessions
                .set_cookie(&token, SESSION_LIFETIME_MS / 1_000)?,
        )
        .map_err(|_| TransportError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "__Host-pwmtf_oidc=; Path=/; Max-Age=0; SameSite=Lax; Secure; HttpOnly; Priority=High",
        ),
    );
    Ok(response)
}

/// Upgrades the single authenticated session socket. Origin and session are
/// checked before the upgrade; every match and lobby subscription is then
/// negotiated over the socket itself.
async fn websocket(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, TransportError> {
    validate_origin(&headers, &state.origins)?;
    let account = authenticated_account(&state, &headers).await?;
    let upgrade = upgrade.map_err(|_| TransportError::InvalidRequest)?;
    let connection = ConnectionId::new(random_u128()?);
    Ok(upgrade
        .max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| crate::realtime::run_session(state, socket, account, connection))
        .into_response())
}

fn validate_state_change_origin(
    headers: &HeaderMap,
    origins: &BTreeSet<String>,
) -> Result<(), TransportError> {
    let origin = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get("x-pwmtf-origin"))
        .and_then(|value| value.to_str().ok())
        .ok_or(TransportError::InvalidOrigin)?;
    if origins.contains(origin) {
        Ok(())
    } else {
        Err(TransportError::InvalidOrigin)
    }
}

fn validate_origin(headers: &HeaderMap, origins: &BTreeSet<String>) -> Result<(), TransportError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(TransportError::InvalidOrigin)?;
    if origins.contains(origin) {
        Ok(())
    } else {
        Err(TransportError::InvalidOrigin)
    }
}

fn oidc_binding_cookie(attempt: &NewOidcAttempt) -> String {
    format!(
        "{OIDC_BINDING_COOKIE}={}.{}; Path=/; Max-Age=600; SameSite=Lax; Secure; HttpOnly; Priority=High",
        attempt.attempt_id(),
        attempt.browser_binding()
    )
}

fn parse_cookie<'a>(header: &'a str, wanted: &str) -> Result<&'a str, TransportError> {
    if header.len() > 4_096 || header.chars().any(|value| matches!(value, '\r' | '\n')) {
        return Err(TransportError::Unauthenticated);
    }
    let mut found = None;
    for pair in header.split(';') {
        let (name, value) = pair
            .trim()
            .split_once('=')
            .ok_or(TransportError::Unauthenticated)?;
        if name == wanted {
            if found.is_some() {
                return Err(TransportError::Unauthenticated);
            }
            found = Some(value);
        }
    }
    found.ok_or(TransportError::Unauthenticated)
}

#[cfg(feature = "insecure")]
fn development_account_id(handle: &Handle) -> AccountId {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"pwmtf-local-development");
    hash.update([0]);
    hash.update(handle.as_str().as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    AccountId::new(u128::from_be_bytes(bytes))
}

fn account_id(identity: &crate::GoogleIdentity) -> AccountId {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    hash.update(identity.issuer().as_bytes());
    hash.update([0]);
    hash.update(identity.subject().as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    AccountId::new(u128::from_be_bytes(bytes))
}

pub fn random_u64() -> Result<u64, TransportError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| TransportError::Internal)?;
    Ok(u64::from_be_bytes(bytes))
}

pub fn random_u128() -> Result<u128, TransportError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| TransportError::Internal)?;
    Ok(u128::from_be_bytes(bytes))
}

pub fn unix_millis() -> Result<u64, TransportError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TransportError::Internal)?;
    u64::try_from(duration.as_millis()).map_err(|_| TransportError::Internal)
}

/// Secret-safe HTTP/WebSocket transport rejection.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Request has no valid authenticated session.
    #[error("request is not authenticated")]
    Unauthenticated,
    /// WebSocket origin is absent or not exactly allowed.
    #[error("request origin is not allowed")]
    InvalidOrigin,
    /// Request body or path data is malformed or exceeds fixed bounds.
    #[error("request is invalid")]
    InvalidRequest,
    /// Referenced social record does not exist.
    #[error("social record not found")]
    SocialNotFound,
    /// Match does not exist or cannot be recovered.
    #[error("match not found")]
    MatchNotFound,
    /// Requested participants do not match durable canonical membership.
    #[error("match subscription is invalid")]
    InvalidSubscription,
    /// Durable match recovery failed integrity validation.
    #[error("match recovery failed")]
    Recovery,
    /// Internal transport operation failed.
    #[error("transport operation failed")]
    Internal,
    /// Cookie policy failed.
    #[error(transparent)]
    Cookie(#[from] crate::CookieError),
    /// OIDC attempt persistence failed.
    #[error(transparent)]
    Attempt(#[from] crate::OidcAttemptStoreError),
    /// OIDC exchange or verification failed.
    #[error(transparent)]
    Oidc(#[from] crate::GoogleOidcError),
    /// Identity persistence failed.
    #[error(transparent)]
    Identity(#[from] crate::IdentityStoreError),
    /// Session persistence failed.
    #[error(transparent)]
    Session(#[from] crate::SessionStoreError),
    /// Account-profile persistence failed.
    #[error(transparent)]
    Profile(#[from] crate::ProfileStoreError),
    /// Waiting-lobby persistence or lifecycle transition failed.
    #[error(transparent)]
    Lobby(#[from] crate::LobbyStoreError),
    /// Rematch persistence or lifecycle transition failed.
    #[error(transparent)]
    Rematch(#[from] crate::RematchStoreError),
    /// Social persistence or lifecycle transition failed.
    #[error(transparent)]
    Social(#[from] crate::SocialStoreError),
}

impl TransportError {
    const fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::InvalidOrigin
            | Self::InvalidSubscription
            | Self::Lobby(crate::LobbyStoreError::Unauthorized)
            | Self::Rematch(crate::RematchStoreError::Unauthorized)
            | Self::Social(crate::SocialStoreError::Unauthorized) => StatusCode::FORBIDDEN,
            Self::MatchNotFound
            | Self::SocialNotFound
            | Self::Attempt(crate::OidcAttemptStoreError::NotFound)
            | Self::Lobby(crate::LobbyStoreError::NotFound)
            | Self::Rematch(crate::RematchStoreError::NotFound)
            | Self::Social(crate::SocialStoreError::NotFound) => StatusCode::NOT_FOUND,
            Self::Profile(
                crate::ProfileStoreError::AccountConflict
                | crate::ProfileStoreError::HandleConflict,
            )
            | Self::Lobby(
                crate::LobbyStoreError::NotWaiting
                | crate::LobbyStoreError::NotConnected
                | crate::LobbyStoreError::NotReady,
            )
            | Self::Rematch(
                crate::RematchStoreError::NotCompleted
                | crate::RematchStoreError::OfferExists
                | crate::RematchStoreError::AlreadyAccepted
                | crate::RematchStoreError::SelfAccept,
            )
            | Self::Social(
                crate::SocialStoreError::DuplicateChallenge
                | crate::SocialStoreError::Expired
                | crate::SocialStoreError::Revoked
                | crate::SocialStoreError::AlreadyUsed,
            ) => StatusCode::CONFLICT,
            Self::Oidc(
                crate::GoogleOidcError::Discovery
                | crate::GoogleOidcError::TokenExchange
                | crate::GoogleOidcError::MissingIdToken,
            ) => StatusCode::BAD_GATEWAY,
            Self::Cookie(_)
            | Self::InvalidRequest
            | Self::Attempt(
                crate::OidcAttemptStoreError::Expired
                | crate::OidcAttemptStoreError::Mismatch
                | crate::OidcAttemptStoreError::AlreadyUsed,
            )
            | Self::Oidc(
                crate::GoogleOidcError::Callback | crate::GoogleOidcError::InvalidIdToken,
            )
            | Self::Profile(crate::ProfileStoreError::Social(_))
            | Self::Lobby(crate::LobbyStoreError::InvalidDeadline)
            | Self::Social(
                crate::SocialStoreError::SelfChallenge
                | crate::SocialStoreError::SelfInvitation
                | crate::SocialStoreError::InvalidExpiration
                | crate::SocialStoreError::Token(_),
            ) => StatusCode::BAD_REQUEST,
            Self::Internal
            | Self::Recovery
            | Self::Attempt(
                crate::OidcAttemptStoreError::InvalidExpiration
                | crate::OidcAttemptStoreError::Malformed
                | crate::OidcAttemptStoreError::Overflow
                | crate::OidcAttemptStoreError::Oidc(_)
                | crate::OidcAttemptStoreError::Database(_),
            )
            | Self::Oidc(
                crate::GoogleOidcError::Configuration
                | crate::GoogleOidcError::HttpClient
                | crate::GoogleOidcError::Randomness,
            )
            | Self::Identity(_)
            | Self::Session(_)
            | Self::Profile(
                crate::ProfileStoreError::Malformed | crate::ProfileStoreError::Database(_),
            )
            | Self::Lobby(
                crate::LobbyStoreError::Malformed
                | crate::LobbyStoreError::Overflow
                | crate::LobbyStoreError::Database(_),
            )
            | Self::Rematch(
                crate::RematchStoreError::Malformed
                | crate::RematchStoreError::Overflow
                | crate::RematchStoreError::Database(_),
            )
            | Self::Social(
                crate::SocialStoreError::Malformed
                | crate::SocialStoreError::Overflow
                | crate::SocialStoreError::Database(_),
            ) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    const fn public_message(status: StatusCode) -> &'static str {
        match status {
            StatusCode::BAD_REQUEST => "request is invalid",
            StatusCode::UNAUTHORIZED => "request is not authenticated",
            StatusCode::FORBIDDEN => "request is not allowed",
            StatusCode::NOT_FOUND => "resource not found",
            StatusCode::CONFLICT => "request conflicts with current state",
            StatusCode::BAD_GATEWAY => "identity provider is unavailable",
            _ => "transport operation failed",
        }
    }
}

impl IntoResponse for TransportError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        (status, Self::public_message(status)).into_response()
    }
}

fn session_profile_response(
    handle: Option<crate::Handle>,
    development_login: bool,
) -> SessionProfile {
    SessionProfile {
        handle: handle.map(|handle| handle.as_str().to_owned()),
        development_login,
    }
}

#[derive(Debug, serde::Deserialize)]
struct HandleRequest {
    handle: String,
}

#[derive(Debug, serde::Serialize)]
struct HandleResponse {
    handle: String,
}

async fn set_handle(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    payload: Result<axum::Json<HandleRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<axum::Json<HandleResponse>, TransportError> {
    ensure_json_bound(&headers)?;
    validate_state_change_origin(&headers, &state.origins)?;
    let account = authenticated_account(&state, &headers).await?;
    let request = payload.map_err(|_| TransportError::InvalidRequest)?.0;
    let handle = Handle::new(&request.handle).map_err(|_| TransportError::InvalidRequest)?;
    assign_handle(&*state.db, account, &handle).await?;
    Ok(axum::Json(HandleResponse {
        handle: handle.as_str().to_owned(),
    }))
}

#[derive(Debug, serde::Serialize)]
struct LobbyResponse {
    lobby_id: String,
}

#[derive(Debug, serde::Serialize)]
struct ChallengeResponse {
    challenge_id: String,
}

#[derive(Debug, serde::Serialize)]
struct PendingChallengeResponse {
    challenge_id: String,
    from_handle: String,
}

async fn list_pending_challenges(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<PendingChallengeResponse>>, TransportError> {
    let account = authenticated_account(&state, &headers).await?;
    let pending = pending_challenges_for(&*state.db, account).await?;
    let mut response = Vec::with_capacity(pending.len());
    for (challenge_id, challenger) in pending {
        let handle = handle_for_account(&*state.db, challenger)
            .await?
            .ok_or(TransportError::Internal)?;
        response.push(PendingChallengeResponse {
            challenge_id: challenge_id.value().to_string(),
            from_handle: handle.as_str().to_owned(),
        });
    }
    Ok(axum::Json(response))
}

async fn challenge_handle(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    payload: Result<axum::Json<HandleRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<axum::Json<ChallengeResponse>, TransportError> {
    ensure_json_bound(&headers)?;
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let request = payload.map_err(|_| TransportError::InvalidRequest)?.0;
    let handle = Handle::new(&request.handle).map_err(|_| TransportError::InvalidRequest)?;
    let target = account_for_handle(&*state.db, &handle)
        .await?
        .ok_or(TransportError::SocialNotFound)?;
    let id = ChallengeId::new(random_u128()?);
    create_challenge(&*state.db, id, actor, target).await?;
    state.notify_social(&[actor, target]);
    Ok(axum::Json(ChallengeResponse {
        challenge_id: id.value().to_string(),
    }))
}

async fn accept_challenge(
    State(state): State<Arc<HttpState>>,
    Path(challenge_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<LobbyResponse>, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let requested_lobby_id = LobbyId::new(random_u128()?);
    let (participants, lobby_id) = accept_challenge_into_lobby(
        &*state.db,
        ChallengeId::new(challenge_id),
        actor,
        requested_lobby_id,
    )
    .await?;
    state.notify_social(&[participants.player_one, participants.player_two]);
    Ok(axum::Json(lobby_response(lobby_id)))
}

#[derive(Debug, serde::Serialize)]
struct InvitationResponse {
    invitation_url: String,
    expires_at_ms: u64,
}

async fn create_invitation_link(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<axum::Json<InvitationResponse>, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let now = unix_millis()?;
    let expires_at = now.saturating_add(INVITATION_LIFETIME_MS);
    let token = generate_invitation(
        &*state.db,
        InvitationId::new(random_u128()?),
        actor,
        expires_at,
        now,
    )
    .await?;
    Ok(axum::Json(InvitationResponse {
        invitation_url: format!("{}/?invite={}", state.public_origin, token.expose()),
        expires_at_ms: expires_at,
    }))
}

#[derive(Debug, serde::Deserialize)]
struct InvitationRedeemRequest {
    token: String,
}

async fn redeem_invitation_link(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    payload: Result<axum::Json<InvitationRedeemRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<axum::Json<LobbyResponse>, TransportError> {
    ensure_json_bound(&headers)?;
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let request = payload.map_err(|_| TransportError::InvalidRequest)?.0;
    let lobby_id = LobbyId::new(random_u128()?);
    let participants = redeem_invitation_token_into_lobby(
        &*state.db,
        &request.token,
        actor,
        lobby_id,
        unix_millis()?,
    )
    .await?;
    state.notify_social(&[participants.player_one, participants.player_two]);
    Ok(axum::Json(lobby_response(lobby_id)))
}

/// Lobby view. Presence and readiness are reported only for the requesting
/// participant and "the opponent"; participant identifiers are never exposed.
#[derive(Debug, serde::Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct LobbyStatusResponse {
    lobby_id: String,
    status: &'static str,
    match_id: Option<String>,
    both_ready: bool,
    self_present: bool,
    self_ready: bool,
    opponent_present: bool,
    opponent_ready: bool,
}

/// Live presence/readiness of one waiting lobby, from the requester's seat.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct LobbyLiveness {
    self_present: bool,
    self_ready: bool,
    opponent_present: bool,
    opponent_ready: bool,
}

impl HttpState {
    async fn lobby_liveness(
        &self,
        record: crate::LobbyRecord,
        actor: AccountId,
    ) -> Result<LobbyLiveness, TransportError> {
        if record.status != crate::LobbyStatus::Waiting {
            return Ok(LobbyLiveness::default());
        }
        let opponent = if actor == record.participants.player_one {
            record.participants.player_two
        } else {
            record.participants.player_one
        };
        let live = self.hub.lobby_presences(record.id);
        let (player_one_ready, player_two_ready) =
            lobby_readiness(&*self.db, record.id, &live).await?;
        let (self_ready, opponent_ready) = if actor == record.participants.player_one {
            (player_one_ready, player_two_ready)
        } else {
            (player_two_ready, player_one_ready)
        };
        Ok(LobbyLiveness {
            self_present: self.hub.account_present(record.id, actor),
            self_ready,
            opponent_present: self.hub.account_present(record.id, opponent),
            opponent_ready,
        })
    }
}

async fn list_account_lobbies(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<LobbyStatusResponse>>, TransportError> {
    let actor = authenticated_account(&state, &headers).await?;
    let records = lobbies_for_account(&*state.db, actor).await?;
    let mut response = Vec::with_capacity(records.len());
    for record in records {
        let liveness = state.lobby_liveness(record, actor).await?;
        response.push(lobby_status_response(record, liveness));
    }
    Ok(axum::Json(response))
}

#[derive(Debug, serde::Serialize)]
struct MatchAccessResponse {
    player: u8,
    revision: u64,
    active_player: u8,
    completed: bool,
    deadline_at_ms: Option<u64>,
    server_time_ms: u64,
}

fn match_access_response(
    participants: Participants,
    actor: AccountId,
    revision: u64,
    state: &pwmtf_game_domain::MatchState,
    deadline: Option<crate::ScheduledDeadline>,
    server_time_ms: u64,
) -> Result<MatchAccessResponse, TransportError> {
    let player = if actor == participants.player_one {
        1
    } else if actor == participants.player_two {
        2
    } else {
        return Err(TransportError::InvalidSubscription);
    };
    let completed = matches!(state.status(), pwmtf_game_domain::MatchStatus::Completed(_));
    Ok(MatchAccessResponse {
        player,
        revision,
        active_player: if completed {
            0
        } else {
            match state.active_player() {
                pwmtf_game_domain::Player::One => 1,
                pwmtf_game_domain::Player::Two => 2,
            }
        },
        completed,
        deadline_at_ms: deadline.map(|deadline| deadline.due_at.value()),
        server_time_ms,
    })
}

async fn match_access(
    State(state): State<Arc<HttpState>>,
    Path(match_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<MatchAccessResponse>, TransportError> {
    let actor = authenticated_account(&state, &headers).await?;
    let unit = state
        .authorized_match_unit(MatchId::new(match_id), actor)
        .await?;
    let (participants, revision, match_state, deadline) =
        unit.head().await.ok_or(TransportError::MatchNotFound)?;
    Ok(axum::Json(match_access_response(
        participants,
        actor,
        revision,
        &match_state,
        deadline,
        unix_millis()?,
    )?))
}

#[derive(Debug, serde::Serialize)]
struct PendingRematchResponse {
    previous_match_id: String,
}

async fn list_pending_rematches(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<PendingRematchResponse>>, TransportError> {
    let actor = authenticated_account(&state, &headers).await?;
    let pending = pending_rematches_for(&*state.db, actor).await?;
    Ok(axum::Json(
        pending
            .into_iter()
            .map(|offer| PendingRematchResponse {
                previous_match_id: offer.previous_match_id.value().to_string(),
            })
            .collect(),
    ))
}

async fn create_rematch_offer(
    State(state): State<Arc<HttpState>>,
    Path(match_id): Path<u128>,
    headers: HeaderMap,
) -> Result<StatusCode, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let match_id = MatchId::new(match_id);
    offer_rematch(&*state.db, match_id, actor).await?;
    if let Ok(participants) = SwitchyCommandJournal::new(Arc::clone(&state.db))
        .participants(match_id)
        .await
    {
        state.notify_social(&[participants.player_one, participants.player_two]);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn accept_rematch_offer(
    State(state): State<Arc<HttpState>>,
    Path(match_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<LobbyStatusResponse>, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let previous_match_id = MatchId::new(match_id);
    let new_match_id = MatchId::new(random_u128()?);
    let now = unix_millis()?;
    let _rematch = crate::accept_rematch(
        &*state.db,
        previous_match_id,
        new_match_id,
        actor,
        pwmtf_game_domain::RackSeed::new(random_u64()?),
        crate::DeadlineMillis::new(now),
    )
    .await?;
    let journal = SwitchyCommandJournal::new(Arc::clone(&state.db));
    let participants = journal
        .participants(new_match_id)
        .await
        .map_err(|_| TransportError::Recovery)?;
    state.match_unit(new_match_id, participants).await?;
    // A rematch has no lobby; `MatchStarted` carries lobby id 0.
    state.notify_match_started(participants, None, new_match_id);
    Ok(axum::Json(LobbyStatusResponse {
        lobby_id: String::new(),
        status: "started",
        match_id: Some(new_match_id.value().to_string()),
        both_ready: true,
        self_present: false,
        self_ready: false,
        opponent_present: false,
        opponent_ready: false,
    }))
}

async fn lobby_status(
    State(state): State<Arc<HttpState>>,
    Path(lobby_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<LobbyStatusResponse>, TransportError> {
    let actor = authenticated_account(&state, &headers).await?;
    let record = load_lobby(&*state.db, LobbyId::new(lobby_id))
        .await?
        .ok_or(TransportError::SocialNotFound)?;
    authorize_lobby_member(record.participants, actor)?;
    let liveness = state.lobby_liveness(record, actor).await?;
    Ok(axum::Json(lobby_status_response(record, liveness)))
}

/// Records explicit readiness. The actor must hold a live lobby presence
/// (an active `SubscribeLobby` on a session socket), else 409. The match
/// starts atomically when both participants are present and ready.
async fn ready_waiting_lobby(
    State(state): State<Arc<HttpState>>,
    Path(lobby_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<LobbyStatusResponse>, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let record = state
        .ready_lobby_member(LobbyId::new(lobby_id), actor)
        .await?;
    let liveness = state.lobby_liveness(record, actor).await?;
    Ok(axum::Json(lobby_status_response(record, liveness)))
}

async fn cancel_waiting_lobby(
    State(state): State<Arc<HttpState>>,
    Path(lobby_id): Path<u128>,
    headers: HeaderMap,
) -> Result<axum::Json<LobbyStatusResponse>, TransportError> {
    validate_state_change_origin(&headers, &state.origins)?;
    let actor = authenticated_account(&state, &headers).await?;
    let record = cancel_lobby(&*state.db, LobbyId::new(lobby_id), actor).await?;
    state.retire_lobby_presence(record.id).await;
    state.notify_lobby_changed(record);
    state.notify_social(&[
        record.participants.player_one,
        record.participants.player_two,
    ]);
    Ok(axum::Json(lobby_status_response(
        record,
        LobbyLiveness::default(),
    )))
}

fn authorize_lobby_member(
    participants: Participants,
    actor: AccountId,
) -> Result<(), TransportError> {
    if actor == participants.player_one || actor == participants.player_two {
        Ok(())
    } else {
        Err(TransportError::InvalidSubscription)
    }
}

fn lobby_status_response(
    record: crate::LobbyRecord,
    liveness: LobbyLiveness,
) -> LobbyStatusResponse {
    let (status, match_id) = match record.status {
        crate::LobbyStatus::Waiting => ("waiting", None),
        crate::LobbyStatus::Started { match_id } => ("started", Some(match_id.value().to_string())),
        crate::LobbyStatus::Cancelled { .. } => ("cancelled", None),
    };
    LobbyStatusResponse {
        lobby_id: record.id.value().to_string(),
        status,
        match_id,
        both_ready: liveness.self_ready && liveness.opponent_ready,
        self_present: liveness.self_present,
        self_ready: liveness.self_ready,
        opponent_present: liveness.opponent_present,
        opponent_ready: liveness.opponent_ready,
    }
}

fn lobby_response(lobby_id: LobbyId) -> LobbyResponse {
    LobbyResponse {
        lobby_id: lobby_id.value().to_string(),
    }
}

fn ensure_json_bound(headers: &HeaderMap) -> Result<(), TransportError> {
    let length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(TransportError::InvalidRequest)?;
    if length == 0 || length > MAX_JSON_BYTES {
        Err(TransportError::InvalidRequest)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_callback_query_is_bounded_and_control_free() {
        let valid = OidcCallbackQuery {
            code: "provider-code".to_owned(),
            state: "a".repeat(64),
        };
        assert!(validate_oidc_callback(&valid).is_ok());
        for invalid in [
            OidcCallbackQuery {
                code: String::new(),
                state: "a".repeat(64),
            },
            OidcCallbackQuery {
                code: "x".repeat(OIDC_MAX_CODE_BYTES + 1),
                state: "a".repeat(64),
            },
            OidcCallbackQuery {
                code: "provider\ncode".to_owned(),
                state: "a".repeat(64),
            },
            OidcCallbackQuery {
                code: "provider-code".to_owned(),
                state: "x".repeat(OIDC_MAX_STATE_BYTES + 1),
            },
        ] {
            assert!(matches!(
                validate_oidc_callback(&invalid),
                Err(TransportError::InvalidRequest)
            ));
        }
    }

    #[cfg(feature = "insecure")]
    #[test]
    fn development_account_ids_are_stable_and_namespaced() {
        let first = Handle::new("local_one").unwrap();
        let second = Handle::new("local_two").unwrap();
        assert_eq!(
            development_account_id(&first),
            development_account_id(&first)
        );
        assert_ne!(
            development_account_id(&first),
            development_account_id(&second)
        );
    }

    #[test]
    fn session_profile_exposes_only_public_profile_data() {
        let profile =
            session_profile_response(Some(crate::Handle::new("player_one").unwrap()), false);
        assert_eq!(profile.handle.as_deref(), Some("player_one"));
        assert!(!profile.development_login);
    }

    #[test]
    fn json_requests_require_a_small_nonzero_content_length() {
        let mut headers = HeaderMap::new();
        assert!(matches!(
            ensure_json_bound(&headers),
            Err(TransportError::InvalidRequest)
        ));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("32"));
        assert!(ensure_json_bound(&headers).is_ok());
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1025"));
        assert!(matches!(
            ensure_json_bound(&headers),
            Err(TransportError::InvalidRequest)
        ));
    }

    #[test]
    fn lobby_responses_do_not_disclose_participant_identifiers() {
        let response = lobby_response(LobbyId::new(3));
        assert_eq!(response.lobby_id, "3");
    }

    #[test]
    fn lobby_status_responses_preserve_authorization_and_started_match() {
        let participants = Participants {
            player_one: AccountId::new(1),
            player_two: AccountId::new(2),
        };
        assert!(authorize_lobby_member(participants, AccountId::new(1)).is_ok());
        assert!(matches!(
            authorize_lobby_member(participants, AccountId::new(3)),
            Err(TransportError::InvalidSubscription)
        ));
        let response = lobby_status_response(
            crate::LobbyRecord {
                id: LobbyId::new(4),
                participants,
                status: crate::LobbyStatus::Started {
                    match_id: MatchId::new(8),
                },
            },
            LobbyLiveness::default(),
        );
        assert_eq!(response.status, "started");
        assert_eq!(response.match_id.as_deref(), Some("8"));
    }

    #[test]
    fn match_access_derives_seat_from_durable_membership() {
        let participants = Participants {
            player_one: AccountId::new(1),
            player_two: AccountId::new(2),
        };
        let deadline = crate::ScheduledDeadline {
            id: crate::DeadlineId {
                revision: 4,
                player: pwmtf_game_domain::Player::One,
            },
            due_at: crate::DeadlineMillis::new(9_000),
        };
        let mut state = pwmtf_game_domain::MatchState::new(
            pwmtf_game_domain::RulesProfile::standard(),
            pwmtf_game_domain::PhysicsProfile::standard(),
            pwmtf_game_domain::TableGeometry::standard(),
            pwmtf_game_domain::RackSeed::new(3),
            pwmtf_game_domain::Player::One,
        )
        .unwrap();
        let response = match_access_response(
            participants,
            AccountId::new(1),
            4,
            &state,
            Some(deadline),
            8_000,
        )
        .unwrap();
        assert_eq!(response.player, 1);
        assert_eq!(response.revision, 4);
        assert_eq!(response.active_player, 1);
        assert!(!response.completed);
        assert_eq!(response.deadline_at_ms, Some(9_000));
        assert_eq!(response.server_time_ms, 8_000);
        assert_eq!(
            match_access_response(participants, AccountId::new(2), 0, &state, None, 8_000)
                .unwrap()
                .player,
            2
        );
        assert!(matches!(
            match_access_response(participants, AccountId::new(3), 0, &state, None, 8_000),
            Err(TransportError::InvalidSubscription)
        ));
        state.concede(pwmtf_game_domain::Player::Two).unwrap();
        let terminal =
            match_access_response(participants, AccountId::new(1), 5, &state, None, 8_000).unwrap();
        assert!(terminal.completed);
        assert_eq!(terminal.active_player, 0);
    }

    #[test]
    fn logout_and_state_changes_share_exact_origin_policy() {
        let allowed = BTreeSet::from([CANONICAL_ORIGIN.to_owned()]);
        let mut headers = HeaderMap::new();
        assert!(validate_state_change_origin(&headers, &allowed).is_err());
        headers.insert(header::ORIGIN, HeaderValue::from_static(CANONICAL_ORIGIN));
        assert!(validate_state_change_origin(&headers, &allowed).is_ok());
    }

    #[test]
    fn state_change_origin_header_is_exact_and_fail_closed() {
        let allowed = BTreeSet::from([CANONICAL_ORIGIN.to_owned()]);
        let mut headers = HeaderMap::new();
        assert!(validate_state_change_origin(&headers, &allowed).is_err());
        headers.insert(header::ORIGIN, HeaderValue::from_static(CANONICAL_ORIGIN));
        assert!(validate_state_change_origin(&headers, &allowed).is_ok());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        headers.insert("x-pwmtf-origin", HeaderValue::from_static(CANONICAL_ORIGIN));
        assert!(validate_state_change_origin(&headers, &allowed).is_err());
        headers.remove(header::ORIGIN);
        assert!(validate_state_change_origin(&headers, &allowed).is_ok());
    }

    #[test]
    fn websocket_origin_is_exact_and_fail_closed() {
        let allowed = BTreeSet::from([CANONICAL_ORIGIN.to_owned()]);
        let mut headers = HeaderMap::new();
        assert!(validate_origin(&headers, &allowed).is_err());
        headers.insert(header::ORIGIN, HeaderValue::from_static(CANONICAL_ORIGIN));
        assert!(validate_origin(&headers, &allowed).is_ok());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(validate_origin(&headers, &allowed).is_err());
    }

    #[test]
    fn scheduler_readiness_is_bounded_and_fail_closed() {
        let healthy = AtomicBool::new(false);
        let last_success = AtomicU64::new(0);
        assert!(!scheduler_is_ready(&healthy, &last_success, 1_000));
        last_success.store(1_000, Ordering::Release);
        healthy.store(true, Ordering::Release);
        assert!(scheduler_is_ready(&healthy, &last_success, 6_000));
        assert!(!scheduler_is_ready(&healthy, &last_success, 6_001));
        healthy.store(false, Ordering::Release);
        assert!(!scheduler_is_ready(&healthy, &last_success, 1_001));
    }

    #[test]
    fn transport_errors_expose_only_stable_public_categories() {
        let cases = [
            (
                TransportError::Unauthenticated,
                StatusCode::UNAUTHORIZED,
                "request is not authenticated",
            ),
            (
                TransportError::Social(crate::SocialStoreError::Unauthorized),
                StatusCode::FORBIDDEN,
                "request is not allowed",
            ),
            (
                TransportError::Social(crate::SocialStoreError::NotFound),
                StatusCode::NOT_FOUND,
                "resource not found",
            ),
            (
                TransportError::Profile(crate::ProfileStoreError::HandleConflict),
                StatusCode::CONFLICT,
                "request conflicts with current state",
            ),
            (
                TransportError::Oidc(crate::GoogleOidcError::TokenExchange),
                StatusCode::BAD_GATEWAY,
                "identity provider is unavailable",
            ),
            (
                TransportError::Social(crate::SocialStoreError::Malformed),
                StatusCode::INTERNAL_SERVER_ERROR,
                "transport operation failed",
            ),
        ];
        for (error, expected_status, expected_message) in cases {
            let status = error.status_code();
            assert_eq!(status, expected_status);
            assert_eq!(TransportError::public_message(status), expected_message);
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                assert_ne!(error.to_string(), expected_message);
            }
        }
    }

    #[test]
    fn oidc_binding_cookie_is_secure_host_only_and_strictly_parsed() {
        let attempt = crate::OidcAttempt::generate().unwrap();
        let created =
            NewOidcAttempt::from_parts("abc".to_owned(), attempt, "def".to_owned()).unwrap();
        let cookie = oidc_binding_cookie(&created);
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Priority=High"));
        assert!(!cookie.contains("Domain="));
        assert_eq!(
            parse_cookie("theme=dark; __Host-pwmtf_oidc=abc.def", OIDC_BINDING_COOKIE).unwrap(),
            "abc.def"
        );
    }
}
