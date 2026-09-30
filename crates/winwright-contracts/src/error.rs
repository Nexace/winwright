use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type WinwrightResult<T> = Result<T, WinwrightError>;

/// Stable wire codes (spec §32). Never rename or remove a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ElementNotFound,
    ElementAmbiguous,
    ElementStale,
    WindowNotFound,
    WindowNotFocused,
    UnsupportedPattern,
    ActionBlocked,
    ConfirmationRequired,
    UipiBlocked,
    Timeout,
    ProcessExited,
    CaptureFailed,
    DpiConversionFailed,
    InputFailed,
    VisionNoMatch,
    Cancelled,
    DesktopBusy,
    BackendUnavailable,
    ActionOutcomeUnknown,
    SensitiveField,
    InvalidRequest,
    PlatformError,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ElementNotFound => "ELEMENT_NOT_FOUND",
            Self::ElementAmbiguous => "ELEMENT_AMBIGUOUS",
            Self::ElementStale => "ELEMENT_STALE",
            Self::WindowNotFound => "WINDOW_NOT_FOUND",
            Self::WindowNotFocused => "WINDOW_NOT_FOCUSED",
            Self::UnsupportedPattern => "UNSUPPORTED_PATTERN",
            Self::ActionBlocked => "ACTION_BLOCKED",
            Self::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Self::UipiBlocked => "UIPI_BLOCKED",
            Self::Timeout => "TIMEOUT",
            Self::ProcessExited => "PROCESS_EXITED",
            Self::CaptureFailed => "CAPTURE_FAILED",
            Self::DpiConversionFailed => "DPI_CONVERSION_FAILED",
            Self::InputFailed => "INPUT_FAILED",
            Self::VisionNoMatch => "VISION_NO_MATCH",
            Self::Cancelled => "CANCELLED",
            Self::DesktopBusy => "DESKTOP_BUSY",
            Self::BackendUnavailable => "BACKEND_UNAVAILABLE",
            Self::ActionOutcomeUnknown => "ACTION_OUTCOME_UNKNOWN",
            Self::SensitiveField => "SENSITIVE_FIELD",
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::PlatformError => "PLATFORM_ERROR",
        }
    }
}

/// Internal typed error. Serialize through [`WinwrightError::payload`], never `Debug`.
#[derive(Debug, thiserror::Error)]
pub enum WinwrightError {
    #[error("no element matched {locator}")]
    ElementNotFound { locator: String },
    #[error("{} elements matched {locator}", matches.len())]
    ElementAmbiguous {
        locator: String,
        matches: Vec<String>,
    },
    #[error("reference {reference} is stale: {reason}")]
    ElementStale { reference: String, reason: String },
    #[error("no window matched {query}")]
    WindowNotFound { query: String },
    #[error("window {window} is not focused")]
    WindowNotFocused { window: String },
    #[error("{element} does not support {pattern}")]
    UnsupportedPattern { element: String, pattern: String },
    #[error("action blocked: {reason}")]
    ActionBlocked { reason: String },
    #[error("confirmation required: {reason}")]
    ConfirmationRequired { reason: String },
    #[error("{target} runs at a higher integrity level than Winwright")]
    UipiBlocked { target: String },
    #[error("{operation} timed out after {elapsed_ms} ms")]
    Timeout { operation: String, elapsed_ms: u64 },
    #[error("process {pid} exited")]
    ProcessExited { pid: u32 },
    #[error("capture failed: {reason}")]
    CaptureFailed { reason: String },
    #[error("DPI conversion failed: {reason}")]
    DpiConversionFailed { reason: String },
    #[error("input failed: {reason}")]
    InputFailed { reason: String },
    #[error("vision found no match for {instruction:?}")]
    VisionNoMatch { instruction: String },
    #[error("operation cancelled")]
    Cancelled,
    #[error("desktop is controlled by session {holder}")]
    DesktopBusy { holder: String },
    #[error("{backend} backend unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },
    #[error("{operation} may or may not have executed: {reason}")]
    ActionOutcomeUnknown { operation: String, reason: String },
    #[error("{element} is a sensitive field; reading it is blocked")]
    SensitiveField { element: String },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("{operation} failed with HRESULT {hresult:#010x}")]
    Platform { operation: String, hresult: i32 },
}

