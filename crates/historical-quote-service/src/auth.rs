use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use historical_quote::api::{ApiError, ApiErrorCode};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BearerKey {
    id: String,
    digest: [u8; 32],
}

impl BearerKey {
    #[must_use]
    pub fn new(id: String, digest: [u8; 32]) -> Self {
        Self { id, digest }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone)]
pub struct BearerTokenVerifier {
    keys: Vec<BearerKey>,
}

impl BearerTokenVerifier {
    #[must_use]
    pub fn new(keys: Vec<BearerKey>) -> Self {
        Self { keys }
    }

    #[must_use]
    pub fn verify(&self, presented: &str) -> Option<&str> {
        let candidate: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        let mut matched_id = None;
        for key in &self.keys {
            if bool::from(candidate.ct_eq(&key.digest)) {
                matched_id = Some(key.id());
            }
        }
        matched_id
    }
}

pub async fn require_bearer(
    State(verifier): State<BearerTokenVerifier>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_value);
    if let Some(key_id) = token.and_then(|token| verifier.verify(token)) {
        tracing::info!(
            bearer_key_id = key_id,
            method = %request.method(),
            path = request.uri().path(),
            "historical quote API request authenticated"
        );
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(ApiError {
            code: ApiErrorCode::Unauthorized,
            message: "bearer token is missing or invalid".to_owned(),
            retryable: false,
            details: None,
        }),
    )
        .into_response()
}

fn bearer_value(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && !token.contains(' '))
        .then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_each_matching_bearer_key() {
        let verifier = BearerTokenVerifier::new(vec![
            BearerKey::new(
                "edson".to_owned(),
                Sha256::digest(b"edson-test-token").into(),
            ),
            BearerKey::new(
                "pedro".to_owned(),
                Sha256::digest(b"pedro-test-token").into(),
            ),
        ]);

        assert_eq!(verifier.verify("edson-test-token"), Some("edson"));
        assert_eq!(verifier.verify("pedro-test-token"), Some("pedro"));
        assert_eq!(verifier.verify("unknown-token"), None);
        assert_eq!(verifier.verify(""), None);
    }

    #[test]
    fn accepts_one_bearer_value_without_normalizing_the_secret() {
        assert_eq!(bearer_value("Bearer exact-token"), Some("exact-token"));
        assert_eq!(bearer_value("bearer exact-token"), Some("exact-token"));
        assert_eq!(bearer_value("Bearer exact-token "), None);
        assert_eq!(bearer_value("Bearer  exact-token"), None);
        assert_eq!(bearer_value("Basic exact-token"), None);
    }
}
