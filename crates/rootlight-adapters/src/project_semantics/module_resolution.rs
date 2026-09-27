//! Source-bound TypeScript and JavaScript path-alias resolution.
//!
//! The service forwards discovered configuration snapshots in the project
//! manifest; this module validates their hashes and resolves only local paths
//! established by those snapshots.

use rootlight_ids::{ContentHash, content_hash};
use serde::Deserialize;
use serde_json::Value;

use super::{PROJECT_MODULE_RESOLUTION_MARKER, SemanticProjectLanguage};

/// Source-verified project-local module paths shared by analysis and partition discovery.
#[derive(Default)]
pub struct ModuleResolution {
    configs: Vec<Config>,
    language: Option<SemanticProjectLanguage>,
}

struct Config {
    directory: String,
    base_url: Option<String>,
    aliases: Vec<(Pattern, Vec<Pattern>)>,
    kind: ConfigKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigKind {
    TypeScript,
    JavaScript,
}

struct Pattern {
    prefix: String,
    suffix: String,
    wildcard: bool,
}

#[derive(Deserialize)]
struct Manifest {
    configs: Vec<ConfigSource>,
}

#[derive(Deserialize)]
struct ConfigSource {
    path: String,
    content_hash: ContentHash,
    content: String,
}

impl ModuleResolution {
    /// Reads only configuration snapshots whose content matches the manifest hash.
    #[must_use]
    pub fn from_manifest(manifest: &[u8], language: SemanticProjectLanguage) -> Self {
        if !matches!(
            language,
            SemanticProjectLanguage::TypeScript | SemanticProjectLanguage::JavaScript
        ) {
            return Self::default();
        }
        let Some(marker) = manifest
            .windows(PROJECT_MODULE_RESOLUTION_MARKER.len())
            .rposition(|window| window == PROJECT_MODULE_RESOLUTION_MARKER)
        else {
            return Self::default();
        };
        let encoded = &manifest[marker + PROJECT_MODULE_RESOLUTION_MARKER.len()..];
        let Ok(decoded) = serde_json::from_slice::<Manifest>(encoded) else {
            return Self::default();
        };
        let configs = decoded
            .configs
            .into_iter()
            .filter_map(Config::from_source)
            .collect();
        Self {
            configs,
            language: Some(language),
        }
    }

    /// Returns configured local paths in declaration priority, or `None` if not configured.
    #[must_use]
    pub fn candidates(&self, current_path: &str, module: &str) -> Option<Vec<String>> {
        if module.starts_with('.') || module.starts_with('/') {
            return None;
        }
        let config = self
            .configs
            .iter()
            .filter(|config| {
                self.language != Some(SemanticProjectLanguage::TypeScript)
                    || config.kind == ConfigKind::TypeScript
            })
            .filter(|config| {
                config.directory.is_empty()
                    || current_path
                        .strip_prefix(&config.directory)
                        .is_some_and(|remainder| remainder.starts_with('/'))
            })
            .max_by_key(|config| {
                (
                    config.directory.len(),
                    Some(config.kind)
                        == self.language.map(|language| match language {
                            SemanticProjectLanguage::JavaScript => ConfigKind::JavaScript,
                            _ => ConfigKind::TypeScript,
                        }),
                )
            })?;
        for (alias, targets) in &config.aliases {
            if let Some(capture) = alias.capture(module) {
                let base = config.base_url.as_deref().unwrap_or(&config.directory);
                return Some(
                    targets
                        .iter()
                        .filter_map(|target| normalize_path(base, &target.expand(capture)))
                        .collect(),
                );
            }
        }
        config
            .base_url
            .as_deref()
            .and_then(|base| normalize_path(base, module))
            .map(|path| vec![path])
    }
}

impl Config {
    fn from_source(source: ConfigSource) -> Option<Self> {
        if content_hash(source.content.as_bytes()) != source.content_hash {
            return None;
        }
        let (directory, filename) = source
            .path
            .rsplit_once('/')
            .map_or(("", source.path.as_str()), |(parent, file)| (parent, file));
        let kind = match filename {
            "tsconfig.json" => ConfigKind::TypeScript,
            "jsconfig.json" => ConfigKind::JavaScript,
            _ => return None,
        };
        if normalize_path("", directory).as_deref() != Some(directory) {
            return None;
        }
        let decoded = jsonc_value(source.content.as_bytes())?;
        let compiler_options = decoded.get("compilerOptions")?.as_object()?;
        let base_url = match compiler_options.get("baseUrl") {
            Some(value) => Some(normalize_path(directory, value.as_str()?)?),
            None => None,
        };
        let mut aliases = Vec::new();
        if let Some(paths) = compiler_options.get("paths") {
            let paths = paths.as_object()?;
            for (key, values) in paths {
                let Some(pattern) = Pattern::parse(key) else {
                    continue;
                };
                let Some(values) = values.as_array() else {
                    continue;
                };
                let targets = values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(Pattern::parse)
                    .collect::<Vec<_>>();
                if !targets.is_empty() {
                    aliases.push((pattern, targets));
                }
            }
        }
        aliases.sort_by(|(left, _), (right, _)| {
            right
                .specificity()
                .cmp(&left.specificity())
                .then_with(|| left.prefix.cmp(&right.prefix))
        });
        if base_url.is_none() && aliases.is_empty() {
            return None;
        }
        Some(Self {
            directory: directory.to_owned(),
            base_url,
            aliases,
            kind,
        })
    }
}

impl Pattern {
    fn parse(input: &str) -> Option<Self> {
        if input.is_empty() || input.starts_with('/') || input.contains(':') {
            return None;
        }
        let (prefix, suffix, wildcard) = match input.split_once('*') {
            Some((prefix, suffix)) if !suffix.contains('*') => (prefix, suffix, true),
            Some(_) => return None,
            None => (input, "", false),
        };
        Some(Self {
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
            wildcard,
        })
    }

