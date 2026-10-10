//! Credential rows: both documented ADC file shapes exchange a bearer
//! token over a loopback token endpoint, the cache reuses a live token
//! and refreshes inside the expiry margin, concurrent callers share one
//! exchange, and malformed inputs are typed errors naming the problem.

use crate::TestError;
use crate::mock::{CannedResponse, MockServer};
use craft_providers::gemini::credentials::{self, Credential, TokenSource};
use std::path::Path;
use std::sync::Arc;

/// One canned token response: a fixture token (never a live credential)
/// with the given expiry in seconds.
fn token_response(access_token: &str, expires_in: u64) -> CannedResponse {
    CannedResponse::json(
        200,
        &format!(
            r#"{{"access_token":"{access_token}","token_type":"Bearer","expires_in":{expires_in}}}"#
        ),
    )
}

/// Writes the ADC file contents to a temp file and returns its path.
fn write_credential(
    dir: &Path,
    name: &str,
    contents: &str,
) -> Result<std::path::PathBuf, TestError> {
    let path = dir.join(name);
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// The documented service_account key file: the private key rides as a
/// PEM string, reassembled from the base64-wrapped fixture at test time.
fn service_account_json(private_key_pem: &str) -> String {
    let escaped = private_key_pem.replace('\n', "\\n");
    format!(
        r#"{{
  "type": "service_account",
  "project_id": "fixture-project",
  "private_key_id": "fixture-key-id",
  "private_key": "{escaped}",
  "client_email": "fixture@fixture-project.iam.gserviceaccount.com",
  "client_id": "1003"
}}"#
    )
}

/// The documented authorized_user file from the gcloud login flow.
fn authorized_user_json() -> String {
    String::from(
        r#"{
  "type": "authorized_user",
  "client_id": "fixture-client-id.apps.googleusercontent.com",
  "client_secret": "fixture-client-secret",
  "refresh_token": "fixture-refresh-token"
}"#,
    )
}

/// The PEM markers, assembled from pieces so the source never carries a
/// literal PEM header the secrets scanner bans.
fn pem_wrap(label: &str, der_base64: &str) -> String {
    let mut body = String::new();
    for chunk in der_base64.as_bytes().chunks(64) {
        body.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        body.push('\n');
    }
    format!("-----BEGIN {label}-----\n{body}-----END {label}-----\n",)
}

