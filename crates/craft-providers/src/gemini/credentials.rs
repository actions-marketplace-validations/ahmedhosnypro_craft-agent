//! Application Default Credentials behind the Vertex bearer: the two
//! documented ADC file shapes, the RS256 service-account JWT exchange,
//! the user refresh-token grant, and an expiry-aware token cache.

mod token;

use std::path::{Path, PathBuf};

use craft_core::error::ProviderError;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use serde::Deserialize;
use sha2::Sha256;

pub use token::TokenSource;

/// The OAuth token endpoint the ADC flows exchange credentials at.
pub const TOKEN_URL: &str = token::TOKEN_URL;

/// The cloud-wide scope a service-account JWT requests; the local ADC
/// file created by the gcloud login flow carries the same scope.
pub const CLOUD_PLATFORM_SCOPE: &str = token::CLOUD_PLATFORM_SCOPE;

/// The env var naming the credential file; the documented ADC search
/// order checks it before the gcloud default path.
pub const CREDENTIALS_ENV: &str = "GOOGLE_APPLICATION_CREDENTIALS";

/// The documented default location of the gcloud-created ADC file.
pub const GCLOUD_DEFAULT_PATH: &str = ".config/gcloud/application_default_credentials.json";

/// One parsed ADC credential file, in either documented shape.
#[derive(Debug)]
pub enum Credential {
    ServiceAccount(Box<ServiceAccountCredential>),
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
}

/// The service-account side of the enum, boxed: the signing key dwarfs
/// the refresh credential's fields.
#[derive(Debug)]
pub struct ServiceAccountCredential {
    pub client_email: String,
    pub private_key_id: Option<String>,
    pub signing_key: SigningKey<Sha256>,
}

/// The documented service-account key file: JSON with a PEM private key.
#[derive(Deserialize)]
struct WireServiceAccount {
    client_email: String,
    #[serde(default)]
    private_key_id: Option<String>,
    private_key: String,
}

/// The documented user credential file: an OAuth client plus its
/// refresh token, created by the gcloud login flow.
#[derive(Deserialize)]
struct WireAuthorizedUser {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

fn invalid(detail: String) -> ProviderError {
    ProviderError::InvalidRequest { detail }
}

pub(super) fn tagged(context: &str, err: impl std::fmt::Display) -> ProviderError {
    invalid(format!("{context}: {err}"))
}

/// Parses ADC file contents: the documented `type` tag selects between
/// the service-account key and the user refresh credential; unknown
/// types are a named rejection.
pub fn parse_str(raw: &str, name: &str) -> Result<Credential, ProviderError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|err| tagged(name, err))?;
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            invalid(format!(
                "credential file {name} carries no documented type tag"
            ))
        })?;
    if kind == "service_account" {
        let wire: WireServiceAccount = serde_json::from_value(value)
            .map_err(|err| tagged(&format!("service_account file {name}"), err))?;
        let signing_key = signing_key(&wire.private_key, name)?;
        Ok(Credential::ServiceAccount(Box::new(
            ServiceAccountCredential {
                client_email: wire.client_email,
                private_key_id: wire.private_key_id,
                signing_key,
            },
        )))
    } else if kind == "authorized_user" {
        let wire: WireAuthorizedUser = serde_json::from_value(value)
            .map_err(|err| tagged(&format!("authorized_user file {name}"), err))?;
        Ok(Credential::AuthorizedUser {
            client_id: wire.client_id,
            client_secret: wire.client_secret,
            refresh_token: wire.refresh_token,
        })
    } else {
        Err(invalid(format!(
            "credential file {name} has unsupported type `{kind}`; expected \
             service_account or authorized_user"
        )))
    }
}

/// The RS256 signing key from the credential's PEM private key: PKCS#8
/// first, the PKCS#1 form as the fallback.
fn signing_key(pem: &str, name: &str) -> Result<SigningKey<Sha256>, ProviderError> {
    let context = format!("credential file {name} private_key");
    let key = RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
        .map_err(|err| tagged(&context, err))?;
    Ok(SigningKey::<Sha256>::new(key))
}

/// Reads and parses the ADC file: the env-named path when the variable
/// is set and non-empty, else the documented gcloud default path. A
/// missing variable and missing default file surface together, naming
/// both. The file read is async; call it once, off the request path.
pub async fn from_env() -> Result<Credential, ProviderError> {
    match std::env::var(CREDENTIALS_ENV) {
        Ok(path) if !path.trim().is_empty() => parse_file(Path::new(&path)).await,
        Ok(_) => Err(invalid(format!(
            "{CREDENTIALS_ENV} is set but empty; name a credential JSON file"
        ))),
        Err(std::env::VarError::NotPresent) => default_path().await,
        Err(err) => Err(invalid(format!("{CREDENTIALS_ENV} is unreadable: {err}"))),
    }
}

/// The gcloud default ADC path under the home directory.
async fn default_path() -> Result<Credential, ProviderError> {
    let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
        return Err(invalid(format!(
            "{CREDENTIALS_ENV} is unset and HOME is unset; name a credential JSON file"
        )));
    };
    let path = PathBuf::from(home).join(GCLOUD_DEFAULT_PATH);
    if !path.exists() {
        return Err(invalid(format!(
            "{CREDENTIALS_ENV} is unset and the default credential file does not exist: {}",
            path.display()
        )));
    }
    parse_file(&path).await
}

/// Reads one ADC credential file and parses either documented shape.
pub async fn parse_file(path: &Path) -> Result<Credential, ProviderError> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|err| tagged(&format!("credential file {}", path.display()), err))?;
    parse_str(&raw, &path.display().to_string())
}
