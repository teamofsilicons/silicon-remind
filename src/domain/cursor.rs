//! Versioned, opaque keyset cursors for deterministic pagination.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const CURSOR_VERSION: u8 = 1;
const MAX_ENCODED_CURSOR_BYTES: usize = 1_024;

/// The ordered query for which a cursor was issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorKind {
    /// Schedule ordering by `(created_at DESC, id DESC)`.
    Schedules,
    /// Execution ordering by `(scheduled_for DESC, id DESC)`.
    Executions,
}

/// A decoded keyset cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageCursor {
    /// The last tuple returned by a schedule page.
    Schedules {
        /// Last schedule creation time.
        created_at: DateTime<Utc>,
        /// Last schedule UUID, used as the deterministic tie breaker.
        id: Uuid,
    },
    /// The last tuple returned by an execution page.
    Executions {
        /// Last execution's scheduled instant.
        scheduled_for: DateTime<Utc>,
        /// Last execution UUID, used as the deterministic tie breaker.
        id: Uuid,
    },
}

impl PageCursor {
    /// Returns the query kind embedded in this cursor.
    #[must_use]
    pub const fn kind(self) -> CursorKind {
        match self {
            Self::Schedules { .. } => CursorKind::Schedules,
            Self::Executions { .. } => CursorKind::Executions,
        }
    }

    /// Returns the timestamp component of the keyset tuple.
    #[must_use]
    pub const fn timestamp(self) -> DateTime<Utc> {
        match self {
            Self::Schedules { created_at, .. } => created_at,
            Self::Executions { scheduled_for, .. } => scheduled_for,
        }
    }

    /// Returns the UUID tie breaker of the keyset tuple.
    #[must_use]
    pub const fn id(self) -> Uuid {
        match self {
            Self::Schedules { id, .. } | Self::Executions { id, .. } => id,
        }
    }

    /// Encodes this cursor as canonical URL-safe base64 JSON without padding.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Serialization`] if the service-owned cursor
    /// envelope cannot be serialized.
    pub fn encode(self) -> Result<String, CursorError> {
        let envelope = CursorEnvelope {
            version: CURSOR_VERSION,
            kind: self.kind(),
            timestamp: self.timestamp(),
            id: self.id(),
        };
        encode_envelope(&envelope)
    }

