//! Vertex mount rows: the documented endpoint host per location, the
//! project-scoped stream URL, the ADC bearer instead of the API-key
//! header, and the shared SSE lane over the same chunk stream.

use crate::TestError;
use crate::mock::{CannedResponse, MockServer};
use crate::wire;
use craft_core::provider::Provider;
use craft_core::types::ModelRequest;
use craft_providers::gemini::credentials::{self, TokenSource};
use craft_providers::gemini::vertex::{self, VertexProvider};
use futures::StreamExt;

use crate::Outcome;

const PROJECT: &str = "fixture-project";
const TOKEN: &str = "vertex-fixture-token";

/// The documented authorized_user ADC file; every value is a fixture.
const USER_FILE: &str = r#"{
  "type": "authorized_user",
  "client_id": "fixture-client-id.apps.googleusercontent.com",
  "client_secret": "fixture-client-secret",
  "refresh_token": "fixture-refresh-token"
}"#;

/// The ADC bearer source over a loopback token endpoint serving one
/// fixture token.
async fn token_source() -> Result<(TokenSource, MockServer), TestError> {
    let server = MockServer::start(
        vec![CannedResponse::json(
            200,
            &format!(r#"{{"access_token":"{TOKEN}","token_type":"Bearer","expires_in":3600}}"#),
        )],
        "",
    )
    .await?;
    let credential = credentials::parse_str(USER_FILE, "fixture-user.json")?;
    let source = TokenSource::at_endpoint(credential, &server.base_url)?;
    Ok((source, server))
}

/// The fixture mount: the bearer source and the model endpoint each on
/// their own loopback, so the URL and the header are pinned without
/// network egress.
async fn mounted(
    project: &str,
    location: &str,
) -> Result<(VertexProvider, MockServer, MockServer), TestError> {
    let (tokens, token_server) = token_source().await?;
    let server = MockServer::start(
        vec![CannedResponse::sse(wire::text_stream().as_bytes())],
        "",
    )
    .await?;
    let provider =
        VertexProvider::at_endpoint(project, location, crate::MODEL, tokens, &server.base_url)?;
    Ok((provider, server, token_server))
}

/// Drives one Vertex stream to completion; a failed connect surfaces as
/// the single error outcome.
async fn run(provider: &VertexProvider, request: ModelRequest) -> Vec<Outcome> {
    let mut stream = match provider.stream(request).await {
        Ok(stream) => stream,
        Err(err) => return vec![Err(err)],
    };
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item);
    }
    events
}

#[test]
fn the_endpoint_host_follows_the_location() -> Result<(), TestError> {
    assert_eq!(
        vertex::endpoint_url("global"),
        "https://aiplatform.googleapis.com",
        "the global endpoint rides the unprefixed host"
    );
    for location in ["us-central1", "europe-west4"] {
        assert_eq!(
            vertex::endpoint_url(location),
            format!("https://{location}-aiplatform.googleapis.com"),
            "regional locations ride their own prefixed host"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_stream_url_rides_the_project_scoped_shape() -> Result<(), TestError> {
    let (provider, server, _token_server) = mounted(PROJECT, "us-central1").await?;
    let events = run(&provider, crate::sample_request()).await;
    assert!(
        crate::errors(&events).is_empty(),
        "the served stream must decode"
    );
    let request = server.request(0).ok_or("the adapter must send a request")?;
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        format!(
            "/v1/projects/{PROJECT}/locations/us-central1/publishers/google/models/{}:streamGenerateContent?alt=sse",
            crate::MODEL
        )
    );
    Ok(())
}

#[tokio::test]
async fn the_bearer_rides_the_authorization_header_not_the_key_header() -> Result<(), TestError> {
    let (provider, server, token_server) = mounted(PROJECT, "global").await?;
    let events = run(&provider, crate::sample_request()).await;
    assert!(
        crate::errors(&events).is_empty(),
        "the served stream must decode"
    );
    let request = server.request(0).ok_or("the adapter must send a request")?;
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer vertex-fixture-token"),
        "the ADC token mounts as the documented bearer"
    );
    assert!(
        !request.headers.contains_key("x-goog-api-key"),
        "the vertex mount never sends the Gemini API-key header"
    );
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let grant = token_server
        .request(0)
        .ok_or("the token source must exchange a grant")?;
    assert!(
        grant.body.contains("grant_type=refresh_token"),
        "the bearer comes from the ADC refresh grant"
    );
    Ok(())
}

#[tokio::test]
async fn the_shared_sse_lane_decodes_the_vertex_stream() -> Result<(), TestError> {
    let (provider, _server, _token_server) = mounted(PROJECT, "us-central1").await?;
    let events = run(&provider, crate::sample_request()).await;
    let turn = crate::folded(&events)?;
    assert_eq!(turn.text, "Hello world");
    assert_eq!(turn.stop_reason, craft_core::types::StopReason::EndTurn);
    assert_eq!(crate::terminal_count(&events), 1);
    Ok(())
}

#[test]
fn an_empty_mount_part_is_a_named_rejection() -> Result<(), TestError> {
    for (part, project, location, model) in [
        ("project", "", "us-central1", "m"),
        ("location", "p", "", "m"),
        ("model", "p", "us-central1", ""),
    ] {
        let Err(err) = VertexProvider::new(project, location, model) else {
            return Err(format!("an empty {part} must be rejected").into());
        };
        assert!(
            err.to_string().contains(part),
            "the failure names the empty part: {err}"
        );
    }
    Ok(())
}
