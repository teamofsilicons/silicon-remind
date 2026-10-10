//! Typed failures. Every error says what went wrong and why; most also say what to do next.

/// Result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong talking to Remind or signing in with Silicon Accounts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Remind answered with its error body `{"error":{"code","message","hint"?,"request_id"}}`.
    #[error("{message} ({code}; HTTP {status}){}", hint_suffix(.hint.as_deref()))]
    Api {
        /// HTTP status.
        status: u16,
        /// Stable machine-readable code, e.g. `not_reminder_owner`, `token_revoked`.
        code: String,
        /// What went wrong and why.
        message: String,
        /// What to do next, when Remind said.
        hint: Option<String>,
        /// Quote it when reporting a problem.
        request_id: Option<String>,
        /// Seconds to wait before retrying, from `Retry-After`.
        retry_after: Option<u64>,
    },
    /// Silicon Accounts answered with an error, or could not be reached (`status` is `None`).
    #[error("{message}{}", hint_suffix(.hint.as_deref()))]
    Accounts {
        /// HTTP status, when Silicon Accounts answered.
        status: Option<u16>,
        /// Silicon Accounts' error code, or `connection_failed` / `request_timeout`.
        code: String,
        /// What went wrong and why.
        message: String,
        /// What to do next.
        hint: Option<String>,
        /// Silicon Accounts' request id, when it answered.
        request_id: Option<String>,
    },
    /// Silicon Accounts refused a sign-in: a short-lived token, a refresh token or a device
    /// code. Signing in again (with a new token or code) is the fix.
    #[error("{message} {hint}")]
    SignInRefused {
        /// Why, as a stable value.
        refusal: Refusal,
        /// Silicon Accounts' own description.
        message: String,
        /// What to do next.
        hint: String,
    },
    /// Remind could not be reached (connection, DNS, TLS or timeout).
    #[error("could not reach Silicon Remind: {0}")]
    Transport(#[source] reqwest::Error),
    /// Input rejected before anything was sent.
    #[error("{0}")]
    Invalid(String),
    /// Remind answered with a body this client does not understand.
    #[error("Silicon Remind returned a response this client does not understand")]
    Decode,
    /// A response body exceeded the 16 MiB bound.
    #[error("Silicon Remind response exceeded the 16 MiB limit")]
    ResponseTooLarge,
}

fn hint_suffix(hint: Option<&str>) -> String {
    hint.filter(|h| !h.trim().is_empty())
        .map(|h| format!(" Hint: {h}"))
        .unwrap_or_default()
}

/// Why Silicon Accounts refused a sign-in. Every refused short-lived token is used up.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The short-lived token was already exchanged; each works once.
    SltAlreadyUsed,
    /// The short-lived token expired (they last 2 minutes).
    SltExpired,
    /// The short-lived token was minted for another app (named when Silicon Accounts said).
    SltWrongApp {
        /// The app it was minted for.
        app: Option<String>,
    },
    /// Mistyped, never issued, or issued by another Silicon Accounts deployment.
    SltUnknown,
    /// Not a short-lived token at all (they start with `slt_`).
    NotAShortLivedToken,
    /// The sign-in a refresh token belonged to has ended (signed out elsewhere, revoked,
    /// expired, or a used refresh token was presented again).
    SignInEnded,
    /// The Carbon denied the device sign-in on the account site.
    DeviceDenied,
    /// The device code expired before it was approved (codes last 10 minutes).
    DeviceExpired,
    /// Remind's sign-in setup does not allow this kind of sign-in from its CLI.
    NotAllowed,
    /// Any other refusal; the message says why.
    Other,
}

impl Refusal {
    /// Stable code for machine-readable output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SltAlreadyUsed => "slt_already_used",
            Self::SltExpired => "slt_expired",
            Self::SltWrongApp { .. } => "slt_wrong_app",
            Self::SltUnknown => "slt_unknown",
            Self::NotAShortLivedToken => "not_a_short_lived_token",
            Self::SignInEnded => "sign_in_ended",
            Self::DeviceDenied => "device_denied",
            Self::DeviceExpired => "device_expired",
            Self::NotAllowed => "sign_in_not_allowed",
            Self::Other => "sign_in_refused",
        }
    }
}

impl Error {
    /// Stable machine-readable code.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } | Self::Accounts { code, .. } => code,
            Self::SignInRefused { refusal, .. } => refusal.code(),
            Self::Transport(error) if error.is_timeout() => "request_timeout",
            Self::Transport(_) => "connection_failed",
            Self::Invalid(_) => "invalid_input",
            Self::Decode => "unexpected_response",
            Self::ResponseTooLarge => "response_too_large",
        }
    }

    /// What went wrong and why, without the hint.
    pub fn message(&self) -> String {
        match self {
            Self::Api { message, .. }
            | Self::Accounts { message, .. }
            | Self::SignInRefused { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }

    /// What to do next, when known.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api { hint, .. } | Self::Accounts { hint, .. } => hint.as_deref(),
            Self::SignInRefused { hint, .. } => Some(hint),
            _ => None,
        }
    }

    /// HTTP status of the failed response, when there was one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            Self::Accounts { status, .. } => *status,
            _ => None,
        }
    }

    /// The request id of the failed response, when there was one.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } | Self::Accounts { request_id, .. } => {
                request_id.as_deref()
            }
            _ => None,
        }
    }

    /// True when Remind or Silicon Accounts answered with this code.
    pub fn is_code(&self, code: &str) -> bool {
        self.code() == code
    }

    /// True when the failure is passing (no answer, 5xx, 429) and a later retry may work.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { status, .. } => *status >= 500 || *status == 429,
            Self::Accounts { status, .. } => status.is_none_or(|s| s >= 500 || s == 429),
            _ => false,
        }
    }

    pub(crate) fn accounts(error: &silicon_accounts_client::Error) -> Self {
        Self::Accounts {
            status: error.status(),
            code: error.code().to_owned(),
            message: error.message(),
            hint: error.hint(),
            request_id: error.request_id().map(str::to_owned),
        }
    }
}
