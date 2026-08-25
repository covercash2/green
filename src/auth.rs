use std::{
    collections::HashMap,
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRef, FromRequestParts},
    http::request::Parts,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::{Mutex, RwLock};
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyRegistration, Webauthn,
    WebauthnBuilder,
};

use crate::{ServerState, error::Error, index::NavLink};

/// How long a session token remains valid after creation.
const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long a WebAuthn registration or authentication challenge remains valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long a one-time recovery code remains valid.
const OTC_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Admin,
    Guest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub rp_id: String,
    pub rp_origin: String,
    pub db_url: String,
    #[serde(default)]
    pub admin_users: Vec<String>,
    #[serde(default)]
    pub ntfy_url: Option<String>,
}

#[derive(Debug)]
pub struct SessionData {
    pub user_id: Uuid,
    pub username: String,
    pub role: Role,
    pub created_at: Instant,
}

/// Lightweight user info passed to Askama templates via the `auth_user` field.
#[derive(Debug, Clone)]
pub struct AuthUserInfo {
    pub username: String,
    pub role: Role,
}

impl AuthUserInfo {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

#[derive(Clone)]
pub struct AuthState {
    pub webauthn: Arc<Webauthn>,
    pub config: AuthConfig,
    pub db: PgPool,
    pub session_store: Arc<RwLock<HashMap<String, SessionData>>>,
    pub reg_states: Arc<Mutex<HashMap<String, (PasskeyRegistration, Instant)>>>,
    pub discoverable_states: Arc<Mutex<HashMap<String, (DiscoverableAuthentication, Instant)>>>,
    pub otc_store: Arc<RwLock<HashMap<String, (String, Instant)>>>,
    pub http_client: reqwest::Client,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("rp_id", &self.config.rp_id)
            .finish_non_exhaustive()
    }
}

impl AuthState {
    pub async fn new(config: AuthConfig) -> Result<Self, Error> {
        let origin = Url::parse(&config.rp_origin)
            .map_err(|e| Error::AuthSetup(format!("invalid rp_origin URL: {e}")))?;
        let webauthn = WebauthnBuilder::new(&config.rp_id, &origin)
            .map_err(|e| Error::AuthSetup(format!("WebauthnBuilder::new failed: {e:?}")))?
            .rp_name("Green")
            .build()
            .map_err(|e| Error::AuthSetup(format!("Webauthn::build failed: {e:?}")))?;

        let db = PgPool::connect(&config.db_url)
            .await
            .map_err(|e| Error::AuthSetup(format!("db connect: {e}")))?;

        sqlx::migrate!("./migrations")
            .run(&db)
            .await
            .map_err(|e| Error::AuthSetup(format!("db migrate: {e}")))?;

        Ok(AuthState {
            webauthn: Arc::new(webauthn),
            config,
            db,
            session_store: Arc::new(RwLock::new(HashMap::new())),
            reg_states: Arc::new(Mutex::new(HashMap::new())),
            discoverable_states: Arc::new(Mutex::new(HashMap::new())),
            otc_store: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
        })
    }

    fn role_for(&self, username: &str) -> Role {
        if self.config.admin_users.iter().any(|u| u == username) {
            Role::Admin
        } else {
            Role::Guest
        }
    }

    /// Returns `None` if no user with this username exists.
    async fn load_passkeys(&self, username: &str) -> Result<Option<(Uuid, Vec<Passkey>)>, Error> {
        let row = sqlx::query(
            "SELECT u.id, COALESCE(p.credentials, '[]'::jsonb) AS credentials \
             FROM users u \
             LEFT JOIN passkeys p ON p.user_id = u.id \
             WHERE u.username = $1",
        )
        .bind(username)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Error::Database(e.to_string()))?;

