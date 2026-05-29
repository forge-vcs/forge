use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: &str = "forge.cli.v0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryMetadata {
    pub retryable: bool,
    pub after_ms: Option<u64>,
}

impl RetryMetadata {
    pub fn no() -> Self {
        Self {
            retryable: false,
            after_ms: None,
        }
    }

    /// A retryable result with an advisory backoff hint (HTTP `Retry-After`-style).
    /// Bounding the number of retries is the client's responsibility.
    pub fn retryable(after_ms: Option<u64>) -> Self {
        Self {
            retryable: true,
            after_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorObject {
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl ErrorObject {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Value::Object(Default::default()),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponseEnvelope {
    pub schema_version: String,
    pub command: String,
    pub request_id: Option<String>,
    pub operation_id: Option<String>,
    pub status: ResponseStatus,
    pub data: Value,
    pub warnings: Vec<String>,
    pub errors: Vec<ErrorObject>,
    pub retry: RetryMetadata,
}

impl ResponseEnvelope {
    pub fn success(
        command: impl Into<String>,
        request_id: Option<String>,
        operation_id: Option<String>,
        data: Value,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            command: command.into(),
            request_id,
            operation_id,
            status: ResponseStatus::Success,
            data,
            warnings: Vec::new(),
            errors: Vec::new(),
            retry: RetryMetadata::no(),
        }
    }

    pub fn error(
        command: impl Into<String>,
        request_id: Option<String>,
        operation_id: Option<String>,
        error: ErrorObject,
    ) -> Self {
        Self::error_with(
            command,
            request_id,
            operation_id,
            error,
            RetryMetadata::no(),
        )
    }

    /// Build an error envelope with an explicit [`RetryMetadata`] (e.g. a
    /// retryable `LOCK_TIMEOUT` / `CONFLICT`). `retry` is a top-level envelope
    /// field, not part of the [`ErrorObject`].
    pub fn error_with(
        command: impl Into<String>,
        request_id: Option<String>,
        operation_id: Option<String>,
        error: ErrorObject,
        retry: RetryMetadata,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            command: command.into(),
            request_id,
            operation_id,
            status: ResponseStatus::Error,
            data: Value::Object(Default::default()),
            warnings: Vec::new(),
            errors: vec![error],
            retry,
        }
    }
}
