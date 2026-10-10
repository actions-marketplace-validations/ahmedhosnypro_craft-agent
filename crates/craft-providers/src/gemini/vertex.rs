//! Vertex mount of the Gemini dialect: the project-scoped stream URL and
//! the ADC bearer instead of the API key, over the shared request
//! encoder, SSE lane, and error taxonomy.

use std::time::Duration;

use craft_core::error::ProviderError;
use craft_core::provider::{ModelEventStream, Provider};
use craft_core::types::ModelRequest;

use super::credentials::{self, TokenSource};
use super::http_failure;
use super::lane;
use super::request;
use crate::retry;
use crate::transport;

/// The unprefixed host of the documented global service endpoint; every
/// regional location pins its own prefixed host.
const GLOBAL_HOST: &str = "aiplatform.googleapis.com";
const HOST_SUFFIX: &str = "-aiplatform.googleapis.com";

/// The generateContent dialect mounted over one Vertex project,
/// location, and model, authenticating with the ADC bearer instead of
/// the Gemini API key. The token source rides boxed: it carries the
/// credential's signing key.
pub struct VertexProvider {
    client: reqwest::Client,
    base_url: String,
    project: String,
    location: String,
    model: String,
    sse: craft_core::sse::SseOptions,
    tokens: tokio::sync::OnceCell<Box<TokenSource>>,
}

impl VertexProvider {
    /// The production mount: the location's documented service endpoint
    /// and the ADC credential, resolved once on the first request.
    pub fn new(project: &str, location: &str, model: &str) -> Result<Self, ProviderError> {
        Self::mounted(
            project,
            location,
            model,
            &endpoint_url(location),
            tokio::sync::OnceCell::new(),
        )
    }

    /// The fixture mount: an explicit token source and endpoint, so
    /// loopback corpora pin the URL and the header without egress.
    pub fn at_endpoint(
        project: &str,
        location: &str,
        model: &str,
        tokens: TokenSource,
        base_url: &str,
    ) -> Result<Self, ProviderError> {
        Self::mounted(
            project,
            location,
            model,
            base_url,
            tokio::sync::OnceCell::from(Box::new(tokens)),
        )
    }

    fn mounted(
        project: &str,
        location: &str,
        model: &str,
        base_url: &str,
        tokens: tokio::sync::OnceCell<Box<TokenSource>>,
    ) -> Result<Self, ProviderError> {
        reject_empty("project", project)?;
        reject_empty("location", location)?;
        reject_empty("model", model)?;
        Ok(Self {
            client: transport::client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            project: project.to_owned(),
            location: location.to_owned(),
            model: model.to_owned(),
            sse: craft_core::sse::SseOptions::default(),
            tokens,
        })
    }

    /// Remounts the lane over an explicit total request deadline; the
    /// constructor's deadline comes from the shared env knob.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self, ProviderError> {
        self.client = transport::client_with_timeout(timeout)?;
        Ok(self)
    }

    /// The documented project-scoped stream URL: the publisher-model
    /// path over the location's endpoint, riding the SSE alt of the
    /// standard Google API `alt` system parameter.
    fn stream_url(&self) -> String {
        format!(
            "{}/v1/projects/{}/locations/{}/publishers/google/models/{}:streamGenerateContent?alt=sse",
            self.base_url, self.project, self.location, self.model
        )
    }

    /// The ADC-backed bearer source, resolved once and shared by every
    /// request; a failed resolution stays unset and names itself again
    /// on the next request.
    async fn token_source(&self) -> Result<&TokenSource, ProviderError> {
        Ok(self
            .tokens
            .get_or_try_init(|| async {
                let credential = credentials::from_env().await?;
                Ok::<Box<TokenSource>, ProviderError>(Box::new(TokenSource::new(credential)?))
            })
            .await?)
    }

    /// Sends one streaming request on the shared ladder, mounted over
    /// this dialect's URL, bearer header, and payload.
    async fn connect(&self, request: &ModelRequest) -> Result<reqwest::Response, ProviderError> {
        let body = request::encode(request, &self.model)?;
        let payload =
            serde_json::to_string(&body).map_err(|err| ProviderError::InvalidRequest {
                detail: err.to_string(),
            })?;
        let token = self.token_source().await?.access_token().await?;
        let url = self.stream_url();
        retry::climb(
            &self.client,
            move |client| {
                client
                    .post(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(payload.clone())
            },
            http_failure,
        )
        .await
    }
}

impl Provider for VertexProvider {
    async fn stream(&self, request: ModelRequest) -> Result<ModelEventStream, ProviderError> {
        let response = self.connect(&request).await?;
        Ok(lane::lane_stream(response, self.sse))
    }
}

/// The documented service endpoint of one location: the global endpoint
/// rides the unprefixed host, every regional location its own prefixed
/// host.
pub fn endpoint_url(location: &str) -> String {
    let host = if location == "global" {
        String::from(GLOBAL_HOST)
    } else {
        format!("{location}{HOST_SUFFIX}")
    };
    format!("https://{host}")
}

/// A mount-time guard: project, location, and model must carry real
/// values, or the failure names the empty part before any wire bytes.
fn reject_empty(part: &str, value: &str) -> Result<(), ProviderError> {
    if value.trim().is_empty() {
        return Err(ProviderError::InvalidRequest {
            detail: format!("vertex {part} must not be empty"),
        });
    }
    Ok(())
}