        match row {
            None => Ok(None),
            Some(row) => {
                let id: Uuid = row.get("id");
                let credentials: Value = row.get("credentials");
                let passkeys: Vec<Passkey> = serde_json::from_value(credentials)
                    .map_err(|e| Error::Database(format!("failed to deserialize passkeys: {e}")))?;
                Ok(Some((id, passkeys)))
            }
        }
    }

    async fn save_passkeys(
        &self,
        user_id: Uuid,
        username: &str,
        display_name: &str,
        role: &Role,
        passkeys: &[Passkey],
    ) -> Result<(), Error> {
        let role_str = match role {
            Role::Admin => "Admin",
            Role::Guest => "Guest",
        };
        let credentials = serde_json::to_value(passkeys)
            .map_err(|e| Error::Database(format!("failed to serialize passkeys: {e}")))?;

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let _ = sqlx::query(
            "INSERT INTO users (id, username, display_name, role) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (username) DO UPDATE SET display_name = EXCLUDED.display_name, role = EXCLUDED.role",
        )
        .bind(user_id)
        .bind(username)
        .bind(display_name)
        .bind(role_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::Database(e.to_string()))?;

        let _ = sqlx::query(
            "INSERT INTO passkeys (user_id, credentials, updated_at) \
             VALUES ($1, $2, NOW()) \
             ON CONFLICT (user_id) DO UPDATE SET credentials = EXCLUDED.credentials, updated_at = NOW()",
        )
        .bind(user_id)
        .bind(credentials)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::Database(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        Ok(())
    }

    /// Retrieve session data for a cookie value. Returns `None` if missing or expired.
    pub async fn get_session(&self, token: &str) -> Option<AuthUserInfo> {
        let store = self.session_store.read().await;
        store
            .get(token)
            .filter(|s| session_is_valid(s))
            .map(|s| AuthUserInfo {
                username: s.username.clone(),
                role: s.role.clone(),
            })
    }

    /// Purge registration challenge states older than [`CHALLENGE_TTL`].
    pub async fn cleanup_reg_states(&self) {
        let mut map = self.reg_states.lock().await;
        map.retain(|_, (_, ts)| ts.elapsed() <= CHALLENGE_TTL);
    }

    /// Returns `None` if no user with this UUID exists.
    async fn load_passkeys_by_id(
        &self,
        user_id: Uuid,
    ) -> Result<Option<(String, Vec<Passkey>)>, Error> {
        let row = sqlx::query(
            "SELECT u.username, COALESCE(p.credentials, '[]'::jsonb) AS credentials \
             FROM users u \
             LEFT JOIN passkeys p ON p.user_id = u.id \
             WHERE u.id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Error::Database(e.to_string()))?;

        match row {
            None => Ok(None),
            Some(row) => {
                let username: String = row.get("username");
                let credentials: Value = row.get("credentials");
                let passkeys: Vec<Passkey> = serde_json::from_value(credentials)
                    .map_err(|e| Error::Database(format!("failed to deserialize passkeys: {e}")))?;
                Ok(Some((username, passkeys)))
            }
        }
    }

    /// Purge discoverable challenge states older than [`CHALLENGE_TTL`].
    pub async fn cleanup_discoverable_states(&self) {
        let mut map = self.discoverable_states.lock().await;
        map.retain(|_, (_, ts)| ts.elapsed() <= CHALLENGE_TTL);
    }

    /// Purge one-time recovery codes older than [`OTC_TTL`].
    pub async fn cleanup_otc_store(&self) {
        let mut map = self.otc_store.write().await;
        map.retain(|_, (_, ts)| ts.elapsed() <= OTC_TTL);
    }

    /// Purge sessions older than [`SESSION_TTL`].
    pub async fn cleanup_sessions(&self) {
        let mut map = self.session_store.write().await;
        map.retain(|_, session| session_is_valid(session));
    }
}

fn session_is_valid(session: &SessionData) -> bool {
    session.created_at.elapsed() <= SESSION_TTL
}

const SESSION_COOKIE: &str = "green_session";

fn session_token_from_parts(parts: &Parts) -> Option<String> {
    let jar = CookieJar::from_headers(&parts.headers);
    jar.get(SESSION_COOKIE).map(|c| c.value().to_owned())
}

/// Resolves `ServerState` (or any state a module composes) down to just the
/// auth slice, so extractors and handlers that only need auth don't have to
/// depend on the rest of the application's state.
impl FromRef<ServerState> for Option<Arc<AuthState>> {
    fn from_ref(state: &ServerState) -> Self {
        state.auth_state.clone()
    }
}

/// Resolves to the configured auth state, or rejects with
/// [`Error::AuthNotConfigured`] — so handlers that require auth to be set up (login,
/// registration, recovery) don't each have to repeat the "is it
/// configured?" check themselves. Handlers that should behave gracefully
/// when auth is absent (e.g. `logout`) should extract
/// `Option<Arc<AuthState>>` directly instead.
pub struct Auth(pub Arc<AuthState>);

impl<S> FromRequestParts<S> for Auth
where
    S: Send + Sync,
    Option<Arc<AuthState>>: FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Option::<Arc<AuthState>>::from_ref(state)
            .map(Auth)
            .ok_or(Error::AuthNotConfigured)
    }
}

