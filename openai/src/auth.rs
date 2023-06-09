extern crate regex;

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use anyhow::bail;
use async_recursion::async_recursion;
use derive_builder::Builder;
use regex::Regex;
use reqwest::browser::ChromeVersion;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use base64::{engine::general_purpose, Engine as _};
use rand::Rng;
use reqwest::{Client, Proxy, StatusCode, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{debug, OAuthError, OAuthResult};

const CLIENT_ID: &str = "pdlLIX2Y72MIl2rhLhTE9VV9bN905kBh";
const OPENAI_OAUTH_URL: &str = "https://auth0.openai.com";
const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth0.openai.com/oauth/token";
const OPENAI_OAUTH_REVOKE_URL: &str = "https://auth0.openai.com/oauth/revoke";
const OPENAI_OAUTH_CALLBACK_URL: &str =
    "com.openai.chat://auth0.openai.com/ios/com.openai.chat/callback";
const OPENAI_OAUTH_PRE_AUTH_COOKIE: &str = "12345678-0707-0707-0707-123456789ABC%3A1689216803-UDB7Sr72DpdIO%2BCtsYEzms8uwGkzYetstlvTflB7%2BA0%3D";

const OPENAI_API_URL: &str = "https://api.openai.com";
/// You do **not** have to wrap the `Client` in an [`Rc`] or [`Arc`] to **reuse** it,
/// because it already uses an [`Arc`] internally.
///
/// [`Rc`]: std::rc::Rc
#[derive(Clone)]
pub struct AuthClient {
    client: Client,
    email_regex: Regex,
}

impl AuthClient {
    pub async fn do_dashboard_login(&self, access_token: &str) -> OAuthResult<DashSession> {
        let access_token = access_token.replace("Bearer ", "");
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/onboarding/login"))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        self.response_handle(resp).await
    }

    pub async fn do_get_api_key(&self, sensitive_id: &str, name: &str) -> OAuthResult<ApiKey> {
        let data = ApiKeyDataBuilder::default()
            .action("create")
            .name(name)
            .build()?;
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .json(&data)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        self.response_handle(resp).await
    }

    pub async fn do_get_api_key_list(&self, sensitive_id: &str) -> OAuthResult<ApiKeyList> {
        let resp = self
            .client
            .get(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        self.response_handle(resp).await
    }

    pub async fn do_delete_api_key(
        &self,
        sensitive_id: &str,
        redacted_key: &str,
        created_at: u64,
    ) -> OAuthResult<ApiKey> {
        let data = ApiKeyDataBuilder::default()
            .action("delete")
            .redacted_key(redacted_key)
            .created_at(created_at)
            .build()?;
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .json(&data)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        self.response_handle(resp).await
    }

    pub async fn do_access_token(&self, account: &OAuthAccount) -> OAuthResult<AccessToken> {
        if !self.email_regex.is_match(&account.username) || account.password.is_empty() {
            bail!(OAuthError::InvalidEmailOrPassword)
        }
        let code_verifier = Self::generate_code_verifier();
        let code_challenge = Self::generate_code_challenge(&code_verifier);

        let url = format!("https://auth0.openai.com/authorize?state=4DJBNv86mezKHDv-i2wMuDBea2-rHAo5nA_ZT4zJeak&ios_app_version=1744&client_id={CLIENT_ID}&redirect_uri={OPENAI_OAUTH_CALLBACK_URL}&code_challenge={code_challenge}&scope=openid%20email%20profile%20offline_access%20model.request%20model.read%20organization.read%20organization.write&prompt=login&preauth_cookie={OPENAI_OAUTH_PRE_AUTH_COOKIE}&audience=https://api.openai.com/v1&code_challenge_method=S256&response_type=code&auth0Client=eyJ2ZXJzaW9uIjoiMi4zLjIiLCJuYW1lIjoiQXV0aDAuc3dpZnQiLCJlbnYiOnsic3dpZnQiOiI1LngiLCJpT1MiOiIxNi4yIn19");

        let resp = self
            .client
            .get(url)
            .header(
                reqwest::header::REFERER,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        let url = resp.url().clone();

        self.response_handle_unit(resp)
            .await
            .map_err(|e| OAuthError::InvalidLoginUrl(e.to_string()))?;

        let state = Self::get_callback_state(&url);
        let url = format!("{OPENAI_OAUTH_URL}/u/login/identifier?state={state}");

        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .json(
                &IdentifierDataBuilder::default()
                    .action("default")
                    .state(&state)
                    .username(&account.username)
                    .js_available(true)
                    .webauthn_available(true)
                    .is_brave(false)
                    .webauthn_platform_available(false)
                    .build()?,
            )
            .send()
            .await?;

        let headers = resp.headers().clone();
        self.response_handle_unit(resp)
            .await
            .map_err(|_| OAuthError::InvalidEmail)?;

        let location = Self::get_location_path(&headers)?;
        self.authenticate_password(&code_verifier, &state, location, &url, account)
            .await
    }

    async fn authenticate_password(
        &self,
        code_verifier: &str,
        state: &str,
        location: &str,
        referrer: &str,
        account: &OAuthAccount,
    ) -> OAuthResult<AccessToken> {
        let data = AuthenticateDataBuilder::default()
            .action("default")
            .state(state)
            .username(&account.username)
            .password(&account.password)
            .build()?;

        let url = format!("{OPENAI_OAUTH_URL}{location}");

        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::REFERER, HeaderValue::from_str(referrer)?)
            .json(&data)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;

        let headers = resp.headers().clone();
        self.response_handle_unit(resp)
            .await
            .map_err(|_| OAuthError::InvalidEmailOrPassword)?;

        let location = Self::get_location_path(&headers)?;
        if location.starts_with("/authorize/resume?") {
            return self
                .authenticate_resume(code_verifier, location, &url, account)
                .await;
        }
        bail!(OAuthError::FailedLogin)
    }

    async fn authenticate_resume(
        &self,
        code_verifier: &str,
        location: &str,
        referrer: &str,
        account: &OAuthAccount,
    ) -> OAuthResult<AccessToken> {
        let resp = self
            .client
            .get(&format!("{OPENAI_OAUTH_URL}{location}"))
            .header(reqwest::header::REFERER, HeaderValue::from_str(referrer)?)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;

        let headers = resp.headers().clone();

        self.response_handle_unit(resp)
            .await
            .map_err(|_| OAuthError::InvalidLocation)?;

        let location: &str = Self::get_location_path(&headers)?;
        if location.starts_with("/u/mfa-otp-challenge?") {
            let mfa = account.mfa.clone().ok_or(OAuthError::MFARequired)?;
            self.authenticate_mfa(&mfa, code_verifier, location, account)
                .await
        } else if !location.starts_with(OPENAI_OAUTH_CALLBACK_URL) {
            bail!(OAuthError::FailedCallbackURL)
        } else {
            self.authorization_code(code_verifier, location).await
        }
    }

    #[async_recursion]
    async fn authenticate_mfa(
        &self,
        mfa_code: &str,
        code_verifier: &str,
        location: &str,
        account: &OAuthAccount,
    ) -> OAuthResult<AccessToken> {
        let url = format!("{OPENAI_OAUTH_URL}{}", location);
        let state = Self::get_callback_state(&Url::parse(&url)?);
        let data = AuthenticateMfaDataBuilder::default()
            .action("default")
            .state(&state)
            .code(mfa_code)
            .build()?;

        let resp = self
            .client
            .post(&url)
            .json(&data)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;
        let headers = resp.headers().clone();

        self.response_handle_unit(resp).await?;

        let location: &str = Self::get_location_path(&headers)?;
        if location.starts_with("/authorize/resume?") && account.mfa.is_none() {
            bail!(OAuthError::MFAFailed)
        }
        self.authenticate_resume(code_verifier, location, &url, &account)
            .await
    }

    async fn authorization_code(
        &self,
        code_verifier: &str,
        callback_url: &str,
    ) -> OAuthResult<AccessToken> {
        let url = Url::parse(callback_url)?;
        let code = Self::get_callback_code(&url)?;
        let data = AuthorizationCodeDataBuilder::default()
            .redirect_uri(OPENAI_OAUTH_CALLBACK_URL)
            .grant_type(GrantType::AuthorizationCode)
            .client_id(CLIENT_ID)
            .code(&code)
            .code_verifier(code_verifier)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_TOKEN_URL)
            .json(&data)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;

        let access_token = self.response_handle::<AccessToken>(resp).await?;
        Ok(access_token)
    }

    pub async fn do_refresh_token(&self, refresh_token: &str) -> OAuthResult<RefreshToken> {
        let refresh_token = Self::verify_refresh_token(refresh_token)?;
        let data = RefreshTokenDataBuilder::default()
            .redirect_uri(OPENAI_OAUTH_CALLBACK_URL)
            .grant_type(GrantType::RefreshToken)
            .client_id(CLIENT_ID)
            .refresh_token(refresh_token)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_TOKEN_URL)
            .json(&data)
            .send()
            .await?;

        let mut token = self.response_handle::<RefreshToken>(resp).await?;
        token.refresh_token = refresh_token.to_owned();
        Ok(token)
    }

    pub async fn do_revoke_token(&self, refresh_token: &str) -> OAuthResult<()> {
        let refresh_token = Self::verify_refresh_token(refresh_token)?;
        let data = RevokeTokenDataBuilder::default()
            .client_id(CLIENT_ID)
            .token(refresh_token)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_REVOKE_URL)
            .json(&data)
            .send()
            .await
            .map_err(OAuthError::FailedRequest)?;

        self.response_handle_unit(resp).await
    }

    async fn response_handle<U: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> OAuthResult<U> {
        let url = resp.url().clone();
        match resp.error_for_status_ref() {
            Ok(_) => Ok(resp
                .json::<U>()
                .await
                .map_err(|op| OAuthError::DeserializeError(op.to_string()))?),
            Err(err) => {
                let err_msg = format!("error: {}, url: {}", resp.text().await?, url);
                bail!(self.handle_error(err.status(), err_msg).await)
            }
        }
    }

    async fn response_handle_unit(&self, resp: reqwest::Response) -> OAuthResult<()> {
        let url = resp.url().clone();

        match resp.error_for_status_ref() {
            Ok(_) => Ok(()),
            Err(err) => {
                let err_msg = format!("error: {}, url: {}", resp.text().await?, url);
                bail!(self.handle_error(err.status(), err_msg).await)
            }
        }
    }

    async fn handle_error(&self, status: Option<StatusCode>, err_msg: String) -> OAuthError {
        match status {
            Some(
                status_code @ (StatusCode::UNAUTHORIZED
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::BAD_REQUEST
                | StatusCode::PAYMENT_REQUIRED
                | StatusCode::FORBIDDEN
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT),
            ) => {
                if status_code == StatusCode::UNAUTHORIZED {
                    return OAuthError::Unauthorized("Unauthorized".to_owned());
                }
                if status_code == StatusCode::TOO_MANY_REQUESTS {
                    return OAuthError::TooManyRequests("Too Many Requests".to_owned());
                }
                if status_code == StatusCode::BAD_REQUEST {
                    return OAuthError::BadRequest("Bad Request".to_owned());
                }

                if status_code.is_client_error() {
                    return OAuthError::InvalidClientRequest(err_msg);
                }

                OAuthError::ServerError(err_msg)
            }
            _ => OAuthError::InvalidRequest("Invalid Request".to_owned()),
        }
    }

    fn generate_code_verifier() -> String {
        let token: [u8; 32] = rand::thread_rng().gen();
        let code_verifier = general_purpose::URL_SAFE
            .encode(token)
            .trim_end_matches('=')
            .to_string();
        code_verifier
    }

    fn generate_code_challenge(code_verifier: &str) -> String {
        let mut m = Sha256::new();
        m.update(code_verifier.as_bytes());
        let code_challenge = general_purpose::URL_SAFE
            .encode(m.finalize())
            .trim_end_matches('=')
            .to_string();
        code_challenge
    }

    fn get_callback_code(url: &Url) -> OAuthResult<String> {
        let mut url_params = HashMap::new();
        url.query_pairs().into_owned().for_each(|(key, value)| {
            url_params
                .entry(key)
                .and_modify(|v: &mut Vec<String>| v.push(value.clone()))
                .or_insert(vec![value]);
        });

        debug!("get_callback_code: {:?}", url_params);

        if let Some(error) = url_params.get("error") {
            if let Some(error_description) = url_params.get("error_description") {
                let msg = format!("{}: {}", error[0], error_description[0]);
                bail!("{}", msg)
            } else {
                bail!("{}", error[0])
            }
        }

        let code = url_params
            .get("code")
            .ok_or(OAuthError::FailedCallbackCode)?[0]
            .to_string();
        Ok(code)
    }

    fn get_callback_state(url: &Url) -> String {
        let url_params = url.query_pairs().into_owned().collect::<HashMap<_, _>>();
        debug!("get_callback_state: {:?}", url_params);
        url_params["state"].to_owned()
    }

    fn get_location_path(header: &HeaderMap<HeaderValue>) -> OAuthResult<&str> {
        debug!("get_location_path: {:?}", header);
        Ok(header
            .get("Location")
            .ok_or(OAuthError::InvalidLocation)?
            .to_str()?)
    }

    fn verify_refresh_token(t: &str) -> OAuthResult<&str> {
        let refresh_token = t.trim_start_matches("Bearer ");
        if refresh_token.is_empty() {
            bail!(OAuthError::InvalidRefreshToken)
        }
        Ok(refresh_token)
    }
}

