//! Optional content-free wake relay for device-initiated mail synchronization.

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::DefaultBodyLimit,
    extract::{Path as AxumPath, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use http_body_util::{BodyExt as _, Full};
use hyper::{header, Request};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use ring::{rand::SystemRandom, signature};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

const SUBSCRIPTION_ID_LENGTH: usize = 43;
const MAX_DEVICE_TOKEN_LENGTH: usize = 512;
const PROVIDER_TOKEN_LIFETIME: Duration = Duration::from_mins(50);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("push registry is unavailable")]
    Registry(#[source] rusqlite::Error),
    #[error("APNs configuration is invalid")]
    ApnsConfiguration,
    #[error("APNs key is unavailable")]
    ApnsKey(#[source] std::io::Error),
}

#[derive(Clone)]
pub struct ServiceConfig {
    pub registration_token: String,
    pub trigger_token: String,
}

#[derive(Clone, Copy)]
pub enum ApnsEnvironment {
    Production,
    Sandbox,
}

pub struct ApnsConfig<'a> {
    pub key_path: &'a Path,
    pub key_id: &'a str,
    pub team_id: &'a str,
    pub topic: &'a str,
    pub environment: ApnsEnvironment,
}

pub struct SubscriptionStore {
    connection: Mutex<Connection>,
}

impl SubscriptionStore {
    /// Open the private subscription registry and apply its schema.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot open or migrate the registry.
    pub fn open(path: &Path) -> Result<Self, ServiceError> {
        let connection = Connection::open(path).map_err(ServiceError::Registry)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS push_subscriptions (
                     subscription_id TEXT PRIMARY KEY NOT NULL,
                     device_token TEXT NOT NULL
                 );",
            )
            .map_err(ServiceError::Registry)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn upsert(&self, subscription_id: &str, device_token: &str) -> Result<(), ApiError> {
        self.connection
            .lock()
            .map_err(|_| ApiError::Internal)?
            .execute(
                "INSERT INTO push_subscriptions (subscription_id, device_token)
                 VALUES (?1, ?2)
                 ON CONFLICT(subscription_id) DO UPDATE SET
                    device_token = excluded.device_token",
                params![subscription_id, device_token],
            )
            .map_err(|_| ApiError::Internal)?;
        Ok(())
    }

    fn remove(&self, subscription_id: &str) -> Result<(), ApiError> {
        self.connection
            .lock()
            .map_err(|_| ApiError::Internal)?
            .execute(
                "DELETE FROM push_subscriptions WHERE subscription_id = ?1",
                [subscription_id],
            )
            .map_err(|_| ApiError::Internal)?;
        Ok(())
    }

    fn device_token(&self, subscription_id: &str) -> Result<Option<String>, ApiError> {
        self.connection
            .lock()
            .map_err(|_| ApiError::Internal)?
            .query_row(
                "SELECT device_token FROM push_subscriptions WHERE subscription_id = ?1",
                [subscription_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| ApiError::Internal)
    }
}

#[async_trait]
pub trait PushSender: Send + Sync + 'static {
    async fn send_wake(&self, device_token: &str, subscription_id: &str) -> Result<(), ()>;
}

pub struct ApnsSender {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    signing_key: signature::EcdsaKeyPair,
    key_id: String,
    team_id: String,
    topic: String,
    endpoint: &'static str,
    token_cache: Mutex<Option<(SystemTime, String)>>,
}

