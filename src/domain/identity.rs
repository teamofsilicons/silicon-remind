//! Syntax of Silicon Accounts identifiers.
//!
//! Accounts now issue canonical lowercase 128-bit UUIDs; legacy case-sensitive
//! base62 identifiers remain readable during the coordinated backfill. Its
//! public id is a prefix and a handle: `c:` for a Carbon, `si:` for a Silicon.
//! Silicon Accounts issues handles of 3 to 30 characters of `a-z`, `0-9`, `-`
//! and `_`; Remind also accepts the up-to-50-character handles that rows from
//! before the move to Silicon Accounts still carry.

/// Minimum byte length of a handle.
pub const HANDLE_MIN_BYTES: usize = 3;
/// Maximum byte length of a handle Remind stores.
pub const HANDLE_MAX_BYTES: usize = 50;

/// Validates an unprefixed handle.
#[must_use]
pub fn is_valid_handle(value: &str) -> bool {
    (HANDLE_MIN_BYTES..=HANDLE_MAX_BYTES).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// Validates a public Silicon id (`si:handle`).
#[must_use]
pub fn is_valid_global_silicon_id(value: &str) -> bool {
    value.strip_prefix("si:").is_some_and(is_valid_handle)
}

/// Validates a public Carbon id (`c:handle`).
#[must_use]
pub fn is_valid_carbon_id(value: &str) -> bool {
    value
        .strip_prefix("c:")
        .is_some_and(|handle| handle.len() <= 30 && is_valid_handle(handle))
}

/// Validates either kind of public id.
#[must_use]
pub fn is_valid_public_id(value: &str) -> bool {
    is_valid_global_silicon_id(value) || is_valid_carbon_id(value)
}

/// Validates a canonical UUID or a legacy case-sensitive account identifier.
#[must_use]
pub fn is_valid_account_uuid(value: &str) -> bool {
    ((1..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        || (value.len() == 36
            && uuid::Uuid::parse_str(value).is_ok_and(|id| {
                id.hyphenated().to_string() == value && id.get_variant() == uuid::Variant::RFC4122
            }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_match_the_stored_bounds_and_alphabet() {
        assert!(is_valid_handle("tos"));
        assert!(is_valid_handle(&"o".repeat(50)));
        for invalid in ["ab", &"o".repeat(51), "Upper", "with.dot", "with:colon"] {
            assert!(!is_valid_handle(invalid));
        }
    }

    #[test]
    fn public_ids_require_an_explicit_kind() {
        assert!(is_valid_global_silicon_id("si:assistant"));
        assert!(is_valid_carbon_id("c:person"));
        assert!(is_valid_public_id("c:person") && is_valid_public_id("si:assistant"));
        for invalid in [
            "assistant",
            "assistant:tos",
            "si:assistant:tos",
            "c:assistant",
            "si:",
        ] {
            assert!(!is_valid_global_silicon_id(invalid));
        }
        for invalid in ["person", "si:person", "c:person:tos", "c:"] {
            assert!(!is_valid_carbon_id(invalid));
        }
    }

    #[test]
    fn standard_uuids_and_legacy_identifiers_are_accepted() {
        for valid in [
            "zQo",
            "8HV",
            "a8K",
            "A",
            "550e8400-e29b-41d4-a716-446655440000",
        ] {
            assert!(is_valid_account_uuid(valid));
        }
        for invalid in ["", "z-Q", "zQo ", "c:ada", &"a".repeat(65)] {
            assert!(!is_valid_account_uuid(invalid));
        }
    }
}