#[derive(Serialize, Builder)]
pub struct ApiKeyData<'a> {
    action: &'a str,
    #[builder(setter(into, strip_option), default)]
    name: Option<&'a str>,
    #[builder(setter(into, strip_option), default)]
    redacted_key: Option<&'a str>,
    #[builder(setter(into, strip_option), default)]
    created_at: Option<u64>,
}

#[derive(Serialize, Builder)]
struct IdentifierData<'a> {
    state: &'a str,
    username: &'a str,
    #[serde(rename = "js-available")]
    js_available: bool,
    #[serde(rename = "webauthn-available")]
    webauthn_available: bool,
    #[serde(rename = "is-brave")]
    is_brave: bool,
    #[serde(rename = "webauthn-platform-available")]
    webauthn_platform_available: bool,
    action: &'a str,
}

#[derive(Clone)]
enum GrantType {
    AuthorizationCode,
    RefreshToken,
}

impl Serialize for GrantType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            GrantType::AuthorizationCode => serializer.serialize_str("authorization_code"),
            GrantType::RefreshToken => serializer.serialize_str("refresh_token"),
        }
    }
}

#[derive(Serialize, Builder)]
struct AuthenticateData<'a> {
    state: &'a str,
    username: &'a str,
    password: &'a str,
    action: &'a str,
}

