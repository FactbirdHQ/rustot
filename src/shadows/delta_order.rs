//! Ordering of shadow delta deliveries by document version.
//!
//! Delta subscriptions are at-least-once, so the same delta may legitimately
//! arrive more than once. On Greengrass it routinely does: the nucleus shares
//! one MQTT connection between all components, the broker sends one copy of a
//! message per matching subscription, and a wildcard such as
//! `shadow/name/+/update/delta` (the multi-shadow manager) overlaps every
//! named shadow's own delta subscription. Applying each copy re-runs the
//! consumer's side effects, so a delta is identified by the shadow document
//! `version` it carries.

/// What to do with a delta, given the newest version already applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOrder {
    /// Newer than anything applied (or unversioned): apply it.
    Apply,
    /// The version already applied: another copy of the same delta.
    Duplicate,
    /// Older than what was applied: a late copy, or the shadow was deleted and
    /// recreated (versions restart at 1). Only a GET tells which, so resync.
    Stale,
}

pub const fn delta_order(applied: Option<i64>, version: Option<i64>) -> DeltaOrder {
    match (applied, version) {
        (Some(applied), Some(version)) if version == applied => DeltaOrder::Duplicate,
        (Some(applied), Some(version)) if version < applied => DeltaOrder::Stale,
        _ => DeltaOrder::Apply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_and_newer_versions_apply() {
        assert_eq!(delta_order(None, Some(10)), DeltaOrder::Apply);
        assert_eq!(delta_order(Some(10), Some(11)), DeltaOrder::Apply);
    }

    #[test]
    fn same_version_is_a_duplicate() {
        assert_eq!(delta_order(Some(10), Some(10)), DeltaOrder::Duplicate);
    }

    #[test]
    fn older_version_needs_a_resync() {
        assert_eq!(delta_order(Some(10), Some(9)), DeltaOrder::Stale);
        assert_eq!(delta_order(Some(10), Some(1)), DeltaOrder::Stale);
    }

    #[test]
    fn unversioned_deltas_always_apply() {
        assert_eq!(delta_order(Some(10), None), DeltaOrder::Apply);
        assert_eq!(delta_order(None, None), DeltaOrder::Apply);
    }
}