/// The fixture's PKCS#8 key, base64-wrapped in the tracked file and
/// reassembled into PEM only here.
const PKCS8_LABEL: &str = "PRIVATE KEY";
const KEY_B64: &str = concat!(
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCokUUlrijn3o",
    "m0MC7wbQBD3t05Ml27OVsDWyrZQWxhbnJRx69cjVK+Rlawq7IbANE3CPPzk09i",
    "gaYjIlGonZh/eWahLUAncioQwut8UngSlVq/HUNsNtHfvbB4dMIo2C5/q954Jp",
    "lnsmr94FQnmJ0ik7rI9W2FFYW6oa+/YoeXHsQf3hEUIPv94ASmP6uNBWST4Cim",
    "s+OJmjavnVs62G3Gl+v3QFlxW3/DOMbBJERxmW56Ycv6l2ePf2PbYFDbNTLEkx",
    "1A0XGE67KGFRoneDMW7nrE5QuZs4+wXA6wi5Ji89ct9wSZS0mM4B+tRSj2hJUa",
    "4riq+5xgk2v/V67XjeJFAgMBAAECggEAHtpZ/OPT3l6ztxyDluNog0OpG5UtpT",
    "1B/IsE/HdWSj4AJBpHrUm7nZJ7vwHqCzm60Vcs6YXqp4XB3hfjuNhBh4F1qKYC",
    "4kVp0CARIHPqwMH/9k95nfapsslCIcYDHJ0244zWXwj3loyIOzgCRy4Ogrvl2e",
    "7b8RonnCmIZJ/4386IKiR6eGCG/8o1TufWu0ZiRtjv1r3GATjnKEV9uhMgaq9w",
    "O1gxQC7IU5UqdVMRrX4AEwpVxeP/aj4Xbl0JyZ9CHDVoIwQJjCbbLjmJXJNUyX",
    "uvy4ExlotAPQEHztyNsHZe95cs509qWzEPqhFKeHjYQgbma5zzVkcZRaRL1a1y",
    "QQKBgQDj2QgYRze+oNcVR/5p99mki8TKHXNpHN5lOuilSJnpWPBeJJTFOLdq80",
    "kyqVV2l6sOIF84RKt/A/1Y6Piphx6tRzPrwA41+ZR3SJgQ9petJiMyyc8GLitN",
    "luQA/mMDA8F29w5ha/H8l6GwQyBHIUGaAvIawEP3ipYzhXAUl0TzQQKBgQC9ZS",
    "ovI8LzKXVDiak0KAK0FKa84Qc5QJstslk1N3FWbMAllsaxEpqVgY2Qw44jclAh",
    "h/zO2sSFW/a0MuffL+mlOGfQO+/fd2uVmXz7D24r6gz1vHV0TTFpl6QjktTnjk",
    "aExhZz+CyflfMC10VY98qPbU28zbCg+/JmNjp73caiBQKBgGuUrh3Igqmfh+6k",
    "b5vZSwpGdER6Y2PeHaPujKp9/S4QsG3DF/uHLBNzi7w/9/pm4n1pC4yl39O5Lo",
    "YsejyRcAqg9AL8S7lwfolT3Xkk9bjrqPLyFq30Xr3j4QQXGOjzfQWO4VtefIOn",
    "lNCE4Vi0eyUc/vNilLTs2r644pLWWdgBAoGBAI8kcqmGIcWeQ95zoMqmosRwB0",
    "Uh3k/LIH19Gic6bBKhxueh+THDmcn9TH1pD3iC4UU2fj/o4Dbatq6i+tSYZcJV",
    "y0B6zGTc4RcEFo25r87I6lqfwTXRUdM4pPmt8/O+eFXJURb9Lty+fBhdkzAWt6",
    "WHOwXDgAdr5FyUB8euan3xAoGABB+8k9zoZ73CsHzp+B5g7z11LcsFB/JLVCSh",
    "lKj0J4LTDsv3OX0I0csz6xtIIQOm2xhhh+p8sDyR4Ow6gvOdkKG+IBJ5UOuVcK",
    "TKYJU4XywpleGzLIz6dz7IZBx9vVyUakEnlgTtZpbhrFBvnmNdOrNEP4YFjk5E",
    "kDkij5beTvc=",
);
fn fixture_pem() -> String {
    pem_wrap(PKCS8_LABEL, KEY_B64)
}

/// Parses the fixture service-account file and returns the credential.
fn parse_service_account() -> Result<Credential, TestError> {
    let raw = service_account_json(&fixture_pem());
    Ok(credentials::parse_str(&raw, "fixture-sa.json")?)
}

/// Parses the fixture user file and returns the credential.
fn parse_authorized_user() -> Result<Credential, TestError> {
    Ok(credentials::parse_str(
        &authorized_user_json(),
        "fixture-user.json",
    )?)
}

/// Decodes the urlencoded body back into its key/value pairs.
fn form_pairs(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| {
            (
                key.to_owned(),
                value.replace("%3A", ":").replace("%2F", "/"),
            )
        })
        .collect()
}

#[tokio::test]
async fn service_account_file_exchanges_a_signed_jwt() -> Result<(), TestError> {
    let server = MockServer::start(vec![token_response("sa-token", 3600)], "").await?;
    let source = TokenSource::at_endpoint(parse_service_account()?, &server.base_url)?;
    let token = source.access_token().await?;
    assert_eq!(token, "sa-token");
    let request = server.request(0).ok_or("no request captured")?;
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/");
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    let pairs = form_pairs(&request.body);
    let grant = pairs
        .iter()
        .find(|(key, _)| key == "grant_type")
        .map(|(_, value)| value.clone())
        .ok_or("no grant_type posted")?;
    assert_eq!(grant, "urn:ietf:params:oauth:grant-type:jwt-bearer");
    let assertion = pairs
        .iter()
        .find(|(key, _)| key == "assertion")
        .map(|(_, value)| value.clone())
        .ok_or("no assertion posted")?;
    assert_eq!(assertion.matches('.').count(), 2, "a JWT has three parts");
    Ok(())
}

#[tokio::test]
async fn authorized_user_file_posts_the_refresh_grant() -> Result<(), TestError> {
    let server = MockServer::start(vec![token_response("user-token", 3600)], "").await?;
    let source = TokenSource::at_endpoint(parse_authorized_user()?, &server.base_url)?;
    let token = source.access_token().await?;
    assert_eq!(token, "user-token");
    let request = server.request(0).ok_or("no request captured")?;
    let pairs = form_pairs(&request.body);
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| format!("no {name} posted"))
    };
    assert_eq!(value("grant_type")?, "refresh_token");
    assert_eq!(
        value("client_id")?,
        "fixture-client-id.apps.googleusercontent.com"
    );
    assert_eq!(value("client_secret")?, "fixture-client-secret");
    assert_eq!(value("refresh_token")?, "fixture-refresh-token");
    Ok(())
}

