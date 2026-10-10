//! Quota classification rows: the RetryInfo delay rides retryable rate
//! limits, the documented daily-quota markers fail fast, and everything
//! else stays fail-open, on the HTTP and mid-stream paths alike.

use crate::mock::{CannedResponse, MockServer};
use crate::wire;
use crate::{TestError, detail_of, provider, run, sample_request, surfaced};
use craft_core::error::ProviderError;
use std::time::Duration;

/// Serves the canned rows and yields the surfaced error.
async fn error_over(
    responses: Vec<CannedResponse>,
) -> Result<(MockServer, ProviderError), TestError> {
    let server = MockServer::start(responses, "").await?;
    let events = run(&provider(&server.base_url)?, sample_request()).await;
    Ok((server, surfaced(&events)?))
}

/// Five identical 429 rows: the streak surfaces before the ladder runs
/// dry.
async fn rate_streak(body: &str) -> Result<(MockServer, ProviderError), TestError> {
    let responses = (0..5).map(|_| CannedResponse::json(429, body)).collect();
    error_over(responses).await
}

/// One canned spent-quota row: fail fast on the first request.
async fn spent_quota_row(body: &str) -> Result<ProviderError, TestError> {
    let (server, err) = error_over(vec![CannedResponse::json(429, body)]).await?;
    assert!(
        matches!(err, ProviderError::QuotaExhausted { .. }),
        "the daily-quota marker must fail fast: {err:?}"
    );
    assert_eq!(server.request_count(), 1);
    Ok(err)
}

/// Serves one SSE stream and yields the surfaced error.
async fn mid_stream_error(body: String) -> Result<ProviderError, TestError> {
    let server = MockServer::start(vec![CannedResponse::sse(body.as_bytes())], "").await?;
    let events = run(&provider(&server.base_url)?, sample_request()).await;
    surfaced(&events)
}

#[tokio::test(start_paused = true)]
async fn retryinfo_details_carry_the_rate_limit_delay() -> Result<(), TestError> {
    let (server, err) = rate_streak(wire::RATE_RETRY_INFO_BODY).await?;
    assert_eq!(
        err,
        ProviderError::RateLimit {
            retry_after: Some(Duration::from_secs(42)),
        },
        "the RetryInfo delay rides the surfaced rate limit"
    );
    assert_eq!(server.request_count(), 5);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn fractional_retryinfo_delay_survives_intact() -> Result<(), TestError> {
    let (_, err) = rate_streak(wire::RATE_RETRY_FRACTIONAL_BODY).await?;
    assert_eq!(
        err,
        ProviderError::RateLimit {
            retry_after: Some(Duration::new(1, 212_000_000)),
        },
        "the documented fractional-second form parses without truncation"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unactionable_retryinfo_delays_fall_back_to_the_ladder() -> Result<(), TestError> {
    for body in [wire::RATE_RETRY_JUNK_BODY, wire::RATE_RETRY_NEGATIVE_BODY] {
        let (_, err) = rate_streak(body).await?;
        assert_eq!(
            err,
            ProviderError::RateLimit { retry_after: None },
            "an unparseable or negative delay is not actionable: {err:?}"
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_daily_quota_code_fails_fast() -> Result<(), TestError> {
    let err = spent_quota_row(wire::SPENT_QUOTA_BODY).await?;
    assert!(
        detail_of(&err).contains("daily quota"),
        "the documented message lands in the detail: {err:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_daily_quota_message_fails_fast_on_the_status_shape() -> Result<(), TestError> {
    spent_quota_row(wire::SPENT_QUOTA_MESSAGE_BODY).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn quota_details_without_the_daily_marker_stay_retryable() -> Result<(), TestError> {
    let (server, err) = rate_streak(wire::RATE_QUOTA_FAILURE_BODY).await?;
    assert_eq!(
        err,
        ProviderError::RateLimit { retry_after: None },
        "fail-open: no daily marker, no fail-fast"
    );
    assert_eq!(server.request_count(), 5);
    Ok(())
}

#[tokio::test]
async fn mid_stream_retryinfo_delay_rides_the_rate_limit() -> Result<(), TestError> {
    let err = mid_stream_error(wire::stream_rate_retry_body()).await?;
    assert_eq!(
        err,
        ProviderError::RateLimit {
            retry_after: Some(Duration::from_secs(7)),
        },
        "the RetryInfo delay survives the mid-stream envelope"
    );
    Ok(())
}

#[tokio::test]
async fn mid_stream_resource_exhausted_stays_retryable() -> Result<(), TestError> {
    let err = mid_stream_error(wire::stream_rate_body()).await?;
    assert_eq!(
        err,
        ProviderError::RateLimit { retry_after: None },
        "a plain RESOURCE_EXHAUSTED stays on the retry ladder"
    );
    Ok(())
}

#[tokio::test]
async fn mid_stream_daily_quota_fails_fast() -> Result<(), TestError> {
    let err = mid_stream_error(wire::stream_spent_quota_body()).await?;
    assert!(
        matches!(err, ProviderError::QuotaExhausted { .. }),
        "the daily-quota message marks the mid-stream envelope: {err:?}"
    );
    assert!(detail_of(&err).contains("daily quota"));
    Ok(())
}
