//! Dialect identity comes from a complete JSON document and its root `$schema`.
//! This certificate admits source analysis; it does not validate schema keywords
//! or turn property-like data in annotations into schema declarations.

use std::{collections::BTreeSet, fmt};

use serde::de::{self, IgnoredAny, MapAccess, Visitor};

use crate::MAX_CLASSIFICATION_BYTES;

struct RootDialect;

impl<'de> Visitor<'de> for RootDialect {
    type Value = bool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one JSON object with a unique supported root $schema")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        let mut supported = false;
        while let Some(name) = map.next_key::<String>()? {
            if !names.insert(name.clone()) {
                return Err(de::Error::custom("ambiguous root member"));
            }
            if name == "$schema" {
                supported =
                    map.next_value::<String>()? == "https://json-schema.org/draft/2020-12/schema";
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(supported)
    }
}

/// Certify an explicitly supported dialect in one complete bounded JSON object.
/// The discovery caller must first validate the entire physical text encoding.
pub(super) fn has_content_hint(content: &[u8], prefix_truncated: bool) -> bool {
    if prefix_truncated || content.len() > MAX_CLASSIFICATION_BYTES {
        return false;
    }
    let Ok(text) = std::str::from_utf8(content) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parser = serde_json::Deserializer::from_str(text);
    matches!(
        serde::Deserializer::deserialize_map(&mut parser, RootDialect),
        Ok(true)
    ) && parser.end().is_ok()
}
