//! Canonical validation for IAM organization and Silicon public identifiers.

/// Minimum byte length of one IAM public-label segment.
pub const IAM_LABEL_MIN_BYTES: usize = 3;
/// Maximum byte length of one IAM public-label segment.
pub const IAM_LABEL_MAX_BYTES: usize = 50;

/// Returns whether a value satisfies IAM's public organization/local-ID label.
#[must_use]
pub fn is_valid_iam_label(value: &str) -> bool {
    (IAM_LABEL_MIN_BYTES..=IAM_LABEL_MAX_BYTES).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// Returns whether a global Silicon ID has two valid IAM label segments.
#[must_use]
pub fn is_valid_global_silicon_id(value: &str) -> bool {
    let Some((local_id, org_id)) = value.split_once(':') else {
        return false;
    };
    !org_id.contains(':') && is_valid_iam_label(local_id) && is_valid_iam_label(org_id)
}

/// Returns whether a global Silicon ID is valid and belongs to `org_id`.
#[must_use]
pub fn silicon_id_belongs_to_org(value: &str, org_id: &str) -> bool {
    is_valid_iam_label(org_id)
        && value
            .split_once(':')
            .is_some_and(|(local_id, suffix)| is_valid_iam_label(local_id) && suffix == org_id)
}

#[cfg(test)]
mod tests {
    use super::{is_valid_global_silicon_id, is_valid_iam_label, silicon_id_belongs_to_org};

    #[test]
    fn labels_match_iam_bounds_and_alphabet() {
        assert!(is_valid_iam_label("tos"));
        assert!(is_valid_iam_label(&"o".repeat(50)));
        for invalid in ["ab", &"o".repeat(51), "Upper", "with.dot", "with:colon"] {
            assert!(!is_valid_iam_label(invalid));
        }
    }

    #[test]
    fn global_id_requires_exactly_two_matching_valid_segments() {
        assert!(is_valid_global_silicon_id("assistant:tos"));
        assert!(silicon_id_belongs_to_org("assistant:tos", "tos"));
        for invalid in ["assistant", "assistant:other", "assistant:tos:extra"] {
            assert!(!silicon_id_belongs_to_org(invalid, "tos"));
        }
    }
}
