//! Silicon-owned Ting delivery setup. Test environments simulate delivery.
use axum::{
    Extension, Json,
    response::{IntoResponse as _, Response},
};
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde_json::json;

use crate::{
    api::{ApiState, ScopedState},
    application::schedules::require_writing_silicon,
    domain::Actor,
    error::AppError,
};

async fn registered_at(state: &ApiState, uuid: &str) -> Result<Option<DateTime<Utc>>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT ting_registered_at FROM accounts WHERE uuid=$1")
            .bind(uuid)
            .fetch_one(state.identity.pool())
            .await?,
    )
}

pub(crate) async fn enrol(
    state: &ApiState,
    actor: &Actor,
    token: &SecretString,
    explicit: bool,
) -> Result<(), AppError> {
    let Some(ting) = &state.ting else {
        return Err(AppError::DependencyUnavailable { dependency: "Ting" });
    };
    if !explicit && registered_at(state, &actor.uuid).await?.is_some() {
        return Ok(());
    }
    ting.enrol(&actor.uuid, token).await?;
    sqlx::query("UPDATE accounts SET ting_registered_at=clock_timestamp() WHERE uuid=$1 AND status='active'")
        .bind(&actor.uuid).execute(state.identity.pool()).await?;
    Ok(())
}

pub(crate) async fn status(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<Response, AppError> {
    require_writing_silicon(&actor)?;
    let registered = if state.is_test {
        None
    } else {
        registered_at(&state, &actor.uuid).await?
    };
    Ok(super::accounts::no_store(Json(json!({"configured":state.ting.is_some(), "simulated":state.is_test, "registered_at":registered})).into_response()))
}

pub(crate) async fn enable(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    Extension(token): Extension<SecretString>,
) -> Result<Response, AppError> {
    require_writing_silicon(&actor)?;
    if !state.is_test {
        enrol(&state, &actor, &token, true).await?;
    }
    status(ScopedState(state), Extension(actor)).await
}