    /// Decodes and strictly validates an opaque cursor for `expected_kind`.
    ///
    /// Alternate JSON formatting, unknown fields, base64 padding, and query-kind
    /// reuse are rejected instead of being silently normalized.
    ///
    /// # Errors
    ///
    /// Returns a [`CursorError`] when `encoded` is empty, exceeds the defensive
    /// size limit, is not canonical URL-safe base64 JSON, uses an unsupported
    /// version, or was issued for a query other than `expected_kind`.
    pub fn decode(encoded: &str, expected_kind: CursorKind) -> Result<Self, CursorError> {
        if encoded.is_empty() {
            return Err(CursorError::Empty);
        }
        if encoded.len() > MAX_ENCODED_CURSOR_BYTES {
            return Err(CursorError::TooLong {
                maximum: MAX_ENCODED_CURSOR_BYTES,
            });
        }

        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CursorError::InvalidEncoding)?;
        let envelope: CursorEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| CursorError::InvalidPayload)?;
        if envelope.version != CURSOR_VERSION {
            return Err(CursorError::UnsupportedVersion {
                version: envelope.version,
            });
        }
        if envelope.kind != expected_kind {
            return Err(CursorError::WrongKind {
                expected: expected_kind,
                actual: envelope.kind,
            });
        }
        if encode_envelope(&envelope)? != encoded {
            return Err(CursorError::NonCanonical);
        }

        Ok(match envelope.kind {
            CursorKind::Schedules => Self::Schedules {
                created_at: envelope.timestamp,
                id: envelope.id,
            },
            CursorKind::Executions => Self::Executions {
                scheduled_for: envelope.timestamp,
                id: envelope.id,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorEnvelope {
    #[serde(rename = "v")]
    version: u8,
    kind: CursorKind,
    #[serde(rename = "at")]
    timestamp: DateTime<Utc>,
    id: Uuid,
}

/// Cursor decoding and encoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CursorError {
    /// The client supplied an empty cursor value.
    #[error("cursor must not be empty")]
    Empty,
    /// The encoded cursor exceeded the defensive input bound.
    #[error("cursor exceeds the maximum encoded length of {maximum} bytes")]
    TooLong {
        /// Maximum accepted encoded length.
        maximum: usize,
    },
    /// The value is not URL-safe unpadded base64.
    #[error("cursor is not valid URL-safe base64")]
    InvalidEncoding,
    /// The decoded JSON does not match the versioned cursor schema.
    #[error("cursor payload is invalid")]
    InvalidPayload,
    /// The cursor was issued by an unsupported schema version.
    #[error("cursor version {version} is not supported")]
    UnsupportedVersion {
        /// Unsupported version received from the client.
        version: u8,
    },
    /// A cursor from one list operation was applied to another.
    #[error("cursor kind {actual:?} cannot be used for {expected:?}")]
    WrongKind {
        /// Kind required by the current query.
        expected: CursorKind,
        /// Kind embedded in the cursor.
        actual: CursorKind,
    },
    /// The cursor decodes but was not emitted in canonical form by this service.
    #[error("cursor encoding is not canonical")]
    NonCanonical,
    /// Serialization failed while emitting the service-owned envelope.
    #[error("cursor could not be encoded")]
    Serialization,
}

fn encode_envelope(envelope: &CursorEnvelope) -> Result<String, CursorError> {
    let bytes = serde_json::to_vec(envelope).map_err(|_| CursorError::Serialization)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    fn timestamp() -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        DateTime::parse_from_rfc3339("2026-08-31T09:00:00Z")
            .map(|value| value.with_timezone(&Utc))
            .map_err(Into::into)
    }

    #[test]
    fn schedule_cursor_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let cursor = PageCursor::Schedules {
            created_at: timestamp()?,
            id: Uuid::from_u128(1),
        };
        let encoded = cursor.encode()?;

        assert!(!encoded.contains('='));
        assert_eq!(PageCursor::decode(&encoded, CursorKind::Schedules)?, cursor);
        Ok(())
    }

    #[test]
    fn execution_cursor_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let cursor = PageCursor::Executions {
            scheduled_for: timestamp()?,
            id: Uuid::from_u128(2),
        };
        let encoded = cursor.encode()?;

        assert_eq!(
            PageCursor::decode(&encoded, CursorKind::Executions)?,
            cursor
        );
        Ok(())
    }

    #[test]
    fn cursor_kind_cannot_be_reused_across_queries() -> Result<(), Box<dyn std::error::Error>> {
        let encoded = PageCursor::Schedules {
            created_at: timestamp()?,
            id: Uuid::from_u128(1),
        }
        .encode()?;

        assert_eq!(
            PageCursor::decode(&encoded, CursorKind::Executions),
            Err(CursorError::WrongKind {
                expected: CursorKind::Executions,
                actual: CursorKind::Schedules,
            })
        );
        Ok(())
    }

    #[test]
    fn malformed_and_noncanonical_values_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            PageCursor::decode("", CursorKind::Schedules),
            Err(CursorError::Empty)
        );
        assert_eq!(
            PageCursor::decode("not+a+cursor", CursorKind::Schedules),
            Err(CursorError::InvalidEncoding)
        );

        let canonical = PageCursor::Schedules {
            created_at: timestamp()?,
            id: Uuid::from_u128(1),
        }
        .encode()?;
        let padded = format!("{canonical}=");
        assert!(matches!(
            PageCursor::decode(&padded, CursorKind::Schedules),
            Err(CursorError::InvalidEncoding | CursorError::NonCanonical)
        ));

        let noncanonical_json = format!(
            "{{ \"v\": 1, \"kind\": \"schedules\", \"at\": \"{}\", \"id\": \"{}\" }}",
            timestamp()?.to_rfc3339(),
            Uuid::from_u128(1)
        );
        let noncanonical = URL_SAFE_NO_PAD.encode(noncanonical_json);
        assert_eq!(
            PageCursor::decode(&noncanonical, CursorKind::Schedules),
            Err(CursorError::NonCanonical)
        );
        Ok(())
    }

    #[test]
    fn unknown_fields_and_versions_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let unknown = URL_SAFE_NO_PAD.encode(format!(
            "{{\"v\":1,\"kind\":\"schedules\",\"at\":\"{}\",\"id\":\"{}\",\"extra\":true}}",
            timestamp()?.to_rfc3339(),
            Uuid::from_u128(1)
        ));
        assert_eq!(
            PageCursor::decode(&unknown, CursorKind::Schedules),
            Err(CursorError::InvalidPayload)
        );

        let unsupported = CursorEnvelope {
            version: 2,
            kind: CursorKind::Schedules,
            timestamp: timestamp()?,
            id: Uuid::from_u128(1),
        };
        let unsupported = encode_envelope(&unsupported)?;
        assert_eq!(
            PageCursor::decode(&unsupported, CursorKind::Schedules),
            Err(CursorError::UnsupportedVersion { version: 2 })
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn arbitrary_keyset_tuples_round_trip(
            seconds in -2_208_988_800_i64..4_102_444_800_i64,
            uuid_bytes in any::<[u8; 16]>(),
            schedule_kind in any::<bool>(),
        ) {
            let Some(at) = DateTime::<Utc>::from_timestamp(seconds, 0) else {
                return Ok(());
            };
            let id = Uuid::from_bytes(uuid_bytes);
            let (cursor, kind) = if schedule_kind {
                (PageCursor::Schedules { created_at: at, id }, CursorKind::Schedules)
            } else {
                (PageCursor::Executions { scheduled_for: at, id }, CursorKind::Executions)
            };

            let encoded = cursor.encode();
            prop_assert!(encoded.is_ok());
            let Ok(encoded) = encoded else {
                return Ok(());
            };
            prop_assert!(!encoded.contains('='));
            prop_assert_eq!(PageCursor::decode(&encoded, kind), Ok(cursor));
        }
    }
}