#[derive(Serialize, Builder)]
struct AuthenticateMfaData<'a> {
    state: &'a str,
    code: &'a str,
    action: &'a str,
}

#[derive(Serialize, Builder)]
struct AuthorizationCodeData<'a> {
    redirect_uri: &'a str,
    grant_type: GrantType,
    client_id: &'a str,
    code_verifier: &'a str,
    code: &'a str,
}

#[derive(Serialize, Builder)]
struct RevokeTokenData<'a> {
    client_id: &'a str,
    token: &'a str,
}

#[derive(Serialize, Builder)]
struct RefreshTokenData<'a> {
    redirect_uri: &'a str,
    grant_type: GrantType,
    client_id: &'a str,
    refresh_token: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
}

#[derive(Deserialize, Builder)]
pub struct OAuthAccount {
    username: String,
    password: String,
    #[builder(setter(into, strip_option), default)]
    mfa: Option<String>,
}

impl OAuthAccount {
    pub fn username(&self) -> &str {
        self.username.as_ref()
    }

    pub fn password(&self) -> &str {
        self.password.as_ref()
    }

    pub fn mfa(&self) -> Option<&str> {
        self.mfa.as_deref()
    }
}

pub struct AuthClientBuilder {
    builder: reqwest::ClientBuilder,
    oauth: AuthClient,
}

