use axum::http::StatusCode;
use tycho_execution::encoding::errors::EncodingError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeErrorKind {
    InvalidRequest,
    NotFound,
    Simulation,
    Encoding,
    Internal,
}

#[derive(Debug)]
pub struct EncodeError {
    kind: EncodeErrorKind,
    message: String,
}

impl EncodeError {
    pub fn invalid<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::InvalidRequest,
            message: message.into(),
        }
    }

    pub fn not_found<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn simulation<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Simulation,
            message: message.into(),
        }
    }

    pub fn encoding<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Encoding,
            message: message.into(),
        }
    }

    pub fn internal<T: Into<String>>(message: T) -> Self {
        Self {
            kind: EncodeErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self.kind {
            EncodeErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            EncodeErrorKind::NotFound => StatusCode::NOT_FOUND,
            EncodeErrorKind::Simulation => StatusCode::UNPROCESSABLE_ENTITY,
            EncodeErrorKind::Encoding | EncodeErrorKind::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn kind(&self) -> EncodeErrorKind {
        self.kind
    }
}

pub(super) fn map_encoding_error(err: EncodingError) -> EncodeError {
    match err {
        EncodingError::InvalidInput(_) => {
            EncodeError::invalid(format!("Tycho encoding error: {err}"))
        }
        _ => EncodeError::encoding(format!("Tycho encoding error: {err}")),
    }
}
