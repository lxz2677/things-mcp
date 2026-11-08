//! HTTP client for the Things Cloud sync protocol.
//!
//! Endpoints (see `docs/03-sync-engine.md`):
//!
//! * `GET  /version/1/account/{email}`
//! * `GET  /version/1/account/{email}/own-history-key`
//! * `GET  /version/1/history/{key}`
//! * `GET  /version/1/history/{key}/items?start-index=N`
//! * `POST /version/1/history/{key}/commit?ancestor-index=N&_cnt=1` (write path, later)
//!
//! Errors are signalled by the `things-response` response header, not the body.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::DEFAULT_BASE_URL;

pub const APP_ID: &str = "com.culturedcode.ThingsMac";
pub const SCHEMA_VERSION: i64 = 301;
pub const DEFAULT_USER_AGENT: &str = "ThingsMac/32209501";

const AUTH_RAW: u8 = 0;
const AUTH_PERCENT_ENCODED: u8 = 1;

#[derive(Clone, Debug)]
pub struct RawRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct RawReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RawReply {
    /// Convenience constructor for tests.
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.into().into_bytes(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn body_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        text.chars().take(400).collect()
    }
}

/// Transport abstraction: production uses reqwest, tests use a fake.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn execute(&self, request: RawRequest) -> anyhow::Result<RawReply>;
}

pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> anyhow::Result<Self> {
        // reqwest is built with `rustls-no-provider` so the binary has no
        // system TLS dependency (no OpenSSL, no Security.framework, no cmake).
        // Ring is the pure-Rust provider we install once per process.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn execute(&self, request: RawRequest) -> anyhow::Result<RawReply> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
        let mut builder = self.client.request(method, &request.url);
        for (key, value) in &request.headers {
            // Hop-by-hop headers are managed by reqwest.
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "host" | "connection" | "content-length" | "accept-encoding"
            ) {
                continue;
            }
            builder = builder.header(key, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response.bytes().await?.to_vec();

        Ok(RawReply {
            status,
            headers,
            body,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThingsError {
    #[error("unauthorized: check the Things Cloud password")]
    Unauthorized,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("rate limited by Things Cloud (AbusePrevention); retry later")]
    RateLimited,
    #[error("Things Cloud server error {status}: {body}")]
    Server { status: u16, body: String },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("empty items page at index {cursor} while server head is {head}")]
    Stalled { cursor: usize, head: i64 },
    #[error(transparent)]
    Transport(#[from] anyhow::Error),
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub base_url: String,
    pub email: String,
    pub password: String,
    pub app_instance_id: String,
    pub user_agent: String,
    pub max_retries: u32,
    pub retry_base_ms: u64,
}

impl ClientOptions {
    pub fn new(
        email: impl Into<String>,
        password: impl Into<String>,
        app_instance_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            email: email.into(),
            password: password.into(),
            app_instance_id: app_instance_id.into(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            max_retries: 3,
            retry_base_ms: 500,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfo {
    #[serde(
        rename = "SLA-version-accepted",
        default,
        deserialize_with = "string_or_default"
    )]
    pub sla_version: String,
    #[serde(default, deserialize_with = "string_or_default")]
    pub email: String,
    #[serde(
        rename = "history-key",
        default,
        deserialize_with = "string_or_default"
    )]
    pub history_key: String,
    #[serde(
        rename = "maildrop-email",
        default,
        deserialize_with = "string_or_default"
    )]
    pub maildrop_email: String,
    #[serde(default, deserialize_with = "string_or_default")]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OwnHistoryKey {
    #[serde(rename = "history-key", deserialize_with = "string_or_default")]
    pub history_key: String,
    #[serde(rename = "is-empty", default, deserialize_with = "bool_or_false")]
    pub is_empty: bool,
    #[serde(
        rename = "latest-schema-version",
        default,
        deserialize_with = "i64_or_zero"
    )]
    pub latest_schema_version: i64,
    #[serde(
        rename = "latest-server-index",
        default,
        deserialize_with = "i64_or_zero"
    )]
    pub latest_server_index: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HistoryMeta {
    #[serde(
        rename = "latest-server-index",
        default,
        deserialize_with = "i64_or_zero"
    )]
    pub latest_server_index: i64,
    #[serde(
        rename = "latest-schema-version",
        default,
        deserialize_with = "i64_or_zero"
    )]
    pub latest_schema_version: i64,
    #[serde(rename = "is-empty", default, deserialize_with = "bool_or_false")]
    pub is_empty: bool,
    #[serde(
        rename = "latest-total-content-size",
        default,
        deserialize_with = "i64_or_zero"
    )]
    pub latest_total_content_size: i64,
}

