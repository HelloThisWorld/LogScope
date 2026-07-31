//! Opaque, stable entity ids.
//!
//! Ids are random (UUID v4) with a short type prefix — never derived from
//! titles, paths, database row numbers, display order, or content. They
//! survive rename, reorder, export, and import.

pub const PREFIX_INVESTIGATION: &str = "inv";
pub const PREFIX_SCOPE_REF: &str = "iscope";
pub const PREFIX_HYPOTHESIS: &str = "hyp";
pub const PREFIX_ITEM: &str = "item";
pub const PREFIX_EVIDENCE: &str = "ev";
pub const PREFIX_EVIDENCE_GROUP: &str = "evg";
pub const PREFIX_MARKER: &str = "mark";
pub const PREFIX_REPORT_DEF: &str = "rep";
pub const PREFIX_REPORT_ARTIFACT: &str = "art";
pub const PREFIX_REDACTION_PROFILE: &str = "red";
pub const PREFIX_BUNDLE_EXPORT: &str = "bnd";
pub const PREFIX_BUNDLE_IMPORT: &str = "bimp";

/// Mints a new opaque id: `<prefix>-<uuid-v4>`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = new_id(PREFIX_INVESTIGATION);
        let b = new_id(PREFIX_INVESTIGATION);
        assert!(a.starts_with("inv-"));
        assert_ne!(a, b);
    }
}