#[derive(Debug, Deserialize)]
pub struct DashSession {
    pub object: String,
    pub user: User,
    pub invites: Vec<Value>,
}

impl DashSession {
    pub fn sensitive_id(&self) -> &str {
        &self.user.session.sensitive_id
    }

    pub fn user_id(&self) -> &str {
        &self.user.id
    }

    pub fn nickname(&self) -> &str {
        &self.user.name
    }

    pub fn email(&self) -> &str {
        &self.user.email
    }

    pub fn picture(&self) -> &str {
        &self.user.picture
    }
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub object: String,
    pub id: String,
    pub email: String,
    pub name: String,
    pub picture: String,
    pub created: i64,
    pub groups: Vec<Value>,
    pub session: Session,
    pub orgs: Orgs,
    pub intercom_hash: String,
    pub amr: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct Session {
    pub sensitive_id: String,
    pub object: String,
    pub name: Option<String>,
    pub created: i64,
    pub last_use: Option<i64>,
    pub publishable: bool,
}

#[derive(Debug, Deserialize)]
pub struct Orgs {
    pub object: String,
    pub data: Vec<OrgsData>,
}

#[derive(Debug, Deserialize)]
pub struct OrgsData {
    pub object: String,
    pub id: String,
    pub created: i64,
    pub title: String,
    pub name: String,
    pub description: String,
    pub personal: bool,
    pub is_default: bool,
    pub role: String,
    pub groups: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub result: String,
    pub key: Option<Key>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyList {
    pub object: String,
    pub data: Vec<Key>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    #[serde(rename = "sensitive_id")]
    pub sensitive_id: String,
    pub object: String,
    pub name: String,
    pub created: i64,
    #[serde(rename = "last_use")]
    pub last_use: Value,
    pub publishable: bool,
}

impl AuthClientBuilder {
    // Proxy options
    pub fn proxy(mut self, proxy: Option<String>) -> Self {
        if let Some(url) = proxy {
            self.builder = self.builder.proxy(
                Proxy::all(url.clone()).expect(&format!("reqwest: invalid proxy url: {url}")),
            );
        } else {
            self.builder = self.builder.no_proxy();
        }
        self
    }