/// Resolves to an authenticated user, or redirects to `/auth/login`.
pub struct AuthUser {
    #[allow(dead_code)]
    pub user_id: Uuid,
    pub username: String,
    pub role: Role,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    Option<Arc<AuthState>>: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_opt = Option::<Arc<AuthState>>::from_ref(state);
        let auth = auth_opt
            .as_ref()
            .ok_or_else(|| Redirect::to("/").into_response())?;

        let next = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or_else(|| parts.uri.path());
        let encoded_next: String = url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
        let login_url = format!("/auth/login?next={encoded_next}");

        let token = session_token_from_parts(parts)
            .ok_or_else(|| Redirect::to(&login_url).into_response())?;

        let store = auth.session_store.read().await;
        let session = store
            .get(&token)
            .filter(|s| session_is_valid(s))
            .ok_or_else(|| Redirect::to(&login_url).into_response())?;

        Ok(AuthUser {
            user_id: session.user_id,
            username: session.username.clone(),
            role: session.role.clone(),
        })
    }
}

/// Resolves only if the authenticated user has the Admin role.
/// Unauthenticated requests are redirected to `/auth/login` (same as `AuthUser`).
/// Authenticated non-admin requests get a 403.
pub struct AdminUser(pub AuthUser);

impl<S> FromRequestParts<S> for AdminUser
where
    S: Send + Sync,
    Option<Arc<AuthState>>: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?; // propagates the /auth/login redirect if unauthenticated
        if user.role != Role::Admin {
            return Err(Error::Forbidden.into_response());
        }
        Ok(AdminUser(user))
    }
}

/// Always succeeds — returns `None` if no valid session exists.
pub struct MaybeAuthUser(pub Option<AuthUserInfo>);

impl<S> FromRequestParts<S> for MaybeAuthUser
where
    S: Send + Sync,
    Option<Arc<AuthState>>: FromRef<S>,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_opt = Option::<Arc<AuthState>>::from_ref(state);
        let Some(auth) = auth_opt.as_ref() else {
            return Ok(MaybeAuthUser(None));
        };
        let Some(token) = session_token_from_parts(parts) else {
            return Ok(MaybeAuthUser(None));
        };
        let info = auth.get_session(&token).await;
        Ok(MaybeAuthUser(info))
    }
}

pub fn make_session_cookie(token: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build()
}

pub fn clear_session_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time::Duration::ZERO)
        .build()
}

#[derive(Debug, Deserialize)]
pub struct StartRegRequest {
    pub username: String,
}

use askama::Template;
use axum::{
    Form, Json,
    extract::{Query, State},
    response::Html,
};
use serde_json::Value;

#[derive(Debug, Deserialize, Default)]
pub struct LoginQuery {
    pub next: Option<String>,
}

#[derive(Template)]
#[template(path = "auth_login.html")]
pub struct LoginPage {
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub next_url: String,
    pub nav_links: Arc<[NavLink]>,
}