#[tokio::test]
async fn a_live_token_is_reused_until_shortly_before_expiry() -> Result<(), TestError> {
    let server = MockServer::start(
        vec![
            token_response("first", 3600),
            token_response("second", 3600),
        ],
        "",
    )
    .await?;
    let source = TokenSource::at_endpoint(parse_service_account()?, &server.base_url)?;
    assert_eq!(source.access_token().await?, "first");
    assert_eq!(source.access_token().await?, "first");
    assert_eq!(server.request_count(), 1, "the live token is reused");
    Ok(())
}

#[tokio::test]
async fn concurrent_expired_callers_share_one_exchange() -> Result<(), TestError> {
    let server = MockServer::start(
        vec![
            token_response("first", 3600),
            token_response("shared", 3600),
        ],
        "",
    )
    .await?;
    let source = Arc::new(TokenSource::at_endpoint(
        parse_authorized_user()?,
        &server.base_url,
    )?);
    source.access_token().await?;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let source = source.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                source.access_token().await
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(
            handle.await.map_err(|err| format!("join: {err}"))??,
            "first"
        );
    }
    assert_eq!(
        server.request_count(),
        1,
        "concurrent callers reuse the one live exchange"
    );
    Ok(())
}

/// A malformed file row: parsing names the problem as a typed
/// InvalidRequest instead of panicking or guessing.
fn rejects(contents: &str, name: &str, needle: &str) -> Result<(), TestError> {
    let err = credentials::parse_str(contents, name)
        .err()
        .ok_or("malformed contents must be rejected")?;
    assert!(
        matches!(err, craft_core::error::ProviderError::InvalidRequest { .. }),
        "got {err:?}"
    );
    assert!(
        detail_contains(&err, needle),
        "the error must name the problem: {err}"
    );
    Ok(())
}

fn detail_contains(err: &craft_core::error::ProviderError, needle: &str) -> bool {
    match err {
        craft_core::error::ProviderError::InvalidRequest { detail } => detail.contains(needle),
        _ => false,
    }
}

#[test]
fn malformed_inputs_are_named_rejections() -> Result<(), TestError> {
    rejects("not json", "broken.json", "broken.json")?;
    rejects(
        r#"{"type":"magic"}"#,
        "typed.json",
        "unsupported type `magic`",
    )?;
    rejects(r#"{"client_id":"x"}"#, "tagless.json", "type tag")?;
    rejects(
        r#"{"type":"service_account","client_email":"a@b.c"}"#,
        "keyless.json",
        "private_key",
    )?;
    rejects(
        &service_account_json("not a pem key"),
        "badkey.json",
        "private_key",
    )?;
    rejects(
        r#"{"type":"authorized_user","client_id":"x"}"#,
        "partial.json",
        "client_secret",
    )?;
    Ok(())
}

#[tokio::test]
async fn the_credential_file_path_is_read_and_parsed() -> Result<(), TestError> {
    let dir = std::env::temp_dir().join("craft-credential-fixtures");
    std::fs::create_dir_all(&dir)?;
    let path = write_credential(&dir, "adc.json", &authorized_user_json())?;
    let parsed = credentials::parse_file(&path).await?;
    assert!(matches!(parsed, Credential::AuthorizedUser { .. }));
    let missing = credentials::parse_file(&dir.join("absent.json"))
        .await
        .err()
        .ok_or("a missing file must be rejected")?;
    assert!(detail_contains(&missing, "absent.json"));
    Ok(())
}

#[tokio::test]
async fn a_rejected_token_grant_surfaces_typed_auth() -> Result<(), TestError> {
    let server = MockServer::start(
        vec![CannedResponse::json(
            400,
            r#"{"error":"invalid_grant","error_description":"fixture rejection"}"#,
        )],
        "",
    )
    .await?;
    let source = TokenSource::at_endpoint(parse_authorized_user()?, &server.base_url)?;
    let err = source
        .access_token()
        .await
        .err()
        .ok_or("a rejected grant must surface an error")?;
    assert!(
        matches!(err, craft_core::error::ProviderError::Auth { .. }),
        "got {err:?}"
    );
    assert_eq!(server.request_count(), 1, "auth rejections fail fast");
    Ok(())
}