#[derive(Clone, Debug)]
pub struct IncomingItem {
    pub uuid: String,
    pub action: i64,
    /// Wire entity name, e.g. `Task7`.
    pub kind: String,
    pub props: Map<String, Value>,
    /// Server history index of the batch this item arrived in.
    pub server_index: usize,
}

#[derive(Clone, Debug)]
pub struct ItemsPage {
    pub items: Vec<IncomingItem>,
    /// `current-item-index`: the server head at the time of the response.
    pub head: i64,
    pub schema: i64,
    /// Number of top-level batches in `items`; this is how far the cursor moves.
    pub batches: usize,
}

#[derive(Deserialize)]
struct ItemsEnvelope {
    /// The server always sends an array, but treat a missing/null field as an
    /// empty page instead of failing the whole sync.
    #[serde(default)]
    items: Option<Vec<Map<String, Value>>>,
    #[serde(
        rename = "current-item-index",
        default,
        deserialize_with = "i64_or_zero"
    )]
    current_item_index: i64,
    #[serde(default)]
    schema: i64,
}

/// Things Cloud sometimes sends `null` for optional scalars (`maildrop-email`,
/// `history-key`, ...). Parse those as the type's default instead of failing.
fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn bool_or_false<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

fn i64_or_zero<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float as i64))
            .unwrap_or_default(),
        Some(Value::String(text)) => text.parse().unwrap_or_default(),
        _ => 0,
    })
}

#[derive(Serialize)]
struct ClientInfo<'a> {
    dm: &'a str,
    lr: &'a str,
    nf: bool,
    nk: bool,
    nn: &'a str,
    nv: &'a str,
    on: &'a str,
    ov: &'a str,
    pl: &'a str,
    ul: &'a str,
}

pub struct ThingsClient {
    transport: Arc<dyn Transport>,
    options: ClientOptions,
    auth_mode: AtomicU8,
    client_info: String,
}