#[derive(Template)]
#[template(path = "auth_register.html")]
pub struct RegisterPage {
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

pub async fn login_page(
    State(nav_links): State<Arc<[NavLink]>>,
    Query(q): Query<LoginQuery>,
) -> Result<Html<String>, Error> {
    Ok(Html(
        LoginPage {
            version: crate::VERSION,
            auth_user: None,
            next_url: q
                .next
                .filter(|n| n.starts_with('/') && !n.starts_with("//"))
                .unwrap_or_else(|| "/".to_owned()),
            nav_links,
        }
        .render()?,
    ))
}

pub async fn register_page(State(nav_links): State<Arc<[NavLink]>>) -> Result<Html<String>, Error> {
    Ok(Html(
        RegisterPage {
            version: crate::VERSION,
            auth_user: None,
            nav_links,
        }
        .render()?,
    ))
}

pub async fn start_registration(
    Auth(auth): Auth,
    Json(req): Json<StartRegRequest>,
) -> Result<Json<Value>, Error> {
    let user_id = auth
        .load_passkeys(&req.username)
        .await?
        .map(|(id, _)| id)
        .unwrap_or_else(Uuid::new_v4);

    auth.cleanup_reg_states().await;

    let (ccr, reg_state) = auth
        .webauthn
        .start_passkey_registration(user_id, &req.username, &req.username, None)
        .map_err(|e| Error::WebAuthn(format!("{e:?}")))?;

    {
        let mut states = auth.reg_states.lock().await;
        let _ = states.insert(req.username.clone(), (reg_state, Instant::now()));
    }

    Ok(Json(
        serde_json::to_value(ccr).map_err(|e| Error::WebAuthn(e.to_string()))?,
    ))
}

pub async fn finish_registration(
    Auth(auth): Auth,
    Json(body): Json<Value>,
) -> Result<(CookieJar, Redirect), Error> {
    let username = body
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::WebAuthn("missing username in finish_registration body".into()))?
        .to_owned();

    let reg_state = {
        let mut states = auth.reg_states.lock().await;
        states
            .remove(&username)
            .ok_or_else(|| Error::WebAuthn("no pending registration for that username".into()))?
            .0
    };

    let credential_json: Value = body
        .get("credential")
        .cloned()
        .ok_or_else(|| Error::WebAuthn("missing credential in body".into()))?;

    let reg_public_key = serde_json::from_value(credential_json)
        .map_err(|e| Error::WebAuthn(format!("invalid credential: {e}")))?;

    let passkey = auth
        .webauthn
        .finish_passkey_registration(&reg_public_key, &reg_state)
        .map_err(|e| Error::WebAuthn(format!("{e:?}")))?;

    let (user_id, mut passkeys) = auth
        .load_passkeys(&username)
        .await?
        .unwrap_or_else(|| (Uuid::new_v4(), vec![]));

    passkeys.push(passkey);
    let role = auth.role_for(&username);
    auth.save_passkeys(user_id, &username, &username, &role, &passkeys)
        .await?;

    tracing::info!(username, "user registered passkey");

    // Log the user in immediately after registration.
    auth.cleanup_sessions().await;
    let token = Uuid::new_v4().to_string();
    {
        let mut sessions = auth.session_store.write().await;
        let _ = sessions.insert(
            token.clone(),
            SessionData {
                user_id,
                username: username.clone(),
                role,
                created_at: Instant::now(),
            },
        );
    }

    let jar = CookieJar::new().add(make_session_cookie(token));
    Ok((jar, Redirect::to("/")))
}

#[derive(Debug, Serialize)]
pub struct DiscoverableChallengeResponse {
    #[serde(rename = "publicKey")]
    pub public_key: Value,
    pub challenge_id: String,
}

