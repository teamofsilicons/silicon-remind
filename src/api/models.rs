//! Strict HTTP request and response data-transfer objects.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use url::Url;
use uuid::Uuid;

use crate::{
    domain::{
        CreateScheduleCommand, ExecutionStatus, PatchScheduleCommand, PatchValue, ScheduleKind,
        ScheduleStatus,
    },
    infrastructure::postgres::{ExecutionRow, ScheduleRow},
};

/// Public schedule creation body.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateScheduleRequest {
    /// Reminder content.
    pub text: String,
    /// One-time or recurring materialization behavior.
    pub kind: ScheduleKind,
    /// IANA timezone identifier; omitted values use UTC.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Five-field Linux cron expression.
    pub cron: String,
}

fn default_timezone() -> String {
    crate::domain::DEFAULT_TIMEZONE.to_owned()
}

impl From<CreateScheduleRequest> for CreateScheduleCommand {
    fn from(request: CreateScheduleRequest) -> Self {
        Self {
            text: request.text,
            timezone: request.timezone,
            kind: request.kind,
            cron: request.cron,
        }
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

/// Public schedule representation matching `openapi.yaml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduleResponse {
    /// Schedule UUID.
    pub id: Uuid,
    /// Organization scope.
    pub org_id: String,
    /// Owner Silicon.
    pub silicon_id: String,
    /// Reminder content.
    pub text: String,
    /// IANA timezone identifier.
    pub timezone: String,
    /// One-time or recurring materialization behavior.
    pub kind: String,
    /// Five-field Linux cron expression.
    pub cron: String,
    /// Public lifecycle status.
    pub status: String,
    /// Next due instant or null.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

impl From<&ScheduleRow> for ScheduleResponse {
    fn from(row: &ScheduleRow) -> Self {
        Self {
            id: row.id,
            org_id: row.org_id.clone(),
            silicon_id: row.silicon_id.clone(),
            text: row.text.clone(),
            timezone: row.timezone.clone(),
            kind: row.schedule_kind.clone(),
            cron: row.cron.clone(),
            status: row.status.clone(),
            next_run_at: row.next_run_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
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
    /// Hook durable-acceptance time.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Delivery lifecycle status.
    pub status: ExecutionStatus,
    /// Hook event UUID after acceptance.
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

/// Service-authenticated Hook destination registration body.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookDestinationRequest {
    /// Organization scope supplied by the trusted provisioning caller.
    pub org_id: String,
    /// Stable IAM UUID of the destination Silicon principal.
    pub principal_id: Uuid,
    /// Public global destination Silicon identifier.
    pub silicon_id: String,
    /// Exact ingress URL issued by Silicon Hook.
    pub endpoint_url: Url,
    /// One-time Hook signing credential.
    pub signing_secret: secrecy::SecretString,
}

/// Non-secret destination registration response.
#[derive(Clone, Debug, Serialize)]
pub struct HookDestinationResponse {
    /// Organization scope.
    pub org_id: String,
    /// Destination Silicon.
    pub silicon_id: String,
    /// Registry version after upsert.
    pub version: i64,
    /// Last replacement time.
    pub updated_at: DateTime<Utc>,
}

/// HMAC-authenticated Silicon IAM application webhook envelope.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IamWebhookEvent {
    /// IAM webhook specification version.
    pub spec_version: String,
    /// Sender-stable idempotency identifier.
    pub event_id: Uuid,
    /// Published event schema.
    pub event_type: IamWebhookEventType,
    /// Event occurrence timestamp.
    pub occurred_at: DateTime<Utc>,
    /// Versioned aggregate which produced this event.
    pub aggregate: IamWebhookAggregate,
    /// Event-specific minimal projection.
    pub data: Map<String, Value>,
}

/// Validated, versioned Silicon IAM application event name.
///
/// IAM's event vocabulary is additive. Remind interprets the lifecycle events
/// it owns and durably records every other syntactically valid event as a no-op,
/// allowing a newer IAM producer to add projections without breaking delivery.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IamWebhookEventType(String);

impl IamWebhookEventType {
    /// Organization disablement may revoke the entire tenant.
    pub const ORGANIZATION_UPDATED: &'static str = "organization.updated.v1";
    /// Silicon membership removal may revoke one principal.
    pub const ORGANIZATION_MEMBERSHIP_REMOVED: &'static str = "organization.membership.removed.v1";

    /// Returns the exact validated event-type spelling used on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether this is the exact supplied event schema.
    #[must_use]
    pub fn is(&self, event_type: &str) -> bool {
        self.0 == event_type
    }

    fn parse(value: String) -> Option<Self> {
        is_versioned_iam_event_name(&value).then_some(Self(value))
    }
}

impl<'de> Deserialize<'de> for IamWebhookEventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).ok_or_else(|| {
            serde::de::Error::custom("event_type must be a versioned dotted IAM event name")
        })
    }
}