impl ThingsClient {
    pub fn new(transport: Arc<dyn Transport>, options: ClientOptions) -> Self {
        let info = ClientInfo {
            dm: "MacBookPro18,3",
            lr: "US",
            nf: true,
            nk: true,
            nn: "ThingsMac",
            nv: "32209501",
            on: "macOS",
            ov: "15.7.3",
            pl: "en-US",
            ul: "en-Latn-US",
        };
        let client_info =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&info).unwrap());

        Self {
            transport,
            options,
            auth_mode: AtomicU8::new(AUTH_RAW),
            client_info,
        }
    }

    pub fn options(&self) -> &ClientOptions {
        &self.options
    }

    pub fn app_instance_id(&self) -> &str {
        &self.options.app_instance_id
    }

    fn authorization(&self) -> String {
        let password = &self.options.password;
        // HTTP header values must be visible ASCII. Passwords with non-ASCII or
        // control characters are percent-encoded up front instead of failing
        // inside the HTTP client.
        let must_encode = password
            .chars()
            .any(|c| !c.is_ascii() || c.is_ascii_control());
        let encoded = self.auth_mode.load(Ordering::Relaxed) == AUTH_PERCENT_ENCODED;

        let value = if encoded || must_encode {
            urlencoding::encode(password).into_owned()
        } else {
            password.clone()
        };
        format!("Password {value}")
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = format!("{}{}", self.options.base_url.trim_end_matches('/'), path);
        if !query.is_empty() {
            url.push('?');
            for (index, (key, value)) in query.iter().enumerate() {
                if index > 0 {
                    url.push('&');
                }
                url.push_str(key);
                url.push('=');
                url.push_str(&urlencoding::encode(value));
            }
        }
        url
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<RawReply, ThingsError> {
        let url = self.url(path, query);

        let mut headers = vec![
            ("Authorization".to_string(), self.authorization()),
            ("App-Id".to_string(), APP_ID.to_string()),
            (
                "App-Instance-Id".to_string(),
                self.options.app_instance_id.clone(),
            ),
            ("Schema".to_string(), SCHEMA_VERSION.to_string()),
            ("Accept".to_string(), "application/json".to_string()),
            ("Accept-Charset".to_string(), "UTF-8".to_string()),
            ("User-Agent".to_string(), self.options.user_agent.clone()),
            ("Things-Client-Info".to_string(), self.client_info.clone()),
        ];
        if method != "GET" {
            headers.push((
                "Content-Type".to_string(),
                "application/json; charset=UTF-8".to_string(),
            ));
            headers.push(("Content-Encoding".to_string(), "UTF-8".to_string()));
        }

        let mut attempt = 0u32;
        loop {
            let request = RawRequest {
                method: method.to_string(),
                url: url.clone(),
                headers: headers.clone(),
                body: body.clone(),
            };
            let reply = self.transport.execute(request).await?;

            match classify(&reply, path) {
                None => return Ok(reply),
                Some(error) => {
                    let retryable =
                        matches!(error, ThingsError::RateLimited | ThingsError::Server { .. });
                    if retryable && attempt < self.options.max_retries {
                        let delay = self.options.retry_base_ms
                            * 2u64.saturating_pow(attempt.min(5))
                            + (attempt as u64 * 137 % 250);
                        tracing::warn!(
                            attempt = attempt + 1,
                            delay_ms = delay,
                            url = %redact(&url),
                            "retrying Things Cloud request"
                        );
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Validate credentials.
    pub async fn verify(&self) -> Result<AccountInfo, ThingsError> {
        let path = format!("/version/1/account/{}", self.options.email);
        let reply = self.request("GET", &path, &[], None).await?;
        serde_json::from_slice(&reply.body)
            .map_err(|error| ThingsError::Protocol(format!("account response: {error}")))
    }

    /// Validate credentials, retrying with a percent-encoded password when the
    /// raw form is rejected. The wire spec says the password is URL-encoded,
    /// while working clients in the wild send it raw, so we probe both.
    pub async fn login(&self) -> Result<AccountInfo, ThingsError> {
        match self.verify().await {
            Ok(info) => Ok(info),
            Err(ThingsError::Unauthorized)
                if self.auth_mode.load(Ordering::Relaxed) == AUTH_RAW =>
            {
                tracing::debug!("raw password rejected; retrying percent-encoded");
                self.auth_mode
                    .store(AUTH_PERCENT_ENCODED, Ordering::Relaxed);
                self.verify().await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn own_history_key(&self) -> Result<OwnHistoryKey, ThingsError> {
        let path = format!("/version/1/account/{}/own-history-key", self.options.email);
        let reply = self.request("GET", &path, &[], None).await?;
        serde_json::from_slice(&reply.body)
            .map_err(|error| ThingsError::Protocol(format!("own-history-key response: {error}")))
    }

    pub async fn history_meta(&self, history_key: &str) -> Result<HistoryMeta, ThingsError> {
        let path = format!("/version/1/history/{history_key}");
        let reply = self.request("GET", &path, &[], None).await?;
        serde_json::from_slice(&reply.body)
            .map_err(|error| ThingsError::Protocol(format!("history response: {error}")))
    }

    /// Fetch one page of history items.
    ///
    /// `start-index` must never exceed the server head: the backend answers such
    /// requests with HTTP 500 instead of an empty page.
    pub async fn items_page(
        &self,
        history_key: &str,
        start_index: usize,
    ) -> Result<ItemsPage, ThingsError> {
        let path = format!("/version/1/history/{history_key}/items");
        let query = [("start-index", start_index.to_string())];
        let reply = self.request("GET", &path, &query, None).await?;
        let envelope: ItemsEnvelope = serde_json::from_slice(&reply.body)
            .map_err(|error| ThingsError::Protocol(format!("items response: {error}")))?;

        let batches = envelope.items.as_ref().map_or(0, Vec::len);
        let mut items = Vec::new();
        for (offset, batch) in envelope.items.iter().flatten().enumerate() {
            if batch.is_empty() {
                tracing::warn!(
                    index = start_index + offset,
                    "history batch contains no entities"
                );
                continue;
            }
            for (uuid, value) in batch {
                let Some(object) = value.as_object() else {
                    tracing::warn!(%uuid, "history item is not an object; skipping");
                    continue;
                };
                let action = object
                    .get("t")
                    .and_then(Value::as_i64)
                    .or_else(|| object.get("t").and_then(Value::as_f64).map(|f| f as i64))
                    .unwrap_or(-1);
                let kind = object
                    .get("e")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let props = object
                    .get("p")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();

                items.push(IncomingItem {
                    uuid: uuid.clone(),
                    action,
                    kind,
                    props,
                    server_index: start_index + offset,
                });
            }
        }

        Ok(ItemsPage {
            items,
            head: envelope.current_item_index,
            schema: envelope.schema,
            batches,
        })
    }
}

fn classify(reply: &RawReply, path: &str) -> Option<ThingsError> {
    let signal = reply.header("things-response").unwrap_or_default();

    match signal {
        "Unauthorized" => return Some(ThingsError::Unauthorized),
        "NotFound" => return Some(ThingsError::NotFound(path.to_string())),
        "AbusePrevention" => return Some(ThingsError::RateLimited),
        "GenericClientError" => {
            return Some(ThingsError::Protocol(format!(
                "GenericClientError: {}",
                reply.body_text()
            )));
        }
        _ => {}
    }

    match reply.status {
        200..=299 => None,
        401 => Some(ThingsError::Unauthorized),
        404 => Some(ThingsError::NotFound(path.to_string())),
        429 => Some(ThingsError::RateLimited),
        400 => Some(ThingsError::Protocol(format!(
            "bad request: {}",
            reply.body_text()
        ))),
        status @ 500..=599 => Some(ThingsError::Server {
            status,
            body: reply.body_text(),
        }),
        status => Some(ThingsError::Protocol(format!(
            "unexpected HTTP {status}: {}",
            reply.body_text()
        ))),
    }
}

/// Drop query strings and credentials before logging a URL.
fn redact(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn account_info_tolerates_null_fields() {
        let info: AccountInfo = serde_json::from_str(
            r#"{"SLA-version-accepted":null,"email":"me@example.com","history-key":null,
                "issues":[],"maildrop-email":null,"status":"SYAccountStatusActive"}"#,
        )
        .expect("null fields must not fail");

        assert_eq!(info.email, "me@example.com");
        assert_eq!(info.status, "SYAccountStatusActive");
        assert_eq!(info.history_key, "");
        assert_eq!(info.maildrop_email, "");
    }

    #[test]
    fn own_history_key_tolerates_nulls() {
        let key: OwnHistoryKey = serde_json::from_str(
            r#"{"history-key":"HK","is-empty":null,"latest-server-index":null,
                "latest-schema-version":null}"#,
        )
        .expect("null fields must not fail");

        assert_eq!(key.history_key, "HK");
        assert!(!key.is_empty);
        assert_eq!(key.latest_server_index, 0);
    }

    #[tokio::test]
    async fn login_accepts_null_account_fields() {
        let fake = crate::testing::FakeTransport::new(|_request| {
            RawReply::json(
                200,
                r#"{"SLA-version-accepted":null,"email":"me@example.com","history-key":"HK",
                    "issues":[],"maildrop-email":null,"status":"SYAccountStatusActive"}"#,
            )
        });

        let client = ThingsClient::new(fake, ClientOptions::new("me@example.com", "pw", "app"));
        let info = client.login().await.expect("login must succeed");

        assert_eq!(info.email, "me@example.com");
        assert_eq!(info.history_key, "HK");
    }

    #[tokio::test]
    async fn login_retries_with_percent_encoded_password() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();

        let fake = crate::testing::FakeTransport::new(move |request| {
            let auth = request
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            sink.lock().unwrap().push(auth.clone());

            if auth.contains('%') {
                RawReply::json(
                    200,
                    r#"{"email":"me@example.com","history-key":"HK","status":"SYAccountStatusActive"}"#,
                )
            } else {
                RawReply::json(401, "{}")
            }
        });

        let client = ThingsClient::new(
            fake,
            ClientOptions::new("me@example.com", "p@ss word", "app"),
        );
        let info = client.login().await.expect("fallback login must succeed");
        assert_eq!(info.email, "me@example.com");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                "Password p@ss word".to_string(),
                "Password p%40ss%20word".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn percent_encodes_non_ascii_passwords_up_front() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();

        let fake = crate::testing::FakeTransport::new(move |request| {
            let auth = request
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            sink.lock().unwrap().push(auth);
            RawReply::json(200, r#"{"email":"me@example.com","history-key":"HK"}"#)
        });

        let client = ThingsClient::new(
            fake,
            ClientOptions::new("me@example.com", "pässwörd", "app"),
        );
        client.verify().await.expect("login must succeed");

        let auths = seen.lock().unwrap().clone();
        assert_eq!(auths.len(), 1);
        assert_eq!(auths[0], "Password p%C3%A4ssw%C3%B6rd");
    }

    #[tokio::test]
    async fn retries_rate_limited_requests() {
        let attempts = std::sync::Arc::new(Mutex::new(0u32));
        let counter = attempts.clone();

        let fake = crate::testing::FakeTransport::new(move |_request| {
            let mut count = counter.lock().unwrap();
            *count += 1;
            if *count == 1 {
                RawReply::json(429, "{}").with_header("things-response", "AbusePrevention")
            } else {
                RawReply::json(
                    200,
                    r#"{"email":"me@example.com","history-key":"HK","status":"SYAccountStatusActive"}"#,
                )
            }
        });

        let mut options = ClientOptions::new("me@example.com", "pw", "app");
        options.retry_base_ms = 1;
        options.max_retries = 2;

        let client = ThingsClient::new(fake, options);
        let info = client.verify().await.expect("retry must succeed");
        assert_eq!(info.email, "me@example.com");
        assert_eq!(*attempts.lock().unwrap(), 2);
    }

    #[test]
    fn history_meta_tolerates_nulls() {
        let meta: HistoryMeta = serde_json::from_str(
            r#"{"latest-server-index":5,"latest-schema-version":null,"is-empty":null}"#,
        )
        .expect("null fields must not fail");

        assert_eq!(meta.latest_server_index, 5);
        assert_eq!(meta.latest_schema_version, 0);
    }
}
