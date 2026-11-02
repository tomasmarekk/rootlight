//! Captures a bounded, authored Gradle Kotlin path into a native CMake entry.
//! Explicit settings mappings and direct Android DSL blocks provide evidence;
//! this module neither executes Gradle nor claims a configured compiler profile.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::FileId;
use tree_sitter::Node;

use super::{CapturedSource, NativeBuildContext, full_source, join, parse};
use crate::{ResolutionError, engine::ResolutionWorkBudget};

/// Extracts only explicit, unconditional project mappings and native build inputs.
/// Unsupported expressions retain no path authority; budget and cancellation
/// failures propagate to the resolver rather than producing partial evidence.
pub(super) fn builds(
    sources: &BTreeMap<FileId, CapturedSource<'_, '_>>,
    work: &mut ResolutionWorkBudget,
    cancel: &Cancellation,
) -> Result<Vec<NativeBuildContext>, ResolutionError> {
    let mut builds = Vec::new();
    for settings in sources.values() {
        cancel.check()?;
        work.consume()?;
        if settings.file.path.rsplit('/').next() != Some("settings.gradle.kts") {
            continue;
        }
        let Some(tree) = parse(
            settings.bytes,
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            work,
            cancel,
        )?
        else {
            continue;
        };
        let mut reader = Reader {
            bytes: settings.bytes,
            work,
            cancel,
        };
        let mut included = BTreeSet::new();
        let mut mappings = BTreeMap::<String, Option<String>>::new();
        let settings_dir = directory(&settings.file.path);
        for node in reader.children(tree.root_node())? {
            if reader.shadows_settings_call(node)?
                || (is_control(node)
                    && reader.mentions(node, &["include", "project", "projectDir"])?)
            {
                mappings.clear();
                included.clear();
                break;
            }
            if node.has_error() {
                continue;
            }
            if reader.callee(node).as_deref() == Some("include") {
                for argument in reader.arguments(node)? {
                    if let Some(id) = reader.value(argument, &BTreeMap::new())? {
                        included.insert(id.text);
                    }
                }
            }
            if node.kind() != "assignment" {
                continue;
            }
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                continue;
            };
            if left.kind() != "navigation_expression" {
                continue;
            }
            let parts = reader.children(left)?;
            let [receiver, member] = parts.as_slice() else {
                continue;
            };
            if reader.text(*member) != Some("projectDir")
                || reader.callee(*receiver).as_deref() != Some("project")
            {
                continue;
            }
            let arguments = reader.arguments(*receiver)?;
            let [argument] = arguments.as_slice() else {
                continue;
            };
            let Some(id) = reader.value(*argument, &BTreeMap::new())? else {
                continue;
            };
            let path = if node
                .child_by_field_name("operator")
                .and_then(|operator| reader.text(operator))
                == Some("=")
                && reader.callee(right).as_deref() == Some("file")
            {
                let arguments = reader.arguments(right)?;
                if let [argument] = arguments.as_slice() {
                    reader
                        .value(*argument, &BTreeMap::new())?
                        .and_then(|path| join(settings_dir, &path.text))
                } else {
                    None
                }
            } else {
                None
            };
            merge(&mut mappings, id.text, path);
        }
        for (id, project) in mappings {
            reader.tick()?;
            if !included.contains(&id) {
                continue;
            }
            let Some(project) = project else { continue };
            let Some(build_path) = join(&project, "build.gradle.kts") else {
                continue;
            };
            for build in sources.values() {
                reader.tick()?;
                if build.file.path != build_path {
                    continue;
                }
                if let Some(mut context) = build_context(build, &project, reader.work, cancel)? {
                    context.evidence = vec![full_source(settings.file)?, full_source(build.file)?];
                    builds.push(context);
                }
            }
        }
    }
    Ok(builds)
}

