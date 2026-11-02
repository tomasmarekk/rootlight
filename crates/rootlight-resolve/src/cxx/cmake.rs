//! Follows source-written CMake routes from authenticated native build entries.
//! Only direct declarations and pure subdirectory wrappers contribute paths;
//! missing SDKs, plugins and configured build state never become authority.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::FileId;
use rootlight_ir::SourceRef;
use tree_sitter::Node;

use super::{CapturedSource, NativeBuildContext, full_source, join, parse};
use crate::{ResolutionError, engine::ResolutionWorkBudget};

/// Ordered public include paths written for a local target in this directory.
/// Unknown entries remain in place so a later consumer can enforce precedence.
pub(super) struct WrittenIncludes {
    pub(super) directory: String,
    pub(super) roots: Vec<Option<String>>,
    pub(super) evidence: Vec<SourceRef>,
}

/// Follows captured, unconditional CMake source routes without executing CMake.
/// Cancellation and work exhaustion propagate; unsupported routes grant no roots.
pub(super) fn includes(
    captured: &BTreeMap<FileId, CapturedSource<'_, '_>>,
    contexts: Vec<NativeBuildContext>,
    work: &mut ResolutionWorkBudget,
    cancel: &Cancellation,
) -> Result<Vec<WrittenIncludes>, ResolutionError> {
    let mut walker = Walker {
        captured,
        work,
        cancel,
        active: BTreeSet::new(),
        output: Vec::new(),
    };
    let mut entries = BTreeMap::<String, Option<NativeBuildContext>>::new();
    for context in contexts {
        walker.tick()?;
        match entries.entry(context.entry.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(context));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if let Some(previous) = entry.get_mut() {
                    if previous.variables != context.variables {
                        entry.insert(None);
                    } else {
                        for source in context.evidence {
                            walker.tick()?;
                            if !previous.evidence.contains(&source) {
                                previous.evidence.push(source);
                            }
                        }
                    }
                }
            }
        }
    }
    for context in entries.into_values().flatten() {
        walker.tick()?;
        let variables = context
            .variables
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    value.map(|text| Value {
                        text,
                        anchored: true,
                    }),
                )
            })
            .collect();
        walker.visit(
            &context.entry,
            variables,
            BTreeMap::new(),
            context.evidence,
            0,
        )?;
    }
    Ok(walker.output)
}

#[derive(Clone, PartialEq, Eq)]
struct Value {
    text: String,
    anchored: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Wrapper {
    parameter: String,
    arguments: Vec<String>,
}

type Variables = BTreeMap<String, Option<Value>>;
type Wrappers = BTreeMap<String, Option<Wrapper>>;

struct Walker<'capture, 'document, 'bytes, 'work> {
    captured: &'capture BTreeMap<FileId, CapturedSource<'document, 'bytes>>,
    work: &'work mut ResolutionWorkBudget,
    cancel: &'work Cancellation,
    active: BTreeSet<String>,
    output: Vec<WrittenIncludes>,
}

impl Walker<'_, '_, '_, '_> {
    fn tick(&mut self) -> Result<(), ResolutionError> {
        self.cancel.check()?;
        self.work.consume()
    }

