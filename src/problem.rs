//! The service's one error type and its answer: every error is an RFC 9457 problem
//! document, `application/problem+json` with `type`, `title`, `status` and `detail`, and each situation
//! maps to its status here and nowhere else. No body carries a stack trace, a file name,
//! a path or document content: `detail` is a fixed sentence per situation, worded without naming the backend, so the
//! answers survive a change of backend and do not show which one runs.

use std::time::Duration;

use axum::http::header::{CONNECTION, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// The media type of every error body (RFC 9457 §3).
pub const PROBLEM_JSON: &str = "application/problem+json";

/// The `detail` of the 408 for a request whose client stopped sending.
pub const IDLE_TIMEOUT_DETAIL: &str = "no part of the request arrived within the idle limit";

/// Every way a request can fail, one variant per status the service answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Not multipart, malformed multipart, no `document` part: 400.
    BadRequest(&'static str),
    /// An unknown path: 404.
    NotFound,
    /// A known path, a wrong method: 405 with `Allow` (axum's router fills it in from the route).
    MethodNotAllowed,
    /// No byte of the request's head or body for the idle limit: 408, and the connection is closed.
    RequestTimeout(&'static str),
    /// The document or the request over the size limit, a DOCX or ODT whose parts inflate past the inflated-size
    /// limit, or a conversion over the hard memory limit: 413, the size limit's kind of refusal — a limit the
    /// service sets, which this document passes every time, so a retry fails again and the remedy is an operator
    /// raising a setting.
    TooLarge(&'static str),
    /// A format outside the allowlist: 415.
    UnsupportedFormat(&'static str),
    /// An allowed format that the backend cannot load: 422.
    CannotLoad(&'static str),
    /// No instance free within the wait limit, not ready, shutting down: 503 with `Retry-After`.
    Unavailable {
        /// The problem document's `detail`.
        detail: &'static str,
        /// The `Retry-After` the answer carries.
        retry_after: Duration,
    },
    /// The conversion exceeded its timeout: 504.
    Timeout(&'static str),
    /// Anything else: 500.
    Internal,
}

impl Problem {
    /// The status this situation is answered with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::RequestTimeout(_) => StatusCode::REQUEST_TIMEOUT,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedFormat(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::CannotLoad(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The human-readable sentence; never names the request's path, file or content.
    pub fn detail(&self) -> &'static str {
        match self {
            Self::BadRequest(d)
            | Self::RequestTimeout(d)
            | Self::TooLarge(d)
            | Self::UnsupportedFormat(d)
            | Self::CannotLoad(d)
            | Self::Timeout(d)
            | Self::Unavailable { detail: d, .. } => d,
            Self::NotFound => {
                "no such path; the service converts at POST /pdf and POST /docx-to-pdf"
            }
            Self::MethodNotAllowed => "this path does not take this method; see Allow",
            Self::Internal => "the service failed to answer this request",
        }
    }

    /// The problem document's JSON.
    pub fn body(&self) -> String {
        let status = self.status();
        serde_json::json!({
            "type": "about:blank",
            "title": status.canonical_reason().unwrap_or("Error"),
            "status": status.as_u16(),
            "detail": self.detail(),
        })
        .to_string()
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut response = (
            self.status(),
            [(CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
            self.body(),
        )
            .into_response();
        if matches!(self, Self::RequestTimeout(_)) {
            // The request was not read to its end, so the connection cannot carry another (RFC 9110 §15.5.9).
            response
                .headers_mut()
                .insert(CONNECTION, HeaderValue::from_static("close"));
        }
        if let Self::Unavailable { retry_after, .. } = &self {
            // Whole seconds, at least one (RFC 9110 §10.2.3).
            let seconds = retry_after.as_secs().max(1);
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn answer(problem: Problem) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = problem.into_response();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn every_situation_has_adr_0005s_status_and_a_problem_document() {
        for (problem, status) in [
            (Problem::BadRequest("x"), 400),
            (Problem::NotFound, 404),
            (Problem::MethodNotAllowed, 405),
            (Problem::RequestTimeout("x"), 408),
            (Problem::TooLarge("x"), 413),
            (Problem::UnsupportedFormat("x"), 415),
            (Problem::CannotLoad("x"), 422),
            (
                Problem::Unavailable {
                    detail: "x",
                    retry_after: Duration::from_secs(5),
                },
                503,
            ),
            (Problem::Timeout("x"), 504),
            (Problem::Internal, 500),
        ] {
            let (got, headers, json) = answer(problem.clone()).await;
            assert_eq!(got.as_u16(), status, "{problem:?}");
            assert_eq!(headers[CONTENT_TYPE], PROBLEM_JSON);
            assert_eq!(json["type"], "about:blank");
            assert_eq!(json["status"], status);
            assert_eq!(json["title"], got.canonical_reason().unwrap());
            assert_eq!(json["detail"], problem.detail());
        }
    }

    #[tokio::test]
    async fn unavailable_carries_retry_after_in_whole_seconds_at_least_one() {
        let (_, headers, _) = answer(Problem::Unavailable {
            detail: "x",
            retry_after: Duration::from_secs(7),
        })
        .await;
        assert_eq!(headers[RETRY_AFTER], "7");
        let (_, headers, _) = answer(Problem::Unavailable {
            detail: "x",
            retry_after: Duration::from_millis(10),
        })
        .await;
        assert_eq!(headers[RETRY_AFTER], "1");
        let (_, headers, _) = answer(Problem::NotFound).await;
        assert!(!headers.contains_key(RETRY_AFTER));
    }

    #[test]
    fn a_detail_is_escaped_as_json() {
        let body = Problem::BadRequest(r#"a "quoted" \ detail"#).body();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["detail"], r#"a "quoted" \ detail"#);
    }
}
