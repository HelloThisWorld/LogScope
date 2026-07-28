//! Resource descriptors.
//!
//! A resource is identified purely by its typed attributes plus schema URL.
//! Derived convenience fields (`service.name` and friends) are computed from
//! well-known keys; the exact original key used for each derivation is
//! recorded so nothing about the derivation is opaque.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hashing::{stable_id, Digest};
use crate::value::{digest_attrs, AnyValue, AttrMap};

/// Derived, denormalized resource identity fields. Everything here is
/// recomputable from `attributes`; `original_keys` maps each derived field
/// name to the source attribute key that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DerivedResource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k8s_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k8s_pod_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k8s_pod_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_executable: Option<String>,
    /// derived field name -> original attribute key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub original_keys: BTreeMap<String, String>,
}

/// Canonical resource descriptor with a stable content-derived ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    /// `res-<32 hex>`: BLAKE3 over attributes + schema URL. Independent of
    /// derivation logic so the ID stays stable across derivation upgrades.
    pub resource_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_url: Option<String>,
    pub attributes: AttrMap,
    pub derived: DerivedResource,
    /// Dropped-attribute count as reported by the source (OTLP), if any.
    #[serde(default)]
    pub dropped_attributes_count: u32,
}

fn first_str<'a>(
    attrs: &'a AttrMap,
    keys: &[&str],
    field: &str,
    original: &mut BTreeMap<String, String>,
) -> Option<&'a str> {
    for key in keys {
        if let Some(AnyValue::Str(s)) = attrs.get(*key) {
            original.insert(field.to_string(), (*key).to_string());
            return Some(s);
        }
    }
    None
}

fn first_int(
    attrs: &AttrMap,
    keys: &[&str],
    field: &str,
    original: &mut BTreeMap<String, String>,
) -> Option<i64> {
    for key in keys {
        match attrs.get(*key) {
            Some(AnyValue::Int(i)) => {
                original.insert(field.to_string(), (*key).to_string());
                return Some(*i);
            }
            Some(AnyValue::Str(s)) => {
                if let Ok(i) = s.parse::<i64>() {
                    original.insert(field.to_string(), (*key).to_string());
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

impl ResourceDescriptor {
    /// Builds a descriptor from complete typed attributes, deriving the
    /// well-known identity fields and computing the stable ID.
    pub fn from_attributes(
        attributes: AttrMap,
        schema_url: Option<String>,
        dropped_attributes_count: u32,
    ) -> Self {
        let mut original = BTreeMap::new();
        let derived = DerivedResource {
            service_name: first_str(
                &attributes,
                &["service.name"],
                "service_name",
                &mut original,
            )
            .map(str::to_string),
            service_namespace: first_str(
                &attributes,
                &["service.namespace"],
                "service_namespace",
                &mut original,
            )
            .map(str::to_string),
            service_instance_id: first_str(
                &attributes,
                &["service.instance.id"],
                "service_instance_id",
                &mut original,
            )
            .map(str::to_string),
            deployment_environment: first_str(
                &attributes,
                &["deployment.environment.name", "deployment.environment"],
                "deployment_environment",
                &mut original,
            )
            .map(str::to_string),
            host_name: first_str(&attributes, &["host.name"], "host_name", &mut original)
                .map(str::to_string),
            host_id: first_str(&attributes, &["host.id"], "host_id", &mut original)
                .map(str::to_string),
            container_id: first_str(
                &attributes,
                &["container.id"],
                "container_id",
                &mut original,
            )
            .map(str::to_string),
            container_name: first_str(
                &attributes,
                &["container.name"],
                "container_name",
                &mut original,
            )
            .map(str::to_string),
            k8s_namespace: first_str(
                &attributes,
                &["k8s.namespace.name"],
                "k8s_namespace",
                &mut original,
            )
            .map(str::to_string),
            k8s_pod_name: first_str(
                &attributes,
                &["k8s.pod.name"],
                "k8s_pod_name",
                &mut original,
            )
            .map(str::to_string),
            k8s_pod_uid: first_str(&attributes, &["k8s.pod.uid"], "k8s_pod_uid", &mut original)
                .map(str::to_string),
            process_pid: first_int(&attributes, &["process.pid"], "process_pid", &mut original),
            process_executable: first_str(
                &attributes,
                &["process.executable.name"],
                "process_executable",
                &mut original,
            )
            .map(str::to_string),
            original_keys: original,
        };

        let resource_id = stable_id("res", |d| {
            Self::digest_identity(&attributes, schema_url.as_deref(), d);
        });

        ResourceDescriptor {
            resource_id,
            schema_url,
            attributes,
            derived,
            dropped_attributes_count,
        }
    }

    fn digest_identity(attributes: &AttrMap, schema_url: Option<&str>, d: &mut Digest) {
        d.str("resource.v1");
        d.opt_str(schema_url);
        digest_attrs(attributes, d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, AnyValue)]) -> AttrMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn id_depends_only_on_attributes_and_schema_url() {
        let a = ResourceDescriptor::from_attributes(
            attrs(&[
                ("service.name", AnyValue::str("checkout")),
                ("host.name", AnyValue::str("node-1")),
            ]),
            Some("https://opentelemetry.io/schemas/1.30.0".into()),
            0,
        );
        let b = ResourceDescriptor::from_attributes(
            attrs(&[
                ("host.name", AnyValue::str("node-1")),
                ("service.name", AnyValue::str("checkout")),
            ]),
            Some("https://opentelemetry.io/schemas/1.30.0".into()),
            0,
        );
        assert_eq!(a.resource_id, b.resource_id);

        let c = ResourceDescriptor::from_attributes(
            attrs(&[("service.name", AnyValue::str("checkout"))]),
            None,
            0,
        );
        assert_ne!(a.resource_id, c.resource_id);
    }

    #[test]
    fn derivation_records_original_keys() {
        let r = ResourceDescriptor::from_attributes(
            attrs(&[
                ("service.name", AnyValue::str("checkout")),
                ("deployment.environment", AnyValue::str("staging")),
                ("process.pid", AnyValue::int(4242)),
            ]),
            None,
            0,
        );
        assert_eq!(r.derived.service_name.as_deref(), Some("checkout"));
        assert_eq!(r.derived.deployment_environment.as_deref(), Some("staging"));
        assert_eq!(r.derived.process_pid, Some(4242));
        assert_eq!(
            r.derived.original_keys.get("deployment_environment"),
            Some(&"deployment.environment".to_string())
        );
        assert_eq!(
            r.derived.original_keys.get("service_name"),
            Some(&"service.name".to_string())
        );
    }

    #[test]
    fn newer_deployment_environment_key_wins() {
        let r = ResourceDescriptor::from_attributes(
            attrs(&[
                ("deployment.environment.name", AnyValue::str("prod")),
                ("deployment.environment", AnyValue::str("legacy")),
            ]),
            None,
            0,
        );
        assert_eq!(r.derived.deployment_environment.as_deref(), Some("prod"));
        assert_eq!(
            r.derived.original_keys.get("deployment_environment"),
            Some(&"deployment.environment.name".to_string())
        );
    }
}
