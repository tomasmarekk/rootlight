//! Checked source-language assignments for the critical source-languages extension.
//! Payloads remain bounded by the shared extension encoder and are redacted from Debug.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ConfigError;

#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceLanguages {
    #[serde(default)]
    extensions: BTreeMap<String, String>,
    #[serde(default)]
    paths: BTreeMap<String, String>,
}

impl std::fmt::Debug for SourceLanguages {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceLanguages")
            .field("extensions", &self.extensions.len())
            .field("paths", &self.paths.len())
            .finish()
    }
}

impl SourceLanguages {
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, ConfigError> {
        let rules: Self =
            serde_json::from_slice(bytes).map_err(|_| ConfigError::InvalidSourceLanguages)?;
        if rules.extensions.iter().any(|(extension, language)| {
            !label(extension)
                || extension
                    .bytes()
                    .any(|byte| matches!(byte, b'.' | b'+' | b'#'))
                || !label(language)
        }) || rules
            .paths
            .iter()
            .any(|(path, language)| !relative_path(path) || !label(language))
        {
            return Err(ConfigError::InvalidSourceLanguages);
        }
        Ok(rules)
    }

    pub(super) fn language_for_path(&self, path: &str) -> Option<&str> {
        if self.extensions.is_empty() && self.paths.is_empty() {
            return None;
        }
        if !relative_path(path) {
            return None;
        }
        if let Some(language) = self.paths.get(path) {
            return Some(language);
        }
        let filename = path.rsplit('/').next()?;
        let (stem, extension) = filename.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        self.extensions
            .get(&extension.to_ascii_lowercase())
            .map(String::as_str)
    }
}

fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'#')
        })
}

fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.chars().any(|character| {
            character.is_control() || matches!(character, '\\' | ':' | '*' | '?' | '[' | ']')
        })
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