    fn children<'tree>(&mut self, node: Node<'tree>) -> Result<Vec<Node<'tree>>, ResolutionError> {
        let mut cursor = node.walk();
        let mut children = Vec::new();
        for child in node.named_children(&mut cursor) {
            self.tick()?;
            if !matches!(child.kind(), "line_comment" | "bracket_comment") {
                children.push(child);
            }
        }
        Ok(children)
    }

    fn arguments(
        &mut self,
        node: Node<'_>,
        bytes: &[u8],
    ) -> Result<Option<Vec<String>>, ResolutionError> {
        let Some(arguments) = self.raw_arguments(node, bytes)? else {
            return Ok(None);
        };
        if arguments.iter().any(|raw| {
            raw.contains(['\\', ';']) || raw.contains("$<") || raw.chars().any(char::is_control)
        }) {
            return Ok(None);
        }
        Ok(Some(arguments))
    }

    fn raw_arguments(
        &mut self,
        node: Node<'_>,
        bytes: &[u8],
    ) -> Result<Option<Vec<String>>, ResolutionError> {
        let mut arguments = Vec::new();
        for child in self.children(node)? {
            if child.kind() != "argument_list" {
                continue;
            }
            for argument in self.children(child)? {
                let parts = self.children(argument)?;
                let [value] = parts.as_slice() else {
                    return Ok(None);
                };
                let Some(raw) = text(*value, bytes) else {
                    return Ok(None);
                };
                let raw = match value.kind() {
                    "quoted_argument" => {
                        raw.strip_prefix('"').and_then(|raw| raw.strip_suffix('"'))
                    }
                    "unquoted_argument" => Some(raw.trim()),
                    _ => None,
                };
                let Some(raw) = raw else { return Ok(None) };
                arguments.push(raw.to_owned());
            }
        }
        Ok(Some(arguments))
    }

    fn visit(
        &mut self,
        entry: &str,
        mut variables: Variables,
        mut wrappers: Wrappers,
        mut evidence: Vec<SourceRef>,
        depth: usize,
    ) -> Result<(), ResolutionError> {
        self.tick()?;
        if depth > 32 || self.active.contains(entry) {
            return Ok(());
        }
        let mut source = None;
        let captured = self.captured;
        for captured in captured.values() {
            self.tick()?;
            if captured.file.path == entry {
                if source.is_some() {
                    return Ok(());
                }
                source = Some((captured.file, captured.bytes));
            }
        }
        let Some((file, bytes)) = source else {
            return Ok(());
        };
        let Some(tree) = parse(
            bytes,
            tree_sitter_cmake::LANGUAGE_CMAKE.into(),
            self.work,
            self.cancel,
        )?
        else {
            return Ok(());
        };
        if tree.root_node().has_error() {
            return Ok(());
        }
        let directory = entry
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let current = Some(Value {
            text: directory.to_owned(),
            anchored: true,
        });
        variables.insert("CMAKE_CURRENT_SOURCE_DIR".to_owned(), current.clone());
        variables.insert("CMAKE_CURRENT_LIST_DIR".to_owned(), current);
        evidence.push(full_source(file)?);
        self.active.insert(entry.to_owned());
        let mut targets = BTreeMap::<String, bool>::new();
        let mut roots = BTreeMap::<String, Vec<Option<String>>>::new();
        for node in self.children(tree.root_node())? {
            match node.kind() {
                "function_def" => {
                    let parts = self.children(node)?;
                    let [declaration, body, _] = parts.as_slice() else {
                        continue;
                    };
                    let Some(arguments) = self.arguments(*declaration, bytes)? else {
                        continue;
                    };
                    let Some(name) = arguments.first().filter(|name| literal(name)) else {
                        continue;
                    };
                    if core_command(name) {
                        self.active.remove(entry);
                        return Ok(());
                    }
                    let body = self.children(*body)?;
                    let wrapper = if let ([_, parameter], [command]) =
                        (arguments.as_slice(), body.as_slice())
                    {
                        if literal(parameter)
                            && command_name(*command, bytes).as_deref() == Some("add_subdirectory")
                        {
                            self.arguments(*command, bytes)?.map(|arguments| Wrapper {
                                parameter: parameter.clone(),
                                arguments,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let name = name.to_ascii_lowercase();
                    match wrappers.entry(name) {
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            entry.insert(None);
                        }
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(wrapper);
                        }
                    }
                }
                "normal_command" => {
                    let Some(name) = command_name(node, bytes) else {
                        continue;
                    };
                    if wrappers.get(&name).is_some_and(Option::is_none) {
                        continue;
                    }
                    let Some(arguments) = self.arguments(node, bytes)? else {
                        if core_command(&name) {
                            self.active.remove(entry);
                            return Ok(());
                        }
                        self.invalidate(node, bytes, &mut variables, &mut targets, &mut wrappers)?;
                        continue;
                    };
                    match name.as_str() {
                        "set" | "unset" => {
                            if let Some(name) = arguments.first().filter(|name| literal(name)) {
                                let value = if let [_, value] = arguments.as_slice() {
                                    self.value(value, &variables)?
                                } else {
                                    None
                                };
                                merge(
                                    &mut variables,
                                    name.clone(),
                                    if name == "CMAKE_CURRENT_SOURCE_DIR"
                                        || name == "CMAKE_CURRENT_LIST_DIR"
                                    {
                                        None
                                    } else {
                                        value
                                    },
                                );
                            }
                        }
                        "file" => {
                            let mut normalized = false;
                            if let [mode, value, name] = arguments.as_slice()
                                && mode.eq_ignore_ascii_case("TO_CMAKE_PATH")
                                && literal(name)
                            {
                                let value = self.value(value, &variables)?;
                                merge(&mut variables, name.clone(), value);
                                normalized = true;
                            }
                            if !normalized {
                                for name in self.file_outputs(&arguments, &variables)? {
                                    variables.insert(name, None);
                                }
                            }
                        }
                        "add_subdirectory" => self.subdirectory(
                            &arguments, &variables, &wrappers, &evidence, directory, depth,
                        )?,
                        "add_library" => {
                            if let Some(target) = arguments.first().filter(|name| literal(name)) {
                                let local = !arguments.iter().skip(1).any(|argument| {
                                    argument.eq_ignore_ascii_case("ALIAS")
                                        || argument.eq_ignore_ascii_case("IMPORTED")
                                });
                                targets
                                    .entry(target.clone())
                                    .and_modify(|valid| *valid = false)
                                    .or_insert(local);
                            }
                        }
                        "target_include_directories" => {
                            let Some(target) = arguments.first().filter(|name| literal(name))
                            else {
                                continue;
                            };
                            let mut public = false;
                            for argument in arguments.iter().skip(1) {
                                self.tick()?;
                                match argument.as_str() {
                                    "PUBLIC" => public = true,
                                    "PRIVATE" | "INTERFACE" => public = false,
                                    "BEFORE" | "AFTER" | "SYSTEM" => {
                                        targets.insert(target.clone(), false);
                                    }
                                    _ if public => {
                                        let root = self
                                            .value(argument, &variables)?
                                            .and_then(|value| path(&value, directory));
                                        roots.entry(target.clone()).or_default().push(root);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {
                            if let Some(Some(wrapper)) = wrappers.get(&name).cloned()
                                && let [argument] = arguments.as_slice()
                                && !argument.contains('$')
                            {
                                let mut bound = variables.clone();
                                bound.insert(
                                    wrapper.parameter,
                                    Some(Value {
                                        text: argument.clone(),
                                        anchored: false,
                                    }),
                                );
                                self.subdirectory(
                                    &wrapper.arguments,
                                    &bound,
                                    &wrappers,
                                    &evidence,
                                    directory,
                                    depth,
                                )?;
                            }
                        }
                    }
                }
                _ => self.invalidate(node, bytes, &mut variables, &mut targets, &mut wrappers)?,
            }
        }
        for (target, valid) in targets {
            self.tick()?;
            if valid && let Some(roots) = roots.remove(&target).filter(|roots| !roots.is_empty()) {
                self.output.push(WrittenIncludes {
                    directory: directory.to_owned(),
                    roots,
                    evidence: evidence.clone(),
                });
            }
        }
        self.active.remove(entry);
        Ok(())
    }

    fn subdirectory(
        &mut self,
        arguments: &[String],
        variables: &Variables,
        wrappers: &Wrappers,
        evidence: &[SourceRef],
        directory: &str,
        depth: usize,
    ) -> Result<(), ResolutionError> {
        if !(1..=2).contains(&arguments.len()) {
            return Ok(());
        }
        let Some(argument) = arguments.first() else {
            return Ok(());
        };
        let Some(directory) = self
            .value(argument, variables)?
            .and_then(|value| path(&value, directory))
        else {
            return Ok(());
        };
        let Some(entry) = join(&directory, "CMakeLists.txt") else {
            return Ok(());
        };
        self.visit(
            &entry,
            variables.clone(),
            wrappers.clone(),
            evidence.to_vec(),
            depth + 1,
        )
    }

    fn value(
        &mut self,
        raw: &str,
        variables: &Variables,
    ) -> Result<Option<Value>, ResolutionError> {
        let mut result = Value {
            text: String::new(),
            anchored: false,
        };
        let mut remaining = raw;
        while let Some((prefix, tail)) = remaining.split_once("${") {
            self.tick()?;
            if prefix.contains('$') {
                return Ok(None);
            }
            result.text.push_str(prefix);
            let Some((name, tail)) = tail.split_once('}') else {
                return Ok(None);
            };
            let Some(value) = variables.get(name).cloned().flatten() else {
                return Ok(None);
            };
            if value.anchored {
                if !result.text.is_empty() {
                    return Ok(None);
                }
                result.anchored = true;
            }
            result.text.push_str(&value.text);
            remaining = tail;
        }
        if remaining.contains('$') {
            return Ok(None);
        }
        result.text.push_str(remaining);
        Ok(Some(result))
    }

    fn invalidate(
        &mut self,
        node: Node<'_>,
        bytes: &[u8],
        variables: &mut Variables,
        targets: &mut BTreeMap<String, bool>,
        wrappers: &mut Wrappers,
    ) -> Result<(), ResolutionError> {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            self.tick()?;
            if matches!(node.kind(), "function_command" | "macro_command")
                && let Some(arguments) = self.arguments(node, bytes)?
                && let Some(name) = arguments.first().filter(|name| literal(name))
            {
                wrappers.insert(name.to_ascii_lowercase(), None);
            }
            if let Some(command) = command_name(node, bytes)
                && let Some(arguments) = self.raw_arguments(node, bytes)?
            {
                if command == "file" {
                    for name in self.file_outputs(&arguments, variables)? {
                        variables.insert(name, None);
                    }
                }
                if let Some(name) = arguments.first().filter(|name| literal(name)) {
                    match command.as_str() {
                        "set" | "unset" => {
                            variables.insert(name.clone(), None);
                        }
                        "add_library" | "target_include_directories" => {
                            targets.insert(name.clone(), false);
                        }
                        _ => {}
                    }
                }
            }
            pending.extend(self.children(node)?);
        }
        Ok(())
    }

    fn file_outputs(
        &mut self,
        arguments: &[String],
        variables: &Variables,
    ) -> Result<Vec<String>, ResolutionError> {
        let Some(mode) = arguments.first() else {
            return Ok(Vec::new());
        };
        let position = match mode.to_ascii_uppercase().as_str() {
            "GLOB" | "GLOB_RECURSE" | "RELATIVE_PATH" => Some(1),
            "TO_CMAKE_PATH" | "TO_NATIVE_PATH" | "REAL_PATH" | "READ" | "STRINGS" | "TIMESTAMP"
            | "SIZE" | "READ_SYMLINK" | "MD5" | "SHA1" | "SHA224" | "SHA256" | "SHA384"
            | "SHA512" | "SHA3_224" | "SHA3_256" | "SHA3_384" | "SHA3_512" => Some(2),
            _ => None,
        };
        if let Some(position) = position {
            return Ok(arguments
                .get(position)
                .filter(|name| literal(name))
                .cloned()
                .into_iter()
                .collect());
        }
        // Unknown file forms may remove an existing named binding, never create one.
        let mut outputs = Vec::new();
        for argument in arguments.iter().skip(1) {
            self.tick()?;
            if literal(argument) && variables.get(argument).is_some_and(Option::is_some) {
                outputs.push(argument.clone());
            }
        }
        Ok(outputs)
    }
}

fn text<'bytes>(node: Node<'_>, bytes: &'bytes [u8]) -> Option<&'bytes str> {
    std::str::from_utf8(bytes.get(node.byte_range())?).ok()
}

fn command_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    if node.kind() != "normal_command" || node.has_error() {
        return None;
    }
    text(node.named_child(0)?, bytes).map(str::to_ascii_lowercase)
}

fn literal(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn core_command(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "set"
            | "unset"
            | "file"
            | "add_subdirectory"
            | "add_library"
            | "target_include_directories"
    )
}

fn path(value: &Value, directory: &str) -> Option<String> {
    join(if value.anchored { "" } else { directory }, &value.text)
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
    use rootlight_ir::{FactEvidence, FileRecord, SourceSpan};

    use super::*;

    fn fixture(utils_prefix: &str) -> Vec<(FileRecord, Vec<u8>)> {
        // Complete MIT-licensed originals at 85a81818c6b9aa19e3d19453640e6ea58afb0357;
        // guard cases insert written commands before PUBLIC includes and rebind bytes.
        let originals = [
            (
                "packages/react-native/settings.gradle.kts",
                include_str!(
                    "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/settings.gradle.kts"
                ),
            ),
            (
                "packages/react-native/ReactAndroid/build.gradle.kts",
                include_str!(
                    "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/ReactAndroid/build.gradle.kts"
                ),
            ),
            (
                "packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt",
                include_str!(
                    "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt"
                ),
            ),
            (
                "packages/react-native/ReactCommon/react/utils/CMakeLists.txt",
                include_str!(
                    "../../../rootlight-service/src/tests/fixtures/cpp_include_resolution/packages/react-native/ReactCommon/react/utils/CMakeLists.txt"
                ),
            ),
        ];
        assert_eq!(
            originals.map(|(_, source)| source.len()),
            [1695, 32911, 19272, 1084]
        );
        let inputs = originals.map(|(path, source)| {
            let source = if !utils_prefix.is_empty() && path.ends_with("react/utils/CMakeLists.txt")
            {
                assert_eq!(source.matches("target_include_directories(").count(), 1);
                source.replacen(
                    "target_include_directories(",
                    &format!("{utils_prefix}target_include_directories("),
                    1,
                )
            } else {
                source.to_owned()
            };
            (path, source.into_bytes())
        });
        let repository = derive_repository(b"https://github.com/facebook/react-native").id();
        let manifest = inputs
            .iter()
            .flat_map(|(_, source)| source.iter().copied())
            .collect::<Vec<_>>();
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: content_hash(&manifest),
            config_hash: content_hash(b""),
            provider_set_hash: content_hash(b""),
            format_version: 1,
        })
        .id();
        inputs
            .into_iter()
            .map(|(path, source)| {
                let id = derive_file(FileIdentity {
                    repository,
                    path_identity: path.as_bytes(),
                })
                .id();
                let byte_length = u64::try_from(source.len()).unwrap();
                let hash = content_hash(&source);
                let file = FileRecord {
                    id,
                    repository,
                    generation,
                    path: path.to_owned(),
                    path_locator: None,
                    content_hash: hash,
                    byte_length,
                    language: if path.ends_with(".kts") {
                        "kotlin"
                    } else {
                        "cmake"
                    }
                    .to_owned(),
                    encoding: "utf-8".to_owned(),
                    generated: false,
                    provenance: derive_fact("authored-native-build-file", &source).id(),
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
                };
                (file, source)
            })
            .collect()
    }

    fn captured<'input>(
        records: &'input [(FileRecord, Vec<u8>)],
    ) -> BTreeMap<FileId, CapturedSource<'input, 'input>> {
        records
            .iter()
            .map(|(file, bytes)| (file.id, CapturedSource { file, bytes }))
            .collect()
    }

    #[test]
    fn authored_gradle_cmake_route_preserves_public_include_order() {
        let records = fixture("");
        let captured = captured(&records);
        let mut work = ResolutionWorkBudget::new(crate::DEFAULT_RESOLUTION_WORK_LIMIT);
        let cancel = Cancellation::new();
        let contexts = super::super::gradle::builds(&captured, &mut work, &cancel).unwrap();
        let observed_contexts = contexts
            .iter()
            .map(|context| (context.entry.clone(), context.variables.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            contexts.len(),
            1,
            "authored contexts: {observed_contexts:?}"
        );
        assert_eq!(
            contexts.first().unwrap().entry,
            "packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt"
        );
        let output = includes(&captured, contexts, &mut work, &cancel).unwrap();
        let observed_routes = output
            .iter()
            .map(|route| {
                (
                    route.directory.clone(),
                    route.roots.clone(),
                    route.evidence.clone(),
                )
            })
            .collect::<Vec<_>>();
        let utils = output
            .iter()
            .filter(|route| route.directory == "packages/react-native/ReactCommon/react/utils")
            .collect::<Vec<_>>();
        assert_eq!(
            utils.len(),
            1,
            "contexts: {observed_contexts:?}; routes: {observed_routes:?}"
        );
        let route = utils.first().unwrap();
        assert_eq!(
            route.roots,
            vec![Some("packages/react-native/ReactCommon".to_owned()), None],
            "contexts: {observed_contexts:?}; routes: {observed_routes:?}"
        );
        let expected_evidence = records
            .iter()
            .map(|(file, _)| full_source(file).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            route.evidence, expected_evidence,
            "contexts: {observed_contexts:?}; routes: {observed_routes:?}"
        );
        assert_eq!(
            route
                .evidence
                .iter()
                .map(SourceRef::content_hash)
                .collect::<Vec<_>>(),
            records
                .iter()
                .map(|(_, source)| content_hash(source))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn conflicting_contexts_and_conditional_file_outputs_refuse_roots() {
        let records = fixture("");
        let sources = captured(&records);
        let cancel = Cancellation::new();
        let mut work = ResolutionWorkBudget::new(crate::DEFAULT_RESOLUTION_WORK_LIMIT);
        let mut contexts = super::super::gradle::builds(&sources, &mut work, &cancel).unwrap();
        assert_eq!(contexts.len(), 1);
        let correct = contexts.pop().unwrap();
        let mut wrong_variables = correct.variables.clone();
        wrong_variables.insert(
            "REACT_COMMON_DIR".to_owned(),
            Some("missing/native/source".to_owned()),
        );
        let conflicting = NativeBuildContext {
            entry: correct.entry.clone(),
            variables: wrong_variables,
            evidence: correct.evidence.clone(),
        };
        assert!(
            includes(&sources, vec![conflicting, correct], &mut work, &cancel)
                .unwrap()
                .is_empty(),
            "a missing conflicting route must not disappear before entry agreement"
        );

        for (command, known_root) in [
            (
                "if(UNCONFIGURED)\r\nfile(TO_CMAKE_PATH \"${UNAVAILABLE_ROOT}\" REACT_COMMON_DIR)\r\nendif()\r\n",
                false,
            ),
            (
                "if(UNCONFIGURED)\r\nfile(GLOB REACT_COMMON_DIR \"${CMAKE_CURRENT_SOURCE_DIR}/*\")\r\nendif()\r\n",
                false,
            ),
            (
                "if(UNCONFIGURED)\r\nfile(GLOB react_utils_SRC \"${CMAKE_CURRENT_SOURCE_DIR}/*.cpp\")\r\nendif()\r\n",
                true,
            ),
        ] {
            let records = fixture(command);
            let sources = captured(&records);
            let mut work = ResolutionWorkBudget::new(crate::DEFAULT_RESOLUTION_WORK_LIMIT);
            let contexts = super::super::gradle::builds(&sources, &mut work, &cancel).unwrap();
            let output = includes(&sources, contexts, &mut work, &cancel).unwrap();
            let roots = output
                .iter()
                .filter(|route| route.directory == "packages/react-native/ReactCommon/react/utils")
                .map(|route| route.roots.clone())
                .collect::<Vec<_>>();
            let expected = known_root.then(|| "packages/react-native/ReactCommon".to_owned());
            assert_eq!(
                roots,
                vec![vec![expected, None]],
                "written guard input: {command}"
            );
        }
    }
}