fn build_context(
    source: &CapturedSource<'_, '_>,
    project: &str,
    work: &mut ResolutionWorkBudget,
    cancel: &Cancellation,
) -> Result<Option<NativeBuildContext>, ResolutionError> {
    let Some(tree) = parse(
        source.bytes,
        tree_sitter_kotlin_ng::LANGUAGE.into(),
        work,
        cancel,
    )?
    else {
        return Ok(None);
    };
    let mut reader = Reader {
        bytes: source.bytes,
        work,
        cancel,
    };
    let mut environment = BTreeMap::from([(
        "projectDir".to_owned(),
        Some(Evaluated {
            text: project.to_owned(),
            anchor: Some(0),
        }),
    )]);
    let mut variables = BTreeMap::new();
    let mut paths = Vec::new();
    for node in reader.children(tree.root_node())? {
        if is_control(node)
            && reader.mentions(node, &["android", "externalNativeBuild", "projectDir"])?
        {
            return Ok(None);
        }
        if node.has_error() {
            continue;
        }
        if node.kind() == "property_declaration" {
            let children = reader.children(node)?;
            if let Some(declaration) = children
                .iter()
                .find(|child| child.kind() == "variable_declaration")
            {
                let names = reader.children(*declaration)?;
                if let Some(name) = names
                    .first()
                    .and_then(|name| reader.text(*name))
                    .map(str::to_owned)
                {
                    let value = if reader
                        .text(node)
                        .is_some_and(|text| text.starts_with("val "))
                    {
                        children
                            .last()
                            .filter(|value| value.id() != declaration.id())
                            .copied()
                    } else {
                        None
                    };
                    let value = match value {
                        Some(value) => reader.value(value, &environment)?,
                        None => None,
                    };
                    merge(&mut environment, name, value);
                }
            }
        }
        if node.kind() == "assignment"
            && let Some(name) = node
                .child_by_field_name("left")
                .and_then(|left| reader.text(left))
                .map(str::to_owned)
            && environment.contains_key(&name)
        {
            environment.insert(name, None);
        }
        if reader.callee(node).as_deref() == Some("android") {
            reader.android(node, &environment, &mut variables, &mut paths)?;
        }
    }
    if environment
        .get("projectDir")
        .and_then(Option::as_ref)
        .is_none_or(|value| value.anchor != Some(0) || value.text != project)
    {
        return Ok(None);
    }
    let mut entries = BTreeSet::new();
    for path in paths {
        reader.tick()?;
        let Some(entry) = path.and_then(|path| match path.anchor {
            None => join(project, &path.text),
            Some(0) => join("", &path.text),
            Some(_) => None,
        }) else {
            return Ok(None);
        };
        entries.insert(entry);
    }
    if entries.len() != 1 {
        return Ok(None);
    }
    Ok(entries.into_iter().next().map(|entry| NativeBuildContext {
        entry,
        variables,
        evidence: Vec::new(),
    }))
}

#[derive(Clone, PartialEq, Eq)]
struct Evaluated {
    text: String,
    // Offsets preserve path provenance through the literal -DNAME= prefix.
    anchor: Option<usize>,
}

struct Reader<'bytes, 'work> {
    bytes: &'bytes [u8],
    work: &'work mut ResolutionWorkBudget,
    cancel: &'work Cancellation,
}

impl Reader<'_, '_> {
    fn tick(&mut self) -> Result<(), ResolutionError> {
        self.cancel.check()?;
        self.work.consume()
    }