/// Start a discoverable (conditional-UI) authentication.
/// No username is required; the browser presents passkeys via its autofill UI.
pub async fn start_discoverable_auth(
    Auth(auth): Auth,
) -> Result<Json<DiscoverableChallengeResponse>, Error> {
    auth.cleanup_discoverable_states().await;

    let (rcr, state) = auth
        .webauthn
        .start_discoverable_authentication()
        .map_err(|e| Error::WebAuthn(format!("{e:?}")))?;

    let challenge_id = Uuid::new_v4().to_string();
    {
        let mut states = auth.discoverable_states.lock().await;
        let _ = states.insert(challenge_id.clone(), (state, Instant::now()));
    }

    let public_key = serde_json::to_value(&rcr)
        .map_err(|e| Error::WebAuthn(e.to_string()))?
        .get("publicKey")
        .cloned()
        .unwrap_or(Value::Null);

    Ok(Json(DiscoverableChallengeResponse {
        public_key,
        challenge_id,
    }))
}

#[derive(Debug, Deserialize)]
pub struct FinishDiscoverableRequest {
    pub challenge_id: String,
    pub credential: Value,
}

/// Finish a discoverable (conditional-UI) authentication.
/// Looks up the user by the `userHandle` embedded in the credential.
pub async fn finish_discoverable_auth(
    Auth(auth): Auth,
    jar: CookieJar,
    Json(req): Json<FinishDiscoverableRequest>,
) -> Result<(CookieJar, Redirect), Error> {
    let disc_state = {
        let mut states = auth.discoverable_states.lock().await;
        states
            .remove(&req.challenge_id)
            .ok_or_else(|| Error::WebAuthn("no pending discoverable challenge".into()))?
            .0
    };

    let auth_result_raw: webauthn_rs::prelude::PublicKeyCredential =
        serde_json::from_value(req.credential)
            .map_err(|e| Error::WebAuthn(format!("invalid credential: {e}")))?;

    let (user_id, _cred_id) = auth
        .webauthn
        .identify_discoverable_authentication(&auth_result_raw)
        .map_err(|e| {
            tracing::warn!("discoverable auth identify failed");
            Error::WebAuthn(format!("{e:?}"))
        })?;

    let (username, mut passkeys) = auth
        .load_passkeys_by_id(user_id)
        .await?
        .ok_or_else(|| Error::WebAuthn("user not found".into()))?;

    let discoverable_creds: Vec<DiscoverableKey> =
        passkeys.iter().map(DiscoverableKey::from).collect();

    let auth_result = auth
        .webauthn
        .finish_discoverable_authentication(&auth_result_raw, disc_state, &discoverable_creds)
        .map_err(|e| {
            tracing::warn!(username, "failed discoverable auth attempt");
            Error::WebAuthn(format!("{e:?}"))
        })?;

    for pk in &mut passkeys {
        let _ = pk.update_credential(&auth_result);
    }

    let role = auth.role_for(&username);
    auth.save_passkeys(user_id, &username, &username, &role, &passkeys)
        .await?;

    tracing::info!(username, ?role, "user logged in via discoverable auth");

    auth.cleanup_sessions().await;
    let token = Uuid::new_v4().to_string();
    {
        let mut sessions = auth.session_store.write().await;
        let _ = sessions.insert(
            token.clone(),
            SessionData {
                user_id,
                username: username.clone(),
                role,
                created_at: Instant::now(),
            },
        );
    }

    let jar = jar.add(make_session_cookie(token));
    Ok((jar, Redirect::to("/")))
}

pub async fn logout(
    State(auth_state): State<Option<Arc<AuthState>>>,
    jar: CookieJar,
) -> (CookieJar, Redirect) {
    if let Some(auth) = auth_state.as_ref()
        && let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned())
    {
        let username = {
            let store = auth.session_store.read().await;
            store.get(&token).map(|s| s.username.clone())
        };
        {
            let mut store = auth.session_store.write().await;
            let _ = store.remove(&token);
        }
        if let Some(username) = username {
            tracing::info!(username, "user logged out");
        }
    }
    let jar = jar.add(clear_session_cookie());
    (jar, Redirect::to("/auth/login"))
}

