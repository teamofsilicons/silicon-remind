//! Strict HTTP request and response data-transfer objects.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

pub use crate::infrastructure::postgres::ScheduleResponse;
use crate::{
    domain::{
        AccountRef, CreateScheduleCommand, ExecutionStatus, PatchScheduleCommand, PatchValue,
        Relation, ScheduleKind, ScheduleSection, ScheduleStatus,
    },
    error::AppError,
    infrastructure::postgres::ExecutionRow,
};

/// Public schedule creation body.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateScheduleRequest {
    /// Reminder content.
    pub text: String,
    /// One-time or recurring materialization behavior.
    pub kind: ScheduleKind,
    /// Required IANA timezone identifier, checked when building the command.
    pub timezone: Option<String>,
    /// Five-field Linux cron expression.
    pub cron: String,
}

impl TryFrom<CreateScheduleRequest> for CreateScheduleCommand {
    type Error = AppError;

    fn try_from(request: CreateScheduleRequest) -> Result<Self, Self::Error> {
        let timezone = request
            .timezone
            .filter(|timezone| !timezone.trim().is_empty())
            .ok_or(AppError::TimezoneRequired)?;
        Ok(Self {
            text: request.text,
            timezone,
            kind: request.kind,
            cron: request.cron,
        })
    }
}

/// Public merge-patch body preserving omitted versus explicit-null fields.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchScheduleRequest {
    /// Replacement text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Replacement timezone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Replacement materialization behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ScheduleKind>,
    /// Cron value, invalid clear, or omission.
    #[serde(default, skip_serializing_if = "NullablePatch::is_unchanged")]
    pub cron: NullablePatch<String>,
    /// Active or paused. `completed` is parsed then rejected by domain policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
}

impl From<PatchScheduleRequest> for PatchScheduleCommand {
    fn from(request: PatchScheduleRequest) -> Self {
        Self {
            text: request.text,
            timezone: request.timezone,
            kind: request.kind,
            cron: request.cron.into_domain(),
            status: request.status,
        }
    }
}

/// Atomic desired-status change for one or more owned reminders.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BulkScheduleStatusRequest {
    /// Reminder identifiers in the order results must be returned.
    pub schedule_ids: Vec<Uuid>,
    /// Desired status. Application policy rejects the terminal status.
    pub status: ScheduleStatus,
}

/// A JSON merge-patch field with omitted, null, and value states.
#[derive(Clone, Debug, Default)]
pub enum NullablePatch<T> {
    /// Property was not present.
    #[default]
    Unchanged,
    /// Property was explicitly `null`.
    Clear,
    /// Property contained a value.
    Set(T),
}

impl<T> NullablePatch<T> {
    fn into_domain(self) -> PatchValue<T> {
        match self {
            Self::Unchanged => PatchValue::Unchanged,
            Self::Clear => PatchValue::Clear,
            Self::Set(value) => PatchValue::Set(value),
        }
    }

    const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl<'de, T> Deserialize<'de> for NullablePatch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl<T> Serialize for NullablePatch<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => serializer.serialize_some(value),
        }
    }
}

/// Query string for schedule visibility and keyset pagination.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListSchedulesQuery {
    /// Optional owner Silicon filter.
    pub silicon_id: Option<String>,
    /// Current reminders by default, or reminders retained in the archive.
    #[serde(default)]
    pub section: ScheduleSection,
    /// Optional lifecycle filter.
    pub status: Option<ScheduleStatus>,
    /// Opaque schedule cursor.
    pub cursor: Option<String>,
    /// Page size from 1 through 100.
    pub limit: Option<u32>,
}

/// Query string shared by execution-history pagination.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    /// Opaque execution cursor.
    pub cursor: Option<String>,
    /// Page size from 1 through 100.
    pub limit: Option<u32>,
}

/// Public execution-history representation.
#[derive(Clone, Debug, Serialize)]
pub struct ExecutionResponse {
    /// Stable logical occurrence UUID.
    pub id: Uuid,
    /// Parent schedule UUID.
    pub schedule_id: Uuid,
    /// Intended occurrence instant.
    pub scheduled_for: DateTime<Utc>,
    /// Most recent attempt time.
    pub attempted_at: Option<DateTime<Utc>>,
    /// webhook durable-acceptance time.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Delivery lifecycle status.
    pub status: ExecutionStatus,
    /// webhook event UUID after acceptance.
    pub hook_event_id: Option<Uuid>,
    /// Sanitized terminal/transient failure reason.
    pub failure_reason: Option<String>,
}

