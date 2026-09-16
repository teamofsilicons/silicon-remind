//! Authenticated bug submission, isolated by the selected environment.
use crate::{api::ScopedState, domain::Actor, error::AppError};
use axum::{
    Json,
    extract::{Extension, Path, rejection},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BugReportRequest {
    message: String,
    pr: Option<String>,
}

#[derive(Serialize, FromRow)]
pub(crate) struct BugReportResponse {
    id: Uuid,
    status: String,
    failure_reason: Option<String>,
}

pub(crate) async fn create(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    body: Result<Json<BugReportRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<BugReportResponse>), AppError> {
    let Json(input) = body.map_err(|_| AppError::Validation)?;
    validate(&input)?;
    let key = super::schedules::idempotency_key(&headers)?;
    if !(8..=255).contains(&key.len()) {
        return Err(AppError::Validation);
    }
    if !state.is_test && !state.reports_enabled {
        return Err(AppError::DependencyUnavailable {
            dependency: "postmark_not_configured",
        });
    }
    let hash = crate::application::schedules::request_hash(&input)?;
    let hash = serde_json::to_string(&hash).map_err(|e| AppError::internal("report_hash", e))?;
    let pool = state.repository.pool();
    let mut tx = pool.begin().await.map_err(db)?;
    // Serialize submissions per actor, including the rate limit and idempotent replay.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 906))")
        .bind(format!("{}:{}", actor.org_id, actor.id))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let existing: Option<(Uuid,String,String,Option<String>)> = sqlx::query_as(
        "SELECT id,request_hash,status,failure_reason FROM bug_reports WHERE org_id=$1 AND actor_id=$2 AND idempotency_key=$3")
        .bind(&actor.org_id).bind(&actor.id).bind(&key).fetch_optional(&mut *tx).await.map_err(db)?;
    if let Some((id, old_hash, status, failure_reason)) = existing {
        if old_hash != hash {
            return Err(AppError::conflict("idempotency_key_reused"));
        }
        return Ok((
            StatusCode::OK,
            Json(BugReportResponse {
                id,
                status,
                failure_reason,
            }),
        ));
    }
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM bug_reports WHERE org_id=$1 AND actor_id=$2 AND created_at > now()-interval '1 hour'")
        .bind(&actor.org_id).bind(&actor.id).fetch_one(&mut *tx).await.map_err(db)?;
    if recent >= 10 {
        return Err(AppError::RateLimited {
            retry_after_seconds: 3600,
        });
    }
    let id = Uuid::now_v7();
    let status = if state.is_test { "simulated" } else { "queued" };
    sqlx::query("INSERT INTO bug_reports(id,org_id,actor_id,idempotency_key,request_hash,message,pr,status) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(id).bind(&actor.org_id).bind(&actor.id).bind(key).bind(hash).bind(input.message).bind(input.pr).bind(status)
        .execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(BugReportResponse {
            id,
            status: status.into(),
            failure_reason: None,
        }),
    ))
}

pub(crate) async fn get(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<BugReportResponse>, AppError> {
    sqlx::query_as("SELECT id,status,failure_reason FROM bug_reports WHERE id=$1 AND org_id=$2 AND actor_id=$3")
        .bind(id).bind(actor.org_id).bind(actor.id).fetch_optional(state.repository.pool()).await.map_err(db)?
        .map(Json).ok_or(AppError::NotFound)
}

fn validate(input: &BugReportRequest) -> Result<(), AppError> {
    if input.message.trim().is_empty() || input.message.len() > 16_384 {
        return Err(AppError::Validation);
    }
    if let Some(pr) = &input.pr {
        let number = pr.strip_prefix("https://github.com/teamofsilicons/silicon-remind/pull/");
        if !number.is_some_and(|n| {
            !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && n.parse::<u64>().is_ok_and(|n| n > 0)
        }) {
            return Err(AppError::Validation);
        }
    }
    Ok(())
}
fn db(error: sqlx::Error) -> AppError {
    AppError::internal("bug_report_database", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_empty_oversized_and_foreign_pull_requests() {
        for (message, pr) in [
            (String::new(), None),
            ("a".repeat(16385), None),
            ("bug".into(), Some("https://evil.example/pull/1".into())),
            (
                "bug".into(),
                Some("https://github.com/teamofsilicons/silicon-remind/pull/0".into()),
            ),
        ] {
            assert!(validate(&BugReportRequest { message, pr }).is_err());
        }
        assert!(
            validate(&BugReportRequest {
                message: "steps and expected result".into(),
                pr: Some("https://github.com/teamofsilicons/silicon-remind/pull/123".into())
            })
            .is_ok()
        );
    }
}
