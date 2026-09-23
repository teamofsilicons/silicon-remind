//! Syntax validation for IAM public identifiers. Organization membership is an IAM fact.

/// Minimum byte length of an IAM handle.
pub const IAM_LABEL_MIN_BYTES: usize = 3;
/// Maximum byte length of an IAM handle.
pub const IAM_LABEL_MAX_BYTES: usize = 50;

/// Validates an organization ID or unprefixed IAM handle.
#[must_use]
pub fn is_valid_iam_label(value: &str) -> bool {
    (IAM_LABEL_MIN_BYTES..=IAM_LABEL_MAX_BYTES).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// Validates a canonical public Silicon ID; the prefix carries no organization.
#[must_use]
pub fn is_valid_global_silicon_id(value: &str) -> bool {
    value.strip_prefix("si:").is_some_and(is_valid_iam_label)
}

/// Validates a canonical public Carbon ID.
#[must_use]
pub fn is_valid_carbon_id(value: &str) -> bool {
    value
        .strip_prefix("c:")
        .is_some_and(|handle| handle.len() <= 30 && is_valid_iam_label(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_iam_bounds_and_alphabet() {
        assert!(is_valid_iam_label("tos"));
        assert!(is_valid_iam_label(&"o".repeat(50)));
        for invalid in ["ab", &"o".repeat(51), "Upper", "with.dot", "with:colon"] {
            assert!(!is_valid_iam_label(invalid));
        }
    }

    #[test]
    fn public_ids_require_explicit_kind_and_no_organization_suffix() {
        assert!(is_valid_global_silicon_id("si:assistant"));
        assert!(is_valid_carbon_id("c:person"));
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
}