impl TryFrom<&ExecutionRow> for ExecutionResponse {
    type Error = anyhow::Error;

    fn try_from(row: &ExecutionRow) -> Result<Self, Self::Error> {
        let status = match row.status.as_str() {
            "pending" => ExecutionStatus::Pending,
            "delivered" => ExecutionStatus::Delivered,
            "retrying" => ExecutionStatus::Retrying,
            "failed" => ExecutionStatus::Failed,
            status => anyhow::bail!("database returned unknown execution status `{status}`"),
        };
        Ok(Self {
            id: row.id,
            schedule_id: row.schedule_id,
            scheduled_for: row.scheduled_for,
            attempted_at: row.attempted_at,
            delivered_at: row.delivered_at,
            status,
            hook_event_id: row.hook_event_id,
            failure_reason: row.failure_reason.clone(),
        })
    }
}

/// Generic cursor-paginated response envelope.
#[derive(Clone, Debug, Serialize)]
pub struct PageResponse<T> {
    /// Records in deterministic keyset order.
    pub items: Vec<T>,
    /// Cursor for the next page, or null at the end.
    pub next_cursor: Option<String>,
}

/// Non-secret subscription registration response.
#[derive(Clone, Debug, Serialize)]
pub struct HookDestinationResponse {
    /// Subscription registry UUID.
    pub id: Uuid,
    /// The owner Silicon's current public id.
    pub silicon_id: String,
    /// The owner Silicon's Silicon Accounts uuid.
    pub silicon_uuid: String,
    /// Registry version.
    pub version: i64,
    /// Last change time.
    pub updated_at: DateTime<Utc>,
}

/// Public metadata for one active webhook subscription.
#[derive(Clone, Debug, Serialize)]
pub struct WebhookSubscriptionResponse {
    /// Subscription registry UUID.
    pub id: Uuid,
    /// The owner Silicon's current public id.
    pub silicon_id: String,
    /// The owner Silicon's account, when Remind knows it.
    pub owner: Option<AccountRef>,
    /// Configured endpoint URL.
    pub endpoint_url: String,
    /// Monotonic subscription version.
    pub version: i64,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// One Silicon visible to the caller (`GET /silicons`).
#[derive(Clone, Debug, Serialize)]
pub struct VisibleSiliconResponse {
    /// Silicon Accounts uuid.
    pub uuid: String,
    /// Current public id.
    pub silicon_id: String,
    /// Display name.
    pub display_name: String,
    /// Profile photo URL.
    pub pfp_url: String,
    /// Why the caller sees it: `self`, `custodian` (the caller looks after it),
    /// `sibling` (same custodian) or `shared` (it granted the caller view).
    pub relation: Relation,
    /// Retained reminders (current and archived).
    pub reminder_count: i64,
}

/// Body that names an account to share with or to allow.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountTargetRequest {
    /// `c:handle`, `si:handle`, or a Silicon Accounts uuid.
    pub id: String,
    /// Which of the caller's Silicons this is about (required for Carbons).
    #[serde(default)]
    pub silicon_id: Option<String>,
}

/// Query naming which of the caller's Silicons a sharing request is about.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiliconSelector {
    /// `si:handle` or uuid of a Silicon the caller looks after (Carbons).
    pub silicon_id: Option<String>,
}

/// Acknowledgement of one Silicon Accounts webhook delivery.
#[derive(Clone, Debug, Serialize)]
pub struct AccountsEventAccepted {
    /// The delivery's event id.
    pub event_id: String,
    /// `processed`, `duplicate` or `ignored`.
    pub status: &'static str,
}

#[cfg(test)]
mod tests {
    use super::{
        AccountTargetRequest, BulkScheduleStatusRequest, CreateScheduleRequest, ListSchedulesQuery,
        NullablePatch, PatchScheduleRequest,
    };
    use crate::{
        domain::{CreateScheduleCommand, PatchScheduleCommand, ScheduleSection},
        error::AppError,
    };