#[derive(Debug, Deserialize)]
pub struct StartRecoveryRequest {
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyRecoveryRequest {
    pub username: String,
    pub code: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct RecoveryQuery {
    pub sent: Option<bool>,
    pub username: Option<String>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "auth_recover.html")]
pub struct RecoveryPage {
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub sent: bool,
    pub username: String,
    pub error: Option<String>,
    pub nav_links: Arc<[NavLink]>,
}

pub async fn recover_page(
    State(nav_links): State<Arc<[NavLink]>>,
    Query(q): Query<RecoveryQuery>,
) -> Result<Html<String>, Error> {
    Ok(Html(
        RecoveryPage {
            version: crate::VERSION,
            auth_user: None,
            sent: q.sent.unwrap_or(false),
            username: q.username.unwrap_or_default(),
            error: q.error,
            nav_links,
        }
        .render()?,
    ))
}

/// Percent-encode a string for safe inclusion as a query parameter value (RFC 3986).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

fn generate_otc() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    // 252 = 7 × 36 — reject bytes ≥ 252 to avoid modulo bias
    let mut code = String::with_capacity(6);
    while code.len() < 6 {
        for &b in Uuid::new_v4().into_bytes().iter() {
            if code.len() == 6 {
                break;
            }
            if b < 252 {
                code.push(CHARSET[(b as usize) % 36] as char);
            }
        }
    }
    code
}

pub async fn start_recovery(
    Auth(auth): Auth,
    Form(req): Form<StartRecoveryRequest>,
) -> Result<Redirect, Error> {
    // Check user exists but don't reveal the result (anti-enumeration)
    let user_exists = auth
        .load_passkeys(&req.username)
        .await
        .is_ok_and(|r| r.is_some());

    if user_exists {
        let code = generate_otc();
        auth.cleanup_otc_store().await;
        let _ = auth
            .otc_store
            .write()
            .await
            .insert(req.username.clone(), (code.clone(), Instant::now()));

        if let Some(ref ntfy_url) = auth.config.ntfy_url
            && let Err(e) = auth
                .http_client
                .post(ntfy_url)
                .header("Title", "green recovery")
                .header("Priority", "high")
                .body(code)
                .send()
                .await
        {
            tracing::error!(error = %e, username = %req.username, "failed to send recovery notification");
        }
    }

    let url = format!(
        "/auth/recover?sent=true&username={}",
        percent_encode(&req.username)
    );
    Ok(Redirect::to(&url))
}

pub async fn verify_recovery(
    State(auth_state): State<Option<Arc<AuthState>>>,
    jar: CookieJar,
    Form(req): Form<VerifyRecoveryRequest>,
) -> Response {
    let error_url = format!(
        "/auth/recover?sent=true&username={}&error=invalid+or+expired+code",
        percent_encode(&req.username)
    );

    let Some(auth) = auth_state.as_ref() else {
        return Redirect::to("/").into_response();
    };

    // Atomically remove the OTC — prevents any race between check and delete.
    // The OTC is consumed whether the code matches or not (no brute-force retries).
    let removed = auth.otc_store.write().await.remove(&req.username);
    let Some((stored_code, created_at)) = removed else {
        return Redirect::to(&error_url).into_response();
    };

    if created_at.elapsed() > OTC_TTL || req.code != stored_code {
        return Redirect::to(&error_url).into_response();
    }

    let Ok(Some((user_id, _))) = auth.load_passkeys(&req.username).await else {
        return Redirect::to(&error_url).into_response();
    };

    let role = auth.role_for(&req.username);
    auth.cleanup_sessions().await;
    let token = Uuid::new_v4().to_string();
    {
        let mut sessions = auth.session_store.write().await;
        // Invalidate all existing sessions for this user before creating the recovery session.
        sessions.retain(|_, data| data.username != req.username);
        let _ = sessions.insert(
            token.clone(),
            SessionData {
                user_id,
                username: req.username.clone(),
                role,
                created_at: Instant::now(),
            },
        );
    }

    tracing::info!(username = %req.username, "user recovered account via OTC");

    let jar = jar.add(make_session_cookie(token));
    (jar, Redirect::to("/")).into_response()
}