impl WinwrightError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ElementNotFound { .. } => ErrorCode::ElementNotFound,
            Self::ElementAmbiguous { .. } => ErrorCode::ElementAmbiguous,
            Self::ElementStale { .. } => ErrorCode::ElementStale,
            Self::WindowNotFound { .. } => ErrorCode::WindowNotFound,
            Self::WindowNotFocused { .. } => ErrorCode::WindowNotFocused,
            Self::UnsupportedPattern { .. } => ErrorCode::UnsupportedPattern,
            Self::ActionBlocked { .. } => ErrorCode::ActionBlocked,
            Self::ConfirmationRequired { .. } => ErrorCode::ConfirmationRequired,
            Self::UipiBlocked { .. } => ErrorCode::UipiBlocked,
            Self::Timeout { .. } => ErrorCode::Timeout,
            Self::ProcessExited { .. } => ErrorCode::ProcessExited,
            Self::CaptureFailed { .. } => ErrorCode::CaptureFailed,
            Self::DpiConversionFailed { .. } => ErrorCode::DpiConversionFailed,
            Self::InputFailed { .. } => ErrorCode::InputFailed,
            Self::VisionNoMatch { .. } => ErrorCode::VisionNoMatch,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::DesktopBusy { .. } => ErrorCode::DesktopBusy,
            Self::BackendUnavailable { .. } => ErrorCode::BackendUnavailable,
            Self::ActionOutcomeUnknown { .. } => ErrorCode::ActionOutcomeUnknown,
            Self::SensitiveField { .. } => ErrorCode::SensitiveField,
            Self::InvalidRequest { .. } => ErrorCode::InvalidRequest,
            Self::Platform { .. } => ErrorCode::PlatformError,
        }
    }

    /// Recovery hint shown to the model alongside the code.
    pub fn hint(&self) -> Option<&'static str> {
        Some(match self {
            Self::ElementNotFound { .. } => {
                "Take a fresh snapshot or relax the locator (contains match, fewer predicates)."
            }
            Self::ElementAmbiguous { .. } => {
                "Narrow by parent/window/AutomationId, or pass nth explicitly."
            }
            Self::ElementStale { .. } => "Take a new snapshot and use the fresh ref.",
            Self::WindowNotFound { .. } => {
                "List open windows first and match by exact title or process."
            }
            Self::WindowNotFocused { .. } => "Focus the window before sending input.",
            Self::UnsupportedPattern { .. } => {
                "Use an action the control supports, or allow the physical-input fallback."
            }
            Self::ConfirmationRequired { .. } => {
                "Ask the user to approve this in Winwright; model-supplied flags cannot approve."
            }
            Self::UipiBlocked { .. } => {
                "The target runs elevated. The user must explicitly start the elevated helper."
            }
            Self::Timeout { .. } => "Check the target app is responsive, or raise timeoutMs.",
            Self::DesktopBusy { .. } => {
                "Wait for the other session to finish, or request an explicit takeover."
            }
            Self::BackendUnavailable { .. } => {
                "Winwright must run in the signed-in user's interactive desktop session."
            }
            Self::ActionOutcomeUnknown { .. } => {
                "Observe the UI before retrying; the action may already have happened."
            }
            Self::ActionBlocked { .. }
            | Self::ProcessExited { .. }
            | Self::CaptureFailed { .. }
            | Self::DpiConversionFailed { .. }
            | Self::InputFailed { .. }
            | Self::VisionNoMatch { .. }
            | Self::Cancelled
            | Self::SensitiveField { .. }
            | Self::InvalidRequest { .. }
            | Self::Platform { .. } => return None,
        })
    }

    pub fn payload(&self) -> ErrorPayload {
        ErrorPayload {
            error: self.code(),
            message: self.to_string(),
            hint: self.hint().map(str::to_owned),
            matches: match self {
                Self::ElementAmbiguous { matches, .. } => matches.clone(),
                _ => Vec::new(),
            },
        }
    }
}

/// Wire form of an error, e.g. `{"error":"ELEMENT_AMBIGUOUS","message":…,"matches":[…],"hint":…}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorPayload {
    pub error: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_code_matches_as_str() {
        let codes = [
            ErrorCode::ElementNotFound,
            ErrorCode::UipiBlocked,
            ErrorCode::DpiConversionFailed,
            ErrorCode::ActionOutcomeUnknown,
            ErrorCode::PlatformError,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn ambiguous_payload_carries_matches_and_hint() {
        let err = WinwrightError::ElementAmbiguous {
            locator: "Button \"Save\"".into(),
            matches: vec!["e12".into(), "e19".into(), "e22".into()],
        };
        let payload = err.payload();
        assert_eq!(payload.error, ErrorCode::ElementAmbiguous);
        assert_eq!(payload.message, "3 elements matched Button \"Save\"");
        assert_eq!(payload.matches, ["e12", "e19", "e22"]);
        assert!(payload.hint.unwrap().contains("AutomationId"));
    }

    #[test]
    fn platform_error_formats_hresult_as_unsigned_hex() {
        let err = WinwrightError::Platform {
            operation: "ElementFromHandle".into(),
            hresult: 0x8000_4005_u32 as i32,
        };
        assert_eq!(
            err.to_string(),
            "ElementFromHandle failed with HRESULT 0x80004005"
        );
        assert_eq!(err.code().as_str(), "PLATFORM_ERROR");
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let json = serde_json::to_value(WinwrightError::Cancelled.payload()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": "CANCELLED", "message": "operation cancelled"})
        );
    }
}