    #[test]
    fn patch_distinguishes_omission_null_and_value() -> anyhow::Result<()> {
        let omitted: PatchScheduleRequest = serde_json::from_str(r#"{"text":"new"}"#)?;
        let cleared: PatchScheduleRequest = serde_json::from_str(r#"{"cron":null}"#)?;
        let set: PatchScheduleRequest = serde_json::from_str(r#"{"cron":"0 9 * * *"}"#)?;

        assert!(matches!(omitted.cron, NullablePatch::Unchanged));
        assert!(matches!(cleared.cron, NullablePatch::Clear));
        assert!(matches!(set.cron, NullablePatch::Set(_)));
        Ok(())
    }

    #[test]
    fn request_rejects_unknown_properties() {
        let parsed = serde_json::from_str::<PatchScheduleRequest>(r#"{"unknown":true}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn bulk_status_request_preserves_order_and_rejects_unknown_properties() -> anyhow::Result<()> {
        let first = uuid::Uuid::parse_str("0199a759-1ef5-7aa2-b96f-10785a8f2341")?;
        let second = uuid::Uuid::parse_str("0199a759-1ef5-7aa2-b96f-10785a8f2342")?;
        let request: BulkScheduleStatusRequest = serde_json::from_value(serde_json::json!({
            "schedule_ids": [first, second],
            "status": "paused"
        }))?;

        assert_eq!(request.schedule_ids, vec![first, second]);
        assert_eq!(request.status, crate::domain::ScheduleStatus::Paused);
        assert!(
            serde_json::from_value::<BulkScheduleStatusRequest>(serde_json::json!({
                "schedule_ids": [first],
                "status": "active",
                "unknown": true
            }))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn create_requires_an_explicit_nonempty_timezone() -> anyhow::Result<()> {
        for timezone in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("")),
            Some(serde_json::json!(" \t\n")),
        ] {
            let mut body = serde_json::json!({
                "text": "report",
                "kind": "one_time",
                "cron": "0 9 * * *"
            });
            if let Some(timezone) = timezone {
                body["timezone"] = timezone;
            }
            let request = serde_json::from_value::<CreateScheduleRequest>(body)?;
            assert!(matches!(
                CreateScheduleCommand::try_from(request),
                Err(AppError::TimezoneRequired)
            ));
        }
        Ok(())
    }

    #[test]
    fn create_preserves_an_explicit_timezone() -> anyhow::Result<()> {
        for timezone in ["UTC", "Asia/Kolkata"] {
            let request = serde_json::from_value::<CreateScheduleRequest>(serde_json::json!({
                "text": "report",
                "kind": "one_time",
                "cron": "0 9 * * *",
                "timezone": timezone
            }))?;
            assert_eq!(CreateScheduleCommand::try_from(request)?.timezone, timezone);
        }
        Ok(())
    }

    #[test]
    fn patch_can_omit_timezone() -> anyhow::Result<()> {
        let request: PatchScheduleRequest = serde_json::from_str(r#"{"text":"new"}"#)?;
        let command = PatchScheduleCommand::from(request);
        assert!(command.timezone.is_none());
        Ok(())
    }

    #[test]
    fn schedule_listing_defaults_to_current_and_accepts_archive() -> anyhow::Result<()> {
        let current = serde_json::from_str::<ListSchedulesQuery>("{}")?;
        let archived = serde_json::from_str::<ListSchedulesQuery>(r#"{"section":"archived"}"#)?;

        assert_eq!(current.section, ScheduleSection::Current);
        assert_eq!(archived.section, ScheduleSection::Archived);
        Ok(())
    }

    #[test]
    fn create_rejects_the_removed_run_at_property() {
        let parsed = serde_json::from_str::<CreateScheduleRequest>(
            r#"{"text":"report","kind":"one_time","cron":"0 9 * * *","run_at":"2026-09-01T09:00:00Z"}"#,
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn account_target_names_an_account_and_optionally_a_silicon() -> anyhow::Result<()> {
        let request: AccountTargetRequest =
            serde_json::from_str(r#"{"id":"c:ada","silicon_id":"si:scout"}"#)?;
        assert_eq!(request.id, "c:ada");
        assert_eq!(request.silicon_id.as_deref(), Some("si:scout"));
        assert!(
            serde_json::from_str::<AccountTargetRequest>(r#"{"id":"c:ada","org":"tos"}"#).is_err()
        );
        Ok(())
    }
}
