//! Public authenticated telemetry accepts operational codes, never arbitrary payloads.
use crate::{api::ScopedState, error::AppError};
use axum::{
    Json,
    extract::rejection,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientObservation {
    source: Source,
    event: EventName,
    step: Step,
    success: bool,
    duration_ms: u64,
    status_code: Option<u16>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    RustClient,
    Cli,
    Daemon,
    Web,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum EventName {
    RequestCompleted,
    CommandCompleted,
    UpdateCompleted,
    Heartbeat,
    PageView,
    Navigation,
    Interaction,
    Error,
    Performance,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    Response,
    Command,
    Maintenance,
    Browser,
}
pub(crate) async fn record(
    ScopedState(state): ScopedState,
    headers: HeaderMap,
    body: Result<Json<ClientObservation>, rejection::JsonRejection>,
) -> Result<StatusCode, AppError> {
    let Json(event) = body.map_err(|_| AppError::Validation)?;
    if event.duration_ms > 86_400_000
        || event.status_code.is_some_and(|s| !(100..=599).contains(&s))
    {
        return Err(AppError::Validation);
    }
    if headers.get("x-remind-telemetry").is_none_or(|v| v != "off") {
        state
            .telemetry
            .record(
                state.repository.pool(),
                state.is_test,
                serde_json::to_value(event)
                    .map_err(|e| AppError::internal("telemetry_encoding", e))?,
            )
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_arbitrary_details_and_secret_bearing_event_names() {
        let valid = serde_json::json!({"source":"cli","event":"command_completed","step":"command","success":true,"duration_ms":10,"status_code":null});
        assert!(serde_json::from_value::<ClientObservation>(valid.clone()).is_ok());
        let mut bad = valid.clone();
        bad["token"] = "secret".into();
        assert!(serde_json::from_value::<ClientObservation>(bad).is_err());
        let mut bad = valid;
        bad["event"] = "ask_secret".into();
        assert!(serde_json::from_value::<ClientObservation>(bad).is_err());
    }
}
