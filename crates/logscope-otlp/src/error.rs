//! OTLP spike errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OtlpError {
    #[error("failed to bind loopback listener: {0}")]
    Bind(#[source] std::io::Error),
    #[error("envelope error: {0}")]
    Envelope(String),
}
