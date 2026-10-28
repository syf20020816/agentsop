//! 统一错误模型：携带稳定错误码，CLI / HTTP / IPC 共用。

use std::fmt;

#[derive(Debug, Clone)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
    pub line: Option<usize>,
}

impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        AppError {
            code,
            message: message.into(),
            line: None,
        }
    }

    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    pub fn sop(message: impl Into<String>) -> Self {
        AppError::new("SOP_INVALID", message)
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        AppError::new("NOT_FOUND", what)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        AppError::new("STATE_CONFLICT", message)
    }

    pub fn invalid_transition(message: impl Into<String>) -> Self {
        AppError::new("INVALID_TRANSITION", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        AppError::new("FORBIDDEN", message)
    }

    pub fn io(err: std::io::Error) -> Self {
        AppError::new("IO_ERROR", err.to_string())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "code": self.code,
            "message": self.message,
            "line": self.line,
        })
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "{} ({}: 第 {} 行)", self.message, self.code, l),
            None => write!(f, "{} ({})", self.message, self.code),
        }
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::io(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::new("BAD_JSON", err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