#[cfg(test)]
impl AuthState {
    /// Create an `AuthState` for unit tests — uses a lazy (never-connecting) DB pool.
    pub fn new_for_testing(config: AuthConfig) -> Result<Self, Error> {
        let origin = Url::parse(&config.rp_origin)
            .map_err(|e| Error::AuthSetup(format!("invalid rp_origin URL: {e}")))?;
        let webauthn = WebauthnBuilder::new(&config.rp_id, &origin)
            .map_err(|e| Error::AuthSetup(format!("WebauthnBuilder::new failed: {e:?}")))?
            .rp_name("Green")
            .build()
            .map_err(|e| Error::AuthSetup(format!("Webauthn::build failed: {e:?}")))?;

        let db = PgPool::connect_lazy("postgres://localhost/nonexistent")
            .map_err(|e| Error::AuthSetup(format!("connect_lazy: {e}")))?;

        Ok(AuthState {
            webauthn: Arc::new(webauthn),
            config,
            db,
            session_store: Arc::new(RwLock::new(HashMap::new())),
            reg_states: Arc::new(Mutex::new(HashMap::new())),
            discoverable_states: Arc::new(Mutex::new(HashMap::new())),
            otc_store: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerState;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        response::Html,
        routing::get,
    };
    use std::path::Path;
    use tower::ServiceExt;

    async fn state_with_auth() -> ServerState {
        use crate::{
            breaker::BreakerContent,
            breaker_detail::{BreakerData, BreakerDetailStore, BreakerStore},
            index::Index,
            route::Routes,
        };

        let auth_config = AuthConfig {
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost".to_string(),
            db_url: "postgres://localhost/nonexistent".to_string(),
            admin_users: vec!["admin".to_string()],
            ntfy_url: None,
        };
        let auth_state = AuthState::new_for_testing(auth_config).unwrap();

        let data = BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![],
        };
        let store = Arc::new(BreakerStore::from_data(data).unwrap());
        let breaker_content = Arc::new(BreakerContent::new(store.as_ref()));
        let breaker_detail_store: Arc<dyn BreakerDetailStore> = store;
        let index = Index::new(
            Routes::default(),
            std::iter::empty::<crate::index::OptionalEntry>(),
            &Default::default(),
            None,
            Arc::new([]),
        )
        .await
        .unwrap();

        ServerState {
            ultron: crate::ultron::Ultron::new(reqwest::Client::new(), "test".into()).into(),
            certificate: Arc::from("fake-cert"),
            breaker_content,
            breaker_detail_store,
            index,
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store: None,
            recipes_store: None,
            auth_state: Some(Arc::new(auth_state)),
            mqtt_state: None,
            log_config: None,
            systemd_config: None,
            nav_links: Arc::new([]),
            peers: Arc::new([]),
            http_client: reqwest::Client::new(),
            peer_api_key: None,
            webhook_secret: None,
        }
    }
    async fn insert_session(state: &ServerState, username: &str, role: Role) -> String {
        let auth = state.auth_state.as_ref().unwrap();
        let token = Uuid::new_v4().to_string();
        let _ = auth.session_store.write().await.insert(
            token.clone(),
            SessionData {
                user_id: Uuid::new_v4(),
                username: username.to_string(),
                role,
                created_at: Instant::now(),
            },
        );
        token
    }

    async fn admin_only(_user: AdminUser) -> Html<&'static str> {
        Html("ok")
    }

    fn admin_router(state: ServerState) -> axum::Router {
        axum::Router::new()
            .route("/admin-only", get(admin_only))
            .with_state(state)
    }

