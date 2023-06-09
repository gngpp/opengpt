#![recursion_limit = "256"]
pub mod arkose;
pub mod auth;
#[cfg(feature = "stream")]
pub mod eventsource;
pub mod log;
pub mod model;
pub mod opengpt;
pub mod platform;
pub mod unescape;

#[cfg(feature = "serve")]
pub mod serve;
pub mod token;

pub const HEADER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36";
pub const URL_CHATGPT_API: &str = "https://chat.openai.com";
pub const URL_PLATFORM_API: &str = "https://api.openai.com";

pub const ORIGIN_CHATGPT: &str = "https://chat.openai.com/chat";
pub const HOST_CHATGPT: &str = "chat.openai.com";

pub type OAuthResult<T, E = anyhow::Error> = anyhow::Result<T, E>;

#[derive(thiserror::Error, Debug)]
pub enum OAuthError {
    #[error("other request (error {0:?}")]
    Other(String),
    #[error("token access (error {0:?}")]
    TokenAccess(anyhow::Error),
    #[error("bad request (error {0:?}")]
    BadRequest(String),
    #[error("too many requests `{0}`")]
    TooManyRequests(String),
    #[error("Unauthorized request (error {0:?}")]
    Unauthorized(String),
    #[error("Server error {0:?}")]
    ServerError(String),
    #[error("failed to get public key")]
    FailedPubKeyRequest,
    #[error("failed login")]
    FailedLogin,
    #[error(transparent)]
    FailedRequest(#[from] reqwest::Error),
    #[error("invalid client request (error {0:?})")]
    InvalidClientRequest(String),
    #[error("failed get code from callback url")]
    FailedCallbackCode,
    #[error("failed callback url")]
    FailedCallbackURL,
    #[error("invalid request login url (error {0:?}")]
    InvalidLoginUrl(String),
    #[error("invalid email or password")]
    InvalidEmailOrPassword,
    #[error("invalid request {0:?}")]
    InvalidRequest(String),
    #[error("invalid email")]
    InvalidEmail,
    #[error("invalid Location")]
    InvalidLocation,
    #[error("invalid access-token")]
    InvalidAccessToken,
    #[error("invalid refresh-token")]
    InvalidRefreshToken,
    #[error("token expired")]
    TokenExpired,
    #[error("MFA failed")]
    MFAFailed,
    #[error("MFA required")]
    MFARequired,
    #[error("json deserialize error `{0}`")]
    DeserializeError(String),
}

#[derive(thiserror::Error, Debug)]
pub enum TokenStoreError {
    #[error("failed to access token")]
    AccessError,
    #[error("token not found error")]
    NotFoundError,
    #[error("failed token deserialize")]
    DeserializeError(#[from] serde_json::error::Error),
    #[error("failed to verify access_token")]
    AccessTokenVerifyError,
    #[error("failed to create default token store file")]
    CreateDefaultTokenFileError,
}