impl Serialize for IamWebhookEventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

fn is_versioned_iam_event_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    let mut segments = value.split('.').peekable();
    let mut semantic_segments = 0_usize;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            let Some(version) = segment.strip_prefix('v') else {
                return false;
            };
            return semantic_segments >= 2
                && !version.is_empty()
                && !version.starts_with('0')
                && version.bytes().all(|byte| byte.is_ascii_digit());
        }
        let mut bytes = segment.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return false;
        }
        semantic_segments += 1;
    }
    false
}

/// Aggregate metadata carried by each IAM webhook event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IamWebhookAggregate {
    /// IAM aggregate UUID.
    pub id: Uuid,
    /// Stable aggregate type.
    #[serde(rename = "type")]
    pub aggregate_type: String,
    /// Positive aggregate-local ordering version.
    pub version: i64,
}

/// Durable internal-event acceptance response.
#[derive(Clone, Debug, Serialize)]
pub struct InternalEventAccepted {
    /// Stored receipt identifier.
    pub receipt_id: Uuid,
    /// Stable accepted state.
    pub status: &'static str,
}

#[cfg(test)]
mod tests {
    use super::{
        CreateScheduleRequest, IamWebhookEvent, IamWebhookEventType, NullablePatch,
        PatchScheduleRequest,
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
    fn omitted_create_timezone_canonicalizes_to_utc() -> anyhow::Result<()> {
        let omitted = serde_json::from_str::<CreateScheduleRequest>(
            r#"{"text":"report","kind":"one_time","cron":"0 9 * * *"}"#,
        )?;
        let explicit = serde_json::from_str::<CreateScheduleRequest>(
            r#"{"text":"report","kind":"one_time","cron":"0 9 * * *","timezone":"UTC"}"#,
        )?;

        assert_eq!(omitted.timezone, "UTC");
        assert_eq!(
            serde_json::to_value(omitted)?,
            serde_json::to_value(explicit)?
        );
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
    fn iam_webhook_model_matches_the_published_envelope() -> anyhow::Result<()> {
        let event = serde_json::from_str::<IamWebhookEvent>(
            r#"{
                "spec_version":"1.0",
                "event_id":"0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
                "event_type":"organization.membership.removed.v1",
                "occurred_at":"2026-08-31T12:00:00Z",
                "aggregate":{
                    "id":"0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                    "type":"membership",
                    "version":7
                },
                "data":{
                    "org_id":"tos",
                    "principal_id":"0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
                    "principal_type":"silicon"
                }
            }"#,
        )?;

        assert_eq!(
            event.event_type.as_str(),
            IamWebhookEventType::ORGANIZATION_MEMBERSHIP_REMOVED
        );
        assert_eq!(event.aggregate.aggregate_type, "membership");
        Ok(())
    }

    #[test]
    fn iam_webhook_accepts_additive_versioned_events_and_rejects_invalid_names() {
        for event_type in [
            "organization.silicon.created.v1",
            "organization.silicon.updated.v1",
            "organization.tag_archived.v2",
        ] {
            let parsed = serde_json::from_value::<IamWebhookEventType>(serde_json::Value::String(
                event_type.to_owned(),
            ));
            assert!(parsed.is_ok(), "valid additive event {event_type} rejected");
        }
        for event_type in [
            "silicon.updated",
            "organization..updated.v1",
            "Organization.updated.v1",
            "organization.updated.v0",
            "organization.updated.v01",
            "organization.updated.v1.extra",
        ] {
            let parsed = serde_json::from_value::<IamWebhookEventType>(serde_json::Value::String(
                event_type.to_owned(),
            ));
            assert!(parsed.is_err(), "invalid event {event_type} accepted");
        }
    }
}