    // Timeout options

    /// Enables a request timeout.
    ///
    /// The timeout is applied from when the request starts connecting until the
    /// response body has finished.
    ///
    /// Default is no timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.timeout(timeout);
        self
    }

    /// Set a timeout for only the connect phase of a `Client`.
    ///
    /// Default is `None`.
    ///
    /// # Note
    ///
    /// This **requires** the futures be executed in a tokio runtime with
    /// a tokio timer enabled.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.connect_timeout(timeout);
        self
    }

    // HTTP options

    /// Set an optional timeout for idle sockets being kept-alive.
    ///
    /// Pass `None` to disable timeout.
    ///
    /// Default is 90 seconds.
    pub fn pool_idle_timeout<D>(mut self, val: D) -> Self
    where
        D: Into<Option<Duration>>,
    {
        self.builder = self.builder.pool_idle_timeout(val);
        self
    }

    /// Sets the maximum idle connection per host allowed in the pool.
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.builder = self.builder.pool_max_idle_per_host(max);
        self
    }

    /// Enable a persistent cookie store for the client.
    ///
    /// Cookies received in responses will be preserved and included in
    /// additional requests.
    ///
    /// By default, no cookie store is used.
    ///
    /// # Optional
    ///
    /// This requires the optional `cookies` feature to be enabled.
    pub fn cookie_store(mut self, store: bool) -> Self {
        self.builder = self.builder.cookie_store(store);
        self
    }

    /// Set that all sockets have `SO_KEEPALIVE` set with the supplied duration.
    ///
    /// If `None`, the option will not be set.
    pub fn tcp_keepalive<D>(mut self, val: D) -> Self
    where
        D: Into<Option<Duration>>,
    {
        self.builder = self.builder.tcp_keepalive(val);
        self
    }

    /// Sets the necessary values to mimic the specified Chrome version.
    pub fn chrome_builder(mut self, ver: ChromeVersion) -> Self {
        self.builder = self.builder.chrome_builder(ver);
        self
    }

    /// Sets the `User-Agent` header to be used by this client.
    pub fn user_agent(mut self, value: &str) -> Self {
        self.builder = self.builder.user_agent(value);
        self
    }

    pub fn build(mut self) -> AuthClient {
        self.oauth.client = self.builder.build().expect("ClientBuilder::build()");
        self.oauth
    }

    pub fn builder() -> AuthClientBuilder {
        let client_builder = Client::builder().redirect(Policy::custom(|attempt| {
            if attempt
                .url()
                .to_string()
                .contains("https://auth0.openai.com/u/login/identifier")
            {
                // redirects to 'https://auth0.openai.com/u/login/identifier'
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));

        AuthClientBuilder {
            builder: client_builder,
            oauth: AuthClient {
                client: Client::new(),
                email_regex: Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,7}\b")
                    .expect("Regex::new()"),
            },
        }
    }
}
