//! The signed-in account and the Silicons it can see.

use axum::{
    Extension, Json,
    extract::{Query, rejection},
    http::{HeaderValue, header},
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;

use crate::{
    api::{ScopedState, models},
    domain::{Actor, ActorKind, Credential},
    error::AppError,
};

/// `GET /api/v2/auth/me`: who is calling, as Remind sees the account.
pub async fn me(Extension(actor): Extension<Actor>) -> Response {
    let (credential, issuing_app) = match &actor.credential {
        Credential::AccessToken { .. } => ("access_token", None),
        Credential::Proof { issuing_app, .. } => ("proof", Some(issuing_app.clone())),
    };
    let visible_silicons = actor
        .visible
        .iter()
        .filter(|owner| owner.account.kind == Some(ActorKind::Silicon))
        .count();
    no_store(
        Json(serde_json::json!({
            "uuid": actor.uuid,
            "kind": actor.kind,
            "id": actor.public_id,
            "display_name": actor.display_name,
            "pfp_url": actor.pfp_url,
            "custodian": actor.custodian,
            "can_manage_reminders": actor.is_silicon() && actor.can_write(),
            "credential": credential,
            "issuing_app": issuing_app,
            "visible_silicons": visible_silicons,
        }))
        .into_response(),
    )
}

/// Pagination for the visible Silicons.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiliconQuery {
    after: Option<String>,
    limit: Option<usize>,
}

/// `GET /api/v2/silicons`: the Silicons whose reminders the caller sees — itself,
/// the Silicons it looks after, the other Silicons of its custodian, and those
/// that shared with it — with retained reminder counts.
///
/// # Errors
///
/// Returns validation for invalid pagination or a database failure.
pub async fn silicons(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    query: Result<Query<SiliconQuery>, rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::Validation);
    }
    let mut silicons = actor
        .visible
        .iter()
        .filter(|owner| owner.account.kind == Some(ActorKind::Silicon))
        .filter(|owner| {
            query
                .after
                .as_deref()
                .is_none_or(|after| owner.account.uuid.as_str() > after)
        })
        .collect::<Vec<_>>();
    silicons.sort_by(|left, right| left.account.uuid.cmp(&right.account.uuid));
    let has_more = silicons.len() > limit;
    silicons.truncate(limit);
    let mut keys_by_uuid: std::collections::HashMap<&str, Vec<uuid::Uuid>> =
        std::collections::HashMap::new();
    if let crate::domain::ReadScope::Owners(owners) = &actor.read {
        for (key, owner) in owners {
            keys_by_uuid
                .entry(owner.account.uuid.as_str())
                .or_default()
                .push(*key);
        }
    }
    let mut all_keys = Vec::new();
    for silicon in &silicons {
        match keys_by_uuid.get(silicon.account.uuid.as_str()) {
            Some(keys) => all_keys.extend(keys.iter().copied()),
            None => all_keys.extend(state.identity.keys_of(&silicon.account.uuid).await?),
        }
    }
    let counts = state.repository.count_schedules_by_owner(&all_keys).await?;
    let mut items = Vec::with_capacity(silicons.len());
    for silicon in &silicons {
        let keys = match keys_by_uuid.get(silicon.account.uuid.as_str()) {
            Some(keys) => keys.clone(),
            None => state.identity.keys_of(&silicon.account.uuid).await?,
        };
        items.push(models::VisibleSiliconResponse {
            uuid: silicon.account.uuid.clone(),
            silicon_id: silicon.account.id.clone(),
            display_name: silicon.display_name.clone(),
            pfp_url: silicon.pfp_url.clone(),
            relation: silicon.relation,
            reminder_count: keys.iter().filter_map(|key| counts.get(key)).sum(),
        });
    }
    let next_cursor = has_more
        .then(|| silicons.last().map(|silicon| silicon.account.uuid.clone()))
        .flatten();
    Ok(Json(
        serde_json::json!({"items": items, "next_cursor": next_cursor}),
    ))
}

pub(crate) fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