    #[tokio::test]
    async fn admin_user_no_session_redirects_to_login() {
        let state = state_with_auth().await;
        let res = admin_router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin-only")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            res.headers().get("location").unwrap(),
            "/auth/login?next=%2Fadmin-only"
        );
    }

    #[tokio::test]
    async fn admin_user_no_session_preserves_query_string_in_next() {
        let state = state_with_auth().await;
        let res = admin_router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin-only?foo=bar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let location = res.headers().get("location").unwrap().to_str().unwrap();
        // The full path+query must be encoded so `&` can't corrupt the outer URL.
        assert_eq!(location, "/auth/login?next=%2Fadmin-only%3Ffoo%3Dbar");
    }

    #[tokio::test]
    async fn admin_user_no_session_encodes_ampersand_in_next() {
        let state = state_with_auth().await;
        let res = admin_router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin-only?a=1&b=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let location = res.headers().get("location").unwrap().to_str().unwrap();
        // Without encoding, `&b=2` would be parsed as a second query param, not part of `next`.
        assert_eq!(location, "/auth/login?next=%2Fadmin-only%3Fa%3D1%26b%3D2");
    }

    #[tokio::test]
    async fn admin_user_guest_session_returns_403() {
        let state = state_with_auth().await;
        let token = insert_session(&state, "alice", Role::Guest).await;
        let res = admin_router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin-only")
                    .header("cookie", format!("green_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_user_admin_session_succeeds() {
        let state = state_with_auth().await;
        let token = insert_session(&state, "admin", Role::Admin).await;
        let res = admin_router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin-only")
                    .header("cookie", format!("green_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    fn recovery_router(state: ServerState) -> axum::Router {
        axum::Router::new()
            .route("/auth/recover/verify", axum::routing::post(verify_recovery))
            .with_state(state)
    }

    fn verify_request(username: &str, code: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/auth/recover/verify")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("username={username}&code={code}")))
            .unwrap()
    }

    async fn insert_otc(state: &ServerState, username: &str, code: &str) {
        let _ = state
            .auth_state
            .as_ref()
            .unwrap()
            .otc_store
            .write()
            .await
            .insert(username.to_string(), (code.to_string(), Instant::now()));
    }

    #[test]
    fn generate_otc_has_valid_format() {
        const CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for _ in 0..200 {
            let code = generate_otc();
            assert_eq!(code.len(), 6, "code must be 6 chars");
            for ch in code.chars() {
                assert!(CHARSET.contains(ch), "'{ch}' not in CHARSET");
            }
        }
    }

    #[tokio::test]
    async fn verify_recovery_no_otc_redirects_with_error() {
        let state = state_with_auth().await;
        let res = recovery_router(state)
            .oneshot(verify_request("alice", "ABCDEF"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let loc = res.headers().get("location").unwrap().to_str().unwrap();
        assert!(
            loc.contains("error="),
            "expected error param in redirect: {loc}"
        );
    }

    #[tokio::test]
    async fn verify_recovery_wrong_code_redirects_with_error() {
        let state = state_with_auth().await;
        insert_otc(&state, "alice", "ABCDEF").await;
        let res = recovery_router(state)
            .oneshot(verify_request("alice", "XXXXXX"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let loc = res.headers().get("location").unwrap().to_str().unwrap();
        assert!(
            loc.contains("error="),
            "expected error param in redirect: {loc}"
        );
    }

    #[tokio::test]
    async fn verify_recovery_expired_otc_redirects_with_error() {
        let state = state_with_auth().await;
        {
            let auth = state.auth_state.as_ref().unwrap();
            let old = Instant::now() - Duration::from_secs(601);
            let _ = auth
                .otc_store
                .write()
                .await
                .insert("alice".to_string(), ("ABCDEF".to_string(), old));
        }
        let res = recovery_router(state)
            .oneshot(verify_request("alice", "ABCDEF"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let loc = res.headers().get("location").unwrap().to_str().unwrap();
        assert!(
            loc.contains("error="),
            "expected error param in redirect: {loc}"
        );
    }

    #[tokio::test]
    async fn verify_recovery_consumes_otc_even_on_wrong_code() {
        let state = state_with_auth().await;
        let auth = Arc::clone(state.auth_state.as_ref().unwrap());
        insert_otc(&state, "alice", "ABCDEF").await;
        let _ = recovery_router(state)
            .oneshot(verify_request("alice", "XXXXXX"))
            .await
            .unwrap();
        assert!(
            !auth.otc_store.read().await.contains_key("alice"),
            "OTC must be consumed even on a wrong-code attempt"
        );
    }
}