impl ApnsSender {
    /// Build a reusable token-authenticated APNs client.
    ///
    /// # Errors
    /// Returns an error when the private key cannot be read or parsed.
    pub fn new(config: &ApnsConfig<'_>) -> Result<Self, ServiceError> {
        let key = read_pkcs8_key(config.key_path)?;
        let signing_key = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &key,
            &SystemRandom::new(),
        )
        .map_err(|_| ServiceError::ApnsConfiguration)?;
        let endpoint = match config.environment {
            ApnsEnvironment::Production => "api.push.apple.com",
            ApnsEnvironment::Sandbox => "api.development.push.apple.com",
        };
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_only()
            .enable_http2()
            .build();
        let client = Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(connector);
        Ok(Self {
            client,
            signing_key,
            key_id: config.key_id.to_owned(),
            team_id: config.team_id.to_owned(),
            topic: config.topic.to_owned(),
            endpoint,
            token_cache: Mutex::new(None),
        })
    }

    fn provider_token(&self) -> Result<String, ()> {
        #[derive(Serialize)]
        struct Header<'a> {
            alg: &'static str,
            kid: &'a str,
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            iat: u64,
        }

        let now = SystemTime::now();
        if let Some((created_at, token)) = self.token_cache.lock().map_err(|_| ())?.as_ref() {
            if now.duration_since(*created_at).unwrap_or_default() < PROVIDER_TOKEN_LIFETIME {
                return Ok(token.clone());
            }
        }

        let issued_at = now.duration_since(UNIX_EPOCH).map_err(|_| ())?.as_secs();
        let header = serde_json::to_vec(&Header {
            alg: "ES256",
            kid: &self.key_id,
        })
        .map_err(|_| ())?;
        let claims = serde_json::to_vec(&Claims {
            iss: &self.team_id,
            iat: issued_at,
        })
        .map_err(|_| ())?;
        let unsigned = format!(
            "{}.{}",
            general_purpose::URL_SAFE_NO_PAD.encode(header),
            general_purpose::URL_SAFE_NO_PAD.encode(claims)
        );
        let signature = self
            .signing_key
            .sign(&SystemRandom::new(), unsigned.as_bytes())
            .map_err(|_| ())?;
        let token = format!(
            "{unsigned}.{}",
            general_purpose::URL_SAFE_NO_PAD.encode(signature.as_ref())
        );
        *self.token_cache.lock().map_err(|_| ())? = Some((now, token.clone()));
        Ok(token)
    }
}

#[derive(Serialize)]
struct WakeRoute<'a> {
    subscription_id: &'a str,
}

#[derive(Serialize)]
struct Aps {
    #[serde(rename = "content-available")]
    content_available: u8,
}

#[derive(Serialize)]
struct WakeNotification<'a> {
    aps: Aps,
    tern: WakeRoute<'a>,
}

#[async_trait]
impl PushSender for ApnsSender {
    async fn send_wake(&self, device_token: &str, subscription_id: &str) -> Result<(), ()> {
        let body = wake_body(subscription_id)?;
        let request = Request::post(format!("https://{}/3/device/{device_token}", self.endpoint))
            .header(
                header::AUTHORIZATION,
                format!("bearer {}", self.provider_token()?),
            )
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "background")
            .header("apns-priority", "5")
            .header("apns-collapse-id", subscription_id)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| ())?;
        let response = self.client.request(request).await.map_err(|_| ())?;
        let status = response.status();
        response.into_body().collect().await.map_err(|_| ())?;
        status.is_success().then_some(()).ok_or(())
    }
}

fn wake_body(subscription_id: &str) -> Result<Vec<u8>, ()> {
    serde_json::to_vec(&WakeNotification {
        aps: Aps {
            content_available: 1,
        },
        tern: WakeRoute { subscription_id },
    })
    .map_err(|_| ())
}

fn read_pkcs8_key(path: &Path) -> Result<Vec<u8>, ServiceError> {
    let pem = fs::read_to_string(path).map_err(ServiceError::ApnsKey)?;
    let encoded = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ServiceError::ApnsConfiguration)
}

#[derive(Clone)]
struct AppState {
    store: Arc<SubscriptionStore>,
    sender: Arc<dyn PushSender>,
    config: ServiceConfig,
}

/// Build the authenticated relay HTTP API.
pub fn router(
    store: Arc<SubscriptionStore>,
    sender: Arc<dyn PushSender>,
    config: ServiceConfig,
) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route(
            "/v1/subscriptions/{subscription_id}",
            put(register).delete(unregister),
        )
        .route("/v1/subscriptions/{subscription_id}/trigger", post(trigger))
        .layer(DefaultBodyLimit::max(1_024))
        .with_state(AppState {
            store,
            sender,
            config,
        })
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    device_token: String,
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(subscription_id): AxumPath<String>,
    Json(registration): Json<Registration>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.config.registration_token)?;
    validate_subscription_id(&subscription_id)?;
    validate_device_token(&registration.device_token)?;
    state
        .store
        .upsert(&subscription_id, &registration.device_token)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unregister(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(subscription_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.config.registration_token)?;
    validate_subscription_id(&subscription_id)?;
    state.store.remove(&subscription_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trigger {}

async fn trigger(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(subscription_id): AxumPath<String>,
    Json(_trigger): Json<Trigger>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.config.trigger_token)?;
    validate_subscription_id(&subscription_id)?;
    let device_token = state
        .store
        .device_token(&subscription_id)?
        .ok_or(ApiError::NotFound)?;
    state
        .sender
        .send_wake(&device_token, &subscription_id)
        .await
        .map_err(|()| ApiError::Delivery)?;
    Ok(StatusCode::ACCEPTED)
}

fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let Some(provided) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Err(ApiError::Unauthorized);
    };
    let equal =
        provided.len() == expected.len() && provided.as_bytes().ct_eq(expected.as_bytes()).into();
    if equal {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn validate_subscription_id(value: &str) -> Result<(), ApiError> {
    let valid = value.len() == SUBSCRIPTION_ID_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(ApiError::InvalidRequest)
    }
}

