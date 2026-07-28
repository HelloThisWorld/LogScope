//! Attribute assembly helpers.

use logscope_model::{AnyValue, AttrMap, QualityFlag};

/// Builds an attribute map from key/value pairs, applying the documented
/// duplicate-key policy (last value wins) and flagging each duplicate.
pub fn attrs_from_pairs(
    pairs: impl IntoIterator<Item = (String, AnyValue)>,
) -> (AttrMap, Vec<QualityFlag>) {
    let mut map = AttrMap::new();
    let mut flags = Vec::new();
    for (key, value) in pairs {
        if map.insert(key.clone(), value).is_some() {
            flags.push(QualityFlag::DuplicateAttributeKey { key });
        }
    }
    (map, flags)
}

/// Converts a plain JSON object into typed attributes (top-level keys become
/// attribute keys; nested values stay typed and nested).
pub fn attrs_from_json_object(obj: &serde_json::Map<String, serde_json::Value>) -> AttrMap {
    obj.iter()
        .map(|(k, v)| (k.clone(), AnyValue::from_plain_json(v)))
        .collect()
}

/// Derives the single-line display message from a typed body.
pub fn derive_display_message(body: Option<&AnyValue>) -> String {
    match body {
        None => String::new(),
        Some(v) => {
            let s = v.display_string();
            // Display messages are single-line; the full body stays intact.
            if s.contains('\n') {
                s.lines().next().unwrap_or_default().to_string()
            } else {
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_flagged_last_wins() {
        let (map, flags) = attrs_from_pairs(vec![
            ("k".to_string(), AnyValue::int(1)),
            ("other".to_string(), AnyValue::int(9)),
            ("k".to_string(), AnyValue::int(2)),
        ]);
        assert_eq!(map.get("k"), Some(&AnyValue::int(2)));
        assert_eq!(
            flags,
            vec![QualityFlag::DuplicateAttributeKey { key: "k".into() }]
        );
    }

    #[test]
    fn multiline_display_message_takes_first_line() {
        let body = AnyValue::str("first line\n  at com.example.Boom");
        assert_eq!(derive_display_message(Some(&body)), "first line");
    }
}