    fn specificity(&self) -> (bool, usize, usize) {
        (!self.wildcard, self.prefix.len(), self.suffix.len())
    }

    fn capture<'a>(&self, input: &'a str) -> Option<&'a str> {
        if !self.wildcard {
            return (input == self.prefix).then_some("");
        }
        let middle = input
            .strip_prefix(&self.prefix)?
            .strip_suffix(&self.suffix)?;
        Some(middle)
    }

    fn expand(&self, capture: &str) -> String {
        if self.wildcard {
            format!("{}{}{}", self.prefix, capture, self.suffix)
        } else {
            self.prefix.clone()
        }
    }
}

fn normalize_path(base: &str, path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains(':') || path.contains('\\') {
        return None;
    }
    let mut components = base
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value if value.contains('\0') => return None,
            value => components.push(value),
        }
    }
    Some(components.join("/"))
}

fn jsonc_value(source: &[u8]) -> Option<Value> {
    let mut normalized = source.to_vec();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < normalized.len() {
        let current = normalized[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if current == b'\\' {
                escaped = true;
            } else if current == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if current == b'"' {
            in_string = true;
            index += 1;
        } else if current == b'/' && normalized.get(index + 1) == Some(&b'/') {
            normalized[index] = b' ';
            normalized[index + 1] = b' ';
            index += 2;
            while index < normalized.len() && normalized[index] != b'\n' {
                normalized[index] = b' ';
                index += 1;
            }
        } else if current == b'/' && normalized.get(index + 1) == Some(&b'*') {
            normalized[index] = b' ';
            normalized[index + 1] = b' ';
            index += 2;
            let mut closed = false;
            while index < normalized.len() {
                if normalized[index] == b'*' && normalized.get(index + 1) == Some(&b'/') {
                    normalized[index] = b' ';
                    normalized[index + 1] = b' ';
                    index += 2;
                    closed = true;
                    break;
                }
                if normalized[index] != b'\n' {
                    normalized[index] = b' ';
                }
                index += 1;
            }
            if !closed {
                return None;
            }
        } else {
            index += 1;
        }
    }
    if in_string {
        return None;
    }
    in_string = false;
    escaped = false;
    for index in 0..normalized.len() {
        let current = normalized[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if current == b'\\' {
                escaped = true;
            } else if current == b'"' {
                in_string = false;
            }
        } else if current == b'"' {
            in_string = true;
        } else if current == b','
            && normalized[index + 1..]
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|next| matches!(next, b'}' | b']'))
        {
            normalized[index] = b' ';
        }
    }
    serde_json::from_slice(&normalized).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_keeps_comment_markers_inside_strings_and_accepts_trailing_commas() {
        let source = br#"{"compilerOptions": {"baseUrl": ".", // comment
            "paths": {"@pkg/*": ["src/*",],}, "url": "https://host/a/*",},}"#;
        let value = jsonc_value(source).expect("JSONC parses");
        assert_eq!(value["compilerOptions"]["url"], "https://host/a/*");
        assert_eq!(value["compilerOptions"]["paths"]["@pkg/*"][0], "src/*");
        assert!(jsonc_value(b"{/* unterminated").is_none());
    }

    #[test]
    fn alias_specificity_and_paths_stay_inside_repository() {
        let source = r#"{"compilerOptions":{"baseUrl":".","paths":{"@pkg/*":["src/*"],"@pkg/special":["src/exact"]}}}"#;
        let encoded = serde_json::json!({"configs":[{
            "path":"tsconfig.json", "content_hash":content_hash(source.as_bytes()), "content":source
        }]});
        let mut manifest = b"fixture".to_vec();
        manifest.extend_from_slice(PROJECT_MODULE_RESOLUTION_MARKER);
        manifest.extend_from_slice(&serde_json::to_vec(&encoded).expect("manifest encodes"));
        let resolver =
            ModuleResolution::from_manifest(&manifest, SemanticProjectLanguage::TypeScript);
        assert_eq!(
            resolver.candidates("src/main.ts", "@pkg/special"),
            Some(vec!["src/exact".to_owned()])
        );
        assert_eq!(
            resolver.candidates("src/main.ts", "@pkg/other"),
            Some(vec!["src/other".to_owned()])
        );
        assert_eq!(resolver.candidates("src/main.ts", "./local"), None);
        assert_eq!(normalize_path("src", "../../outside"), None);
    }

    #[test]
    fn javascript_configuration_does_not_define_typescript_imports() {
        let source = r#"{"compilerOptions":{"baseUrl":".","paths":{"@script/*":["src/*"]}}}"#;
        let encoded = serde_json::json!({"configs":[{
            "path":"jsconfig.json", "content_hash":content_hash(source.as_bytes()), "content":source
        }]});
        let mut manifest = b"fixture".to_vec();
        manifest.extend_from_slice(PROJECT_MODULE_RESOLUTION_MARKER);
        manifest.extend_from_slice(&serde_json::to_vec(&encoded).expect("manifest encodes"));
        let typescript =
            ModuleResolution::from_manifest(&manifest, SemanticProjectLanguage::TypeScript);
        let javascript =
            ModuleResolution::from_manifest(&manifest, SemanticProjectLanguage::JavaScript);
        assert_eq!(typescript.candidates("src/main.ts", "@script/value"), None);
        assert_eq!(
            javascript.candidates("src/main.js", "@script/value"),
            Some(vec!["src/value".to_owned()])
        );
    }

    #[test]
    fn nested_config_without_local_paths_uses_root_resolution() {
        let root = r#"{"compilerOptions":{"baseUrl":".","paths":{"library/*":["src/library/*"],"library":["src/library/index"]}}}"#;
        let nested = r#"{"extends":"../tsconfig.json","compilerOptions":{"types":["node"]}}"#;
        let encoded = serde_json::json!({"configs":[
            {"path":"tsconfig.json","content_hash":content_hash(root.as_bytes()),"content":root},
            {"path":"test/tsconfig.json","content_hash":content_hash(nested.as_bytes()),"content":nested}
        ]});
        let mut manifest = b"fixture".to_vec();
        manifest.extend_from_slice(PROJECT_MODULE_RESOLUTION_MARKER);
        manifest.extend_from_slice(&serde_json::to_vec(&encoded).expect("manifest encodes"));
        let resolver =
            ModuleResolution::from_manifest(&manifest, SemanticProjectLanguage::TypeScript);
        assert_eq!(
            resolver.candidates("test/unit/example.ts", "library/index"),
            Some(vec!["src/library/index".to_owned()])
        );
    }
}
