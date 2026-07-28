//! Instrumentation scope descriptors.

use serde::{Deserialize, Serialize};

use crate::hashing::stable_id;
use crate::value::{digest_attrs, AttrMap};

/// Canonical instrumentation scope with a stable content-derived ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeDescriptor {
    /// `scp-<32 hex>`: BLAKE3 over name + version + attributes + schema URL.
    pub scope_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_url: Option<String>,
    pub attributes: AttrMap,
    #[serde(default)]
    pub dropped_attributes_count: u32,
}

impl ScopeDescriptor {
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        schema_url: Option<String>,
        attributes: AttrMap,
        dropped_attributes_count: u32,
    ) -> Self {
        let scope_id = stable_id("scp", |d| {
            d.str("scope.v1");
            d.opt_str(name.as_deref());
            d.opt_str(version.as_deref());
            d.opt_str(schema_url.as_deref());
            digest_attrs(&attributes, d);
        });
        ScopeDescriptor {
            scope_id,
            name,
            version,
            schema_url,
            attributes,
            dropped_attributes_count,
        }
    }

    /// The scope used when a source has no scope concept (plain files).
    pub fn unknown() -> Self {
        Self::new(None, None, None, AttrMap::new(), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_id_is_stable_and_content_sensitive() {
        let a = ScopeDescriptor::new(
            Some("io.example.lib".into()),
            Some("1.2.3".into()),
            None,
            AttrMap::new(),
            0,
        );
        let b = ScopeDescriptor::new(
            Some("io.example.lib".into()),
            Some("1.2.3".into()),
            None,
            AttrMap::new(),
            0,
        );
        let c = ScopeDescriptor::new(
            Some("io.example.lib".into()),
            Some("1.2.4".into()),
            None,
            AttrMap::new(),
            0,
        );
        assert_eq!(a.scope_id, b.scope_id);
        assert_ne!(a.scope_id, c.scope_id);
        assert_ne!(a.scope_id, ScopeDescriptor::unknown().scope_id);
    }
}