fn validate_device_token(value: &str) -> Result<(), ApiError> {
    // Apple explicitly treats device tokens as variable-length values. The
    // client serializes the opaque bytes as hex solely for transport.
    let valid = !value.is_empty()
        && value.len() <= MAX_DEVICE_TOKEN_LENGTH
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(ApiError::InvalidRequest)
    }
}

enum ApiError {
    Unauthorized,
    InvalidRequest,
    NotFound,
    Delivery,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid request"),
            Self::NotFound => (StatusCode::NOT_FOUND, "subscription not found"),
            Self::Delivery => (StatusCode::BAD_GATEWAY, "push delivery failed"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "service unavailable"),
        };
        (status, message).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::sync::Mutex;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt as _;
    use tempfile::NamedTempFile;
    use tower::ServiceExt as _;

    use super::*;

    const SUBSCRIPTION_ID: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGH012345678";
    const DEVICE_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct RecordingSender {
        deliveries: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl PushSender for RecordingSender {
        async fn send_wake(&self, device_token: &str, subscription_id: &str) -> Result<(), ()> {
            self.deliveries
                .lock()
                .unwrap()
                .push((device_token.to_owned(), subscription_id.to_owned()));
            Ok(())
        }
    }

    fn test_app(sender: Arc<RecordingSender>) -> Router {
        let file = NamedTempFile::new().unwrap();
        let store = Arc::new(SubscriptionStore::open(file.path()).unwrap());
        router(
            store,
            sender,
            ServiceConfig {
                registration_token: "register-secret".into(),
                trigger_token: "trigger-secret".into(),
            },
        )
    }

    async fn request(app: &Router, request: Request<Body>) -> (StatusCode, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn authenticated_request(method: &str, path: &str, token: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn registered_subscription_delivers_content_free_wake() {
        let sender = Arc::new(RecordingSender::default());
        let app = test_app(Arc::clone(&sender));
        let path = format!("/v1/subscriptions/{SUBSCRIPTION_ID}");
        let register = authenticated_request(
            "PUT",
            &path,
            "register-secret",
            &format!(r#"{{"device_token":"{DEVICE_TOKEN}"}}"#),
        );
        assert_eq!(request(&app, register).await.0, StatusCode::NO_CONTENT);

        let trigger =
            authenticated_request("POST", &format!("{path}/trigger"), "trigger-secret", "{}");
        assert_eq!(request(&app, trigger).await.0, StatusCode::ACCEPTED);
        assert_eq!(
            sender.deliveries.lock().unwrap().as_slice(),
            [(DEVICE_TOKEN.to_owned(), SUBSCRIPTION_ID.to_owned())]
        );
    }

    #[tokio::test]
    async fn trigger_rejects_message_data() {
        let sender = Arc::new(RecordingSender::default());
        let app = test_app(Arc::clone(&sender));
        let trigger = authenticated_request(
            "POST",
            &format!("/v1/subscriptions/{SUBSCRIPTION_ID}/trigger"),
            "trigger-secret",
            r#"{"subject":"must not enter push service"}"#,
        );

        assert_eq!(
            request(&app, trigger).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(sender.deliveries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn credentials_are_not_interchangeable() {
        let sender = Arc::new(RecordingSender::default());
        let app = test_app(sender);
        let register = authenticated_request(
            "PUT",
            &format!("/v1/subscriptions/{SUBSCRIPTION_ID}"),
            "trigger-secret",
            &format!(r#"{{"device_token":"{DEVICE_TOKEN}"}}"#),
        );

        let (status, body) = request(&app, register).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, "unauthorized");
    }

    #[test]
    fn apns_payload_contains_only_wake_routing_data() {
        let body = wake_body(SUBSCRIPTION_ID).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "aps": {"content-available": 1},
                "tern": {"subscription_id": SUBSCRIPTION_ID}
            })
        );
    }
}