    fn children<'tree>(&mut self, node: Node<'tree>) -> Result<Vec<Node<'tree>>, ResolutionError> {
        let mut children = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.tick()?;
            if !matches!(child.kind(), "line_comment" | "block_comment") {
                children.push(child);
            }
        }
        Ok(children)
    }

    fn text(&self, node: Node<'_>) -> Option<&str> {
        std::str::from_utf8(self.bytes.get(node.byte_range())?).ok()
    }

    fn mentions(&mut self, node: Node<'_>, names: &[&str]) -> Result<bool, ResolutionError> {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            self.tick()?;
            if node.kind() == "identifier"
                && self.text(node).is_some_and(|name| names.contains(&name))
            {
                return Ok(true);
            }
            pending.extend(self.children(node)?);
        }
        Ok(false)
    }

    fn callee(&self, node: Node<'_>) -> Option<String> {
        if node.kind() != "call_expression" || node.has_error() {
            return None;
        }
        let callee = node.named_child(0)?;
        (callee.kind() == "identifier")
            .then(|| self.text(callee).map(str::to_owned))
            .flatten()
    }

    fn shadows_settings_call(&mut self, node: Node<'_>) -> Result<bool, ResolutionError> {
        let name = match node.kind() {
            "function_declaration" | "class_declaration" | "object_declaration" => {
                node.child_by_field_name("name")
            }
            "type_alias" => node.child_by_field_name("type"),
            "property_declaration" => {
                let declarations = self.children(node)?;
                let mut name = None;
                for declaration in declarations {
                    if declaration.kind() == "variable_declaration" {
                        name = declaration.named_child(0);
                    }
                }
                name
            }
            _ => None,
        };
        Ok(name
            .and_then(|name| self.text(name))
            .is_some_and(|name| matches!(name, "file" | "include" | "project")))
    }

    fn arguments<'tree>(&mut self, node: Node<'tree>) -> Result<Vec<Node<'tree>>, ResolutionError> {
        let mut arguments = Vec::new();
        for child in self.children(node)? {
            if child.kind() == "value_arguments" {
                for argument in self.children(child)? {
                    let values = self.children(argument)?;
                    if let [value] = values.as_slice() {
                        arguments.push(*value);
                    } else {
                        return Ok(Vec::new());
                    }
                }
            }
        }
        Ok(arguments)
    }

    fn body<'tree>(
        &mut self,
        node: Node<'tree>,
    ) -> Result<Option<Vec<Node<'tree>>>, ResolutionError> {
        let children = self.children(node)?;
        if children
            .iter()
            .any(|child| matches!(child.kind(), "value_arguments" | "type_arguments"))
        {
            return Ok(None);
        }
        for child in children {
            if child.kind() == "annotated_lambda" {
                let parts = self.children(child)?;
                if let [lambda] = parts.as_slice()
                    && lambda.kind() == "lambda_literal"
                {
                    let statements = self.children(*lambda)?;
                    return Ok((!statements
                        .iter()
                        .any(|statement| statement.kind() == "lambda_parameters"))
                    .then_some(statements));
                }
            }
        }
        Ok(None)
    }

    fn value(
        &mut self,
        node: Node<'_>,
        environment: &BTreeMap<String, Option<Evaluated>>,
    ) -> Result<Option<Evaluated>, ResolutionError> {
        self.value_at_depth(node, environment, 0)
    }

    fn value_at_depth(
        &mut self,
        node: Node<'_>,
        environment: &BTreeMap<String, Option<Evaluated>>,
        depth: u8,
    ) -> Result<Option<Evaluated>, ResolutionError> {
        self.tick()?;
        if depth > 16 || node.has_error() {
            return Ok(None);
        }
        if node.kind() == "identifier" {
            return Ok(self
                .text(node)
                .and_then(|name| environment.get(name))
                .cloned()
                .flatten());
        }
        if node.kind() == "navigation_expression" && self.text(node) == Some("projectDir.parent") {
            return Ok(environment
                .get("projectDir")
                .cloned()
                .flatten()
                .filter(|value| value.anchor == Some(0))
                .map(|value| Evaluated {
                    text: directory(&value.text).to_owned(),
                    anchor: Some(0),
                }));
        }
        if node.kind() != "string_literal" {
            return Ok(None);
        }
        let mut value = Evaluated {
            text: String::new(),
            anchor: None,
        };
        let children = self.children(node)?;
        let mut children = children.into_iter().peekable();
        while let Some(child) = children.next() {
            match child.kind() {
                "string_content" => {
                    let Some(text) = self.text(child) else {
                        return Ok(None);
                    };
                    // The pinned grammar can alias short $name interpolation to
                    // adjacent content nodes; its bytes still carry Kotlin meaning.
                    if text == "$"
                        && let Some(next) = children.peek().copied()
                        && next.kind() == "string_content"
                        && next.start_byte() == child.end_byte()
                    {
                        let Some(tail) = self.text(next) else {
                            return Ok(None);
                        };
                        let length = tail
                            .bytes()
                            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                            .count();
                        if tail
                            .as_bytes()
                            .first()
                            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                        {
                            let Some(name) = tail.get(..length) else {
                                return Ok(None);
                            };
                            let Some(remainder) = tail.get(length..) else {
                                return Ok(None);
                            };
                            if remainder
                                .chars()
                                .next()
                                .is_some_and(|character| !character.is_ascii())
                            {
                                return Ok(None);
                            }
                            let Some(part) = environment.get(name).cloned().flatten() else {
                                return Ok(None);
                            };
                            if value.anchor.is_none() {
                                value.anchor = part
                                    .anchor
                                    .and_then(|anchor| value.text.len().checked_add(anchor));
                            }
                            value.text.push_str(&part.text);
                            value.text.push_str(remainder);
                            let _ = children.next();
                            continue;
                        }
                        if tail
                            .chars()
                            .next()
                            .is_some_and(|character| !character.is_ascii())
                        {
                            return Ok(None);
                        }
                    }
                    value.text.push_str(text);
                }
                "interpolation" => {
                    let children = self.children(child)?;
                    let [expression] = children.as_slice() else {
                        return Ok(None);
                    };
                    let Some(part) = self.value_at_depth(*expression, environment, depth + 1)?
                    else {
                        return Ok(None);
                    };
                    if value.anchor.is_none() {
                        value.anchor = part
                            .anchor
                            .and_then(|anchor| value.text.len().checked_add(anchor));
                    }
                    value.text.push_str(&part.text);
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(value))
    }

    fn android(
        &mut self,
        node: Node<'_>,
        environment: &BTreeMap<String, Option<Evaluated>>,
        variables: &mut BTreeMap<String, Option<String>>,
        paths: &mut Vec<Option<Evaluated>>,
    ) -> Result<(), ResolutionError> {
        let Some(children) = self.body(node)? else {
            paths.push(None);
            return Ok(());
        };
        for child in children {
            if is_control(child)
                && self.mentions(
                    child,
                    &[
                        "defaultConfig",
                        "externalNativeBuild",
                        "cmake",
                        "path",
                        "arguments",
                    ],
                )?
            {
                paths.push(None);
            }
            match self.callee(child).as_deref() {
                Some("defaultConfig") => {
                    let Some(children) = self.body(child)? else {
                        paths.push(None);
                        continue;
                    };
                    for nested in children {
                        if is_control(nested)
                            && self.mentions(
                                nested,
                                &["externalNativeBuild", "cmake", "path", "arguments"],
                            )?
                        {
                            paths.push(None);
                        }
                        if self.callee(nested).as_deref() == Some("externalNativeBuild") {
                            self.native(nested, environment, variables, paths, false)?;
                        }
                    }
                }
                Some("externalNativeBuild") => {
                    self.native(child, environment, variables, paths, true)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn native(
        &mut self,
        node: Node<'_>,
        environment: &BTreeMap<String, Option<Evaluated>>,
        variables: &mut BTreeMap<String, Option<String>>,
        paths: &mut Vec<Option<Evaluated>>,
        entry: bool,
    ) -> Result<(), ResolutionError> {
        let Some(children) = self.body(node)? else {
            paths.push(None);
            return Ok(());
        };
        for child in children {
            if is_control(child) && self.mentions(child, &["cmake", "path", "arguments"])? {
                paths.push(None);
            }
            if self.callee(child).as_deref() != Some("cmake") {
                continue;
            }
            let Some(commands) = self.body(child)? else {
                paths.push(None);
                continue;
            };
            for command in commands {
                if command.has_error() {
                    paths.push(None);
                    continue;
                }
                match self.callee(command).as_deref() {
                    Some("path") if entry => {
                        let arguments = self.arguments(command)?;
                        let value = if let [argument] = arguments.as_slice() {
                            self.value(*argument, environment)?
                        } else {
                            None
                        };
                        paths.push(value);
                    }
                    Some("arguments") => {
                        for argument in self.arguments(command)? {
                            if argument.kind() != "string_literal" {
                                paths.push(None);
                                continue;
                            }
                            let Some(raw) = self
                                .text(argument)
                                .and_then(|text| text.strip_prefix('"'))
                                .and_then(|text| text.strip_prefix("-D"))
                            else {
                                continue;
                            };
                            let Some((name, _)) = raw.split_once('=') else {
                                continue;
                            };
                            if name.is_empty()
                                || !name
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                            {
                                continue;
                            }
                            let name = name.to_owned();
                            let value = self.value(argument, environment)?.and_then(|value| {
                                let (prefix, path) = value.text.split_once('=')?;
                                (value.anchor == prefix.len().checked_add(1))
                                    .then(|| join("", path))
                                    .flatten()
                            });
                            merge(variables, name, value);
                        }
                    }
                    _ => {
                        if is_control(command) && self.mentions(command, &["path", "arguments"])? {
                            paths.push(None);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn is_control(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "if_expression"
            | "when_expression"
            | "try_expression"
            | "for_statement"
            | "while_statement"
            | "do_while_statement"
            | "function_declaration"
            | "class_declaration"
            | "object_declaration"
    )
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn merge<T: PartialEq>(values: &mut BTreeMap<String, Option<T>>, name: String, value: Option<T>) {
    values
        .entry(name)
        .and_modify(|previous| {
            if *previous != value {
                *previous = None;
            }
        })
        .or_insert(value);
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{
        FileIdentity, GenerationIdentity, content_hash, derive_fact, derive_file,
        derive_generation, derive_repository,
    };
    use rootlight_ir::{FactEvidence, FileRecord, SourceRef, SourceSpan};

    use super::*;

    #[test]
    fn authored_settings_anchor_native_cmake_paths() {
        // Complete MIT-licensed upstream sources at 85a81818c6b9aa19e3d19453640e6ea58afb0357;
        // the retained fixture files preserve their original notices and CRLF bytes.
        const SETTINGS: &str = include_str!(
            "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/settings.gradle.kts"
        );
        const BUILD: &str = include_str!(
            "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/ReactAndroid/build.gradle.kts"
        );
        assert_eq!(SETTINGS.len(), 1695);
        assert_eq!(BUILD.len(), 32911);
        let repository = derive_repository(b"authored-gradle-context-fixture").id();
        let manifest = [SETTINGS.as_bytes(), BUILD.as_bytes()].concat();
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: content_hash(&manifest),
            config_hash: content_hash(b""),
            provider_set_hash: content_hash(b""),
            format_version: 1,
        })
        .id();
        let record = |path: &str, bytes: &[u8]| {
            let id = derive_file(FileIdentity {
                repository,
                path_identity: path.as_bytes(),
            })
            .id();
            let byte_length = u64::try_from(bytes.len()).unwrap();
            let hash = content_hash(bytes);
            FileRecord {
                id,
                repository,
                generation,
                path: path.to_owned(),
                path_locator: None,
                content_hash: hash,
                byte_length,
                language: "kotlin".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: false,
                provenance: derive_fact("authored-gradle-file", bytes).id(),
                evidence: FactEvidence {
                    source: Some(SourceRef::new(
                        repository,
                        generation,
                        SourceSpan::new(id, 0, byte_length).unwrap(),
                        hash,
                        None,
                    )),
                    derivation: Vec::new(),
                },
            }
        };
        let settings = record(
            "packages/react-native/settings.gradle.kts",
            SETTINGS.as_bytes(),
        );
        let build = record(
            "packages/react-native/ReactAndroid/build.gradle.kts",
            BUILD.as_bytes(),
        );
        let captured = BTreeMap::from([
            (
                settings.id,
                CapturedSource {
                    file: &settings,
                    bytes: SETTINGS.as_bytes(),
                },
            ),
            (
                build.id,
                CapturedSource {
                    file: &build,
                    bytes: BUILD.as_bytes(),
                },
            ),
        ]);
        let contexts = builds(
            &captured,
            &mut ResolutionWorkBudget::new(crate::DEFAULT_RESOLUTION_WORK_LIMIT),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(contexts.len(), 1);
        let context = contexts.first().unwrap();
        assert_eq!(
            context.entry,
            "packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt"
        );
        assert_eq!(
            context
                .variables
                .get("REACT_COMMON_DIR")
                .and_then(Option::as_deref),
            Some("packages/react-native/ReactCommon")
        );
        assert_eq!(
            context
                .variables
                .get("REACT_ANDROID_DIR")
                .and_then(Option::as_deref),
            Some("packages/react-native/ReactAndroid")
        );
        assert_eq!(context.variables.get("REACT_BUILD_DIR"), Some(&None));
        assert_eq!(context.variables.get("ANDROID_STL"), Some(&None));
        assert_eq!(
            context.evidence,
            vec![
                full_source(&settings).unwrap(),
                full_source(&build).unwrap()
            ]
        );
        assert_eq!(
            context.evidence.first().unwrap().content_hash(),
            content_hash(SETTINGS.as_bytes())
        );
        assert_eq!(
            context.evidence.last().unwrap().content_hash(),
            content_hash(BUILD.as_bytes())
        );
    }
}
