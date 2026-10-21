//! Written Rust module paths for project-local free-function calls.
//! Module membership requires authored declarations and complete physical source
//! contexts. This route is independent of the same-module typed call envelope.

use super::{
    AdapterError, BTreeMap, BTreeSet, CoverageStatus, EntityKind, FactRef, FileId, OccurrenceRole,
    ProjectFactsBuilder, RustBindingLookup, RustBindings, SourceRef, SourceSpan, SymbolId,
    SyntaxFact, SyntaxFactKind, contains_span, provider_failure, rust_binding_name_fact,
    rust_source::RustSourceText,
};
use rootlight_adapter_sdk::{SyntaxKindLabel, canonical_rust_identifier};

mod trait_candidates;
pub(super) use trait_candidates::TraitCallProof;

/// A unique foreign free-function binding with authenticated source dependencies.
pub(super) struct CallProof {
    pub(super) target: SymbolId,
    pub(super) derivation: Vec<FactRef>,
}

/// Written module bindings and source-bound unsupported generic-call regions.
pub(super) struct ModuleCalls {
    pub(super) proofs: BTreeMap<SourceSpan, CallProof>,
    pub(super) generic_gaps: Vec<SourceSpan>,
    pub(super) trait_calls: BTreeMap<SourceSpan, TraitCallProof>,
}

struct FileSyntax<'a> {
    root: u64,
    facts: BTreeMap<u64, &'a SyntaxFact>,
    text: RustSourceText<'a>,
    bindings: RustBindings,
    opaque: bool,
}

#[derive(Clone)]
struct Module {
    file: FileId,
    scope: u64,
    root: usize,
    parent: Option<usize>,
    name: Option<String>,
    directory: String,
    visibility: Visibility,
    witnesses: Vec<SourceRef>,
}

#[derive(Clone, Copy)]
enum Visibility {
    Public,
    Private,
    Parent,
    Unavailable,
}

struct WrittenUse {
    local: String,
    path: Vec<String>,
    source: SourceRef,
}

struct Graph {
    modules: Vec<Module>,
    children: BTreeMap<(usize, String), Vec<usize>>,
    placements: BTreeMap<(FileId, u64), Vec<usize>>,
    imports: BTreeMap<usize, Vec<WrittenUse>>,
    conflicting_modules: BTreeSet<(usize, String)>,
}

/// Builds only bindings justified by complete written module contexts.
///
/// # Errors
/// Returns cancellation, source decoding, or immutable request accounting errors.
pub(super) fn resolve(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<ModuleCalls, AdapterError> {
    let mut files = BTreeMap::new();
    for input in &builder.parsed {
        builder.cancellation.check()?;
        let roots: Vec<_> = input
            .facts
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Root
                    && fact.syntax_kind().as_str() == "rust.file.root"
            })
            .collect();
        let [root] = roots.as_slice() else { continue };
        if input.parse_status != CoverageStatus::Complete
            || root.parent().is_some()
            || !complete_source_root(root.span(), input.input)
            || input
                .facts
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "rust.proof_context.signature"
                        && full_context_reaches_root(fact, root, &input.facts)
                        && fact.span() == root.span()
                })
                .count()
                != 1
        {
            continue;
        }
        let text = RustSourceText::new(
            input.input.source().bytes(),
            input.input.encoding().as_str(),
            &input.facts,
            builder.request.limits(),
            builder.cancellation,
        )?;
        let names = input
            .facts
            .iter()
            .filter(|fact| rust_binding_name_fact(fact))
            .filter_map(|fact| {
                canonical_rust_identifier(
                    text.text(fact.span())?,
                    builder.request.limits().ir().max_string_bytes,
                )
                .map(|name| (fact.local_id(), name.into_owned()))
            })
            .collect();
        let bindings = RustBindings::new(
            &input.facts,
            &names,
            builder.request.limits().ir().max_string_bytes,
            builder.cancellation,
        )?;
        // Attributes and macro expansion can alter membership or introduce names.
        // Their bytes remain indexed, but a written graph cannot decide activation.
        let facts: BTreeMap<_, _> = input
            .facts
            .iter()
            .map(|fact| (fact.local_id(), fact))
            .collect();
        let opaque = input.facts.iter().any(|fact| {
            fact.syntax_kind().as_str() == "rust.attribute.signature"
                || fact.syntax_kind().as_str().contains("macro")
                || fact.syntax_kind().as_str().contains("extern_crate")
                || fact.syntax_kind().as_str().contains("foreign")
                || fact.syntax_kind().as_str() == "rust.namespace_barrier.signature"
                    && text
                        .text(fact.span())
                        .and_then(tokens)
                        .is_none_or(|tokens| {
                            tokens
                                .iter()
                                .take_while(|token| {
                                    !matches!(
                                        token.as_str(),
                                        "fn" | "mod"
                                            | "struct"
                                            | "enum"
                                            | "union"
                                            | "trait"
                                            | "type"
                                            | "const"
                                            | "static"
                                    )
                                })
                                .any(|token| matches!(token.as_str(), "extern" | "macro_rules"))
                        })
                || fact.kind() == SyntaxFactKind::Import
                    && (item_module(fact, &facts).is_none()
                        || text.text(fact.span()).and_then(written_use).is_none())
                || fact.kind() == SyntaxFactKind::Declaration
                    && !matches!(
                        fact.syntax_kind().as_str(),
                        "rust.local_variable.declaration"
                            | "rust.parameter.declaration"
                            | "rust.anonymous_function.declaration"
                    )
                    && item_module(fact, &facts).is_none()
                    && !trait_candidates::direct_member(fact, &facts, &text)
        });
        files.insert(
            root.span().file(),
            FileSyntax {
                root: root.local_id(),
                facts,
                text,
                bindings,
                opaque,
            },
        );
    }
    let graph = Graph::new(builder, &files)?;
    let mut proofs = BTreeMap::new();
    let mut generic_gaps = BTreeSet::new();
    for file in files.values() {
        for fact in file
            .facts
            .values()
            .filter(|fact| fact.syntax_kind().as_str() == "rust.path_name.reference")
        {
            builder.cancellation.check()?;
            if file.facts.values().any(|call| {
                call.syntax_kind().as_str() == "rust.generic_call.call"
                    && contains_span(call.span(), fact.span())
            }) {
                continue;
            }
            let mut parent = fact.parent();
            for _ in 0..file.facts.len() {
                let Some(scope) = parent.and_then(|id| file.facts.get(&id)) else {
                    break;
                };
                if scope.kind() == SyntaxFactKind::Scope && contains_span(scope.span(), fact.span())
                {
                    let suffix = SourceSpan::new(
                        fact.span().file(),
                        fact.span().end_byte(),
                        scope.span().end_byte(),
                    )
                    .map_err(|_| provider_failure("rust-generic-call-source"))?;
                    if file
                        .text
                        .text(suffix)
                        .and_then(tokens)
                        .is_some_and(|tokens| generic_callee_suffix(&tokens))
                    {
                        generic_gaps.insert(scope.span());
                    }
                    break;
                }
                parent = scope.parent();
            }
        }
    }
    for call in builder
        .occurrences
        .iter()
        .filter(|call| call.role == OccurrenceRole::CallSite)
    {
        builder.cancellation.check()?;
        if !matches!(
            call.syntax_kind.as_str(),
            "rust.direct_call.call" | "rust.scoped_call.scoped_call"
        ) {
            continue;
        }
        let Some(file) = files.get(&call.file).filter(|file| !file.opaque) else {
            continue;
        };
        let Some(fact) = file.facts.values().find(|fact| {
            fact.span() == call.source.span() && fact.syntax_kind().as_str() == call.syntax_kind
        }) else {
            continue;
        };
        let Some(scope) = containing_module(fact, file, builder)? else {
            continue;
        };
        let Some(&caller) = unique(graph.placements.get(&(call.file, scope))) else {
            continue;
        };
        if !graph.context_is_unique(caller) {
            continue;
        }
        let Some(mut path) = file.text.text(call.source.span()).and_then(path_tokens) else {
            continue;
        };
        let Some(first) = path.first() else { continue };
        let rooted = matches!(first.as_str(), "crate" | "self" | "super");
        if !rooted {
            // Scoped calls use the same lexical environment, selecting the first
            // path segment rather than confusing the terminal name with a binding.
            let probe = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                SyntaxFactKind::Occurrence,
                fact.span(),
                fact.depth(),
                SyntaxKindLabel::new("rust.direct_call.call")
                    .map_err(|_| provider_failure("rust-module-call-kind"))?,
            );
            if file.bindings.lookup(&probe, first, builder.cancellation)?
                != RustBindingLookup::NotLocal
            {
                continue;
            }
        }
        let mut witnesses = graph.modules[caller].witnesses.clone();
        if !rooted {
            let imports: Vec<_> = graph
                .imports
                .get(&caller)
                .into_iter()
                .flatten()
                .filter(|import| import.local == *first)
                .collect();
            if let [import] = imports.as_slice() {
                // A local item with the imported spelling makes the namespace
                // ambiguous even if malformed input materialized a stable symbol.
                if module_item_named(builder, file, graph.modules[caller].scope, first)
                    || graph.children.contains_key(&(caller, first.clone()))
                {
                    continue;
                }
                path.splice(..1, import.path.iter().cloned());
                witnesses.push(import.source.clone());
            } else if !imports.is_empty() || path.len() == 1 {
                continue;
            }
        }
        let Some((owner, name)) = graph.target_module(caller, &path) else {
            continue;
        };
        // Same-module calls retain the separately authenticated v1 consumer route.
        if owner == caller {
            continue;
        }
        let module = &graph.modules[owner];
        if unique(graph.placements.get(&(module.file, module.scope))).is_none() {
            continue;
        }
        let Some(target_file) = files.get(&module.file).filter(|file| !file.opaque) else {
            continue;
        };
        let candidates: Vec<_> = builder
            .declarations
            .iter()
            .filter(|declaration| {
                declaration.file == module.file
                    && declaration.name == name
                    && target_file
                        .facts
                        .get(&declaration.local_id)
                        .is_some_and(|fact| {
                            item_module(fact, &target_file.facts) == Some(module.scope)
                        })
            })
            .collect();
        let [target] = candidates.as_slice() else {
            continue;
        };
        if target.kind != EntityKind::Function || target.synthetic || target.rust_local {
            continue;
        }
        let Some(written) = target_file.text.text(target.span) else {
            continue;
        };
        let Some(tokens) = written
            .get(..written.find('{').unwrap_or(written.len()))
            .and_then(tokens)
        else {
            continue;
        };
        let Some(fn_at) = tokens.iter().position(|token| token == "fn") else {
            continue;
        };
        if tokens.get(fn_at + 2).map(String::as_str) != Some("(")
            || !graph.visible(visibility(&tokens[..fn_at]), owner, caller)
        {
            continue;
        }
        let definitions: Vec<_> = builder
            .occurrences
            .iter()
            .filter(|definition| {
                definition.role == OccurrenceRole::Definition
                    && definition.file == target.file
                    && definition.enclosing_declaration == Some(target.local_id)
                    && definition.name == target.name
                    && contains_span(target.span, definition.source.span())
                    && definition.source.content_hash() == target.source.content_hash()
                    && definition.source.repository() == call.source.repository()
                    && definition.source.generation() == call.source.generation()
            })
            .collect();
        let [definition] = definitions.as_slice() else {
            continue;
        };
        let Some(&symbol) = builder
            .symbol_by_declaration
            .get(&(target.file, target.local_id))
        else {
            continue;
        };
        witnesses.extend(module.witnesses.iter().cloned());
        witnesses.push(definition.source.clone());
        if witnesses.iter().any(|source| {
            builder.input_for_file(source.span().file()).is_err()
                || source.repository() != call.source.repository()
                || source.generation() != call.source.generation()
                || builder
                    .input_for_file(source.span().file())
                    .is_ok_and(|input| {
                        source.content_hash() != input.source().source_ref().content_hash()
                            || !contains_span(input.source().source_ref().span(), source.span())
                    })
        }) {
            continue;
        }
        let mut derivation = BTreeSet::new();
        for source in witnesses {
            derivation.insert(FactRef::File(source.span().file()));
            derivation.insert(FactRef::Fact(
                builder.provenance_for_file(source.span().file())?,
            ));
        }
        derivation.insert(FactRef::Entity(symbol));
        if derivation.len() > builder.request.limits().ir().max_nested_items_per_record {
            continue;
        }
        proofs.insert(
            call.source.span(),
            CallProof {
                target: symbol,
                derivation: derivation.into_iter().collect(),
            },
        );
    }
    Ok(ModuleCalls {
        proofs,
        generic_gaps: generic_gaps.into_iter().collect(),
        trait_calls: trait_candidates::resolve(builder, &files, &graph)?,
    })
}

impl Graph {
    fn new(
        builder: &ProjectFactsBuilder<'_, '_, '_>,
        files: &BTreeMap<FileId, FileSyntax<'_>>,
    ) -> Result<Self, AdapterError> {
        let mut graph = Self {
            modules: Vec::new(),
            children: BTreeMap::new(),
            placements: BTreeMap::new(),
            imports: BTreeMap::new(),
            conflicting_modules: BTreeSet::new(),
        };
        for (&file, syntax) in files {
            let path = &builder.path_by_file[&file];
            if !syntax.opaque && matches!(path.rsplit('/').next(), Some("lib.rs" | "main.rs")) {
                let root = graph.modules.len();
                graph.insert(Module {
                    file,
                    scope: syntax.root,
                    root,
                    parent: None,
                    name: None,
                    directory: path
                        .rsplit_once('/')
                        .map_or("", |(directory, _)| directory)
                        .to_owned(),
                    visibility: Visibility::Public,
                    witnesses: vec![builder.input_for_file(file)?.source().source_ref().clone()],
                });
            }
        }
        let mut index = 0;
        while index < graph.modules.len() {
            builder.cancellation.check()?;
            let parent = graph.modules[index].clone();
            if unique(graph.placements.get(&(parent.file, parent.scope))).is_none() {
                index += 1;
                continue;
            }
            let syntax = &files[&parent.file];
            for declaration in &builder.declarations {
                if declaration.file == parent.file
                    && matches!(
                        declaration.kind,
                        EntityKind::Struct
                            | EntityKind::Union
                            | EntityKind::Enum
                            | EntityKind::Trait
                            | EntityKind::TypeAlias
                            | EntityKind::TypeParameter
                    )
                    && syntax
                        .facts
                        .get(&declaration.local_id)
                        .is_some_and(|fact| item_module(fact, &syntax.facts) == Some(parent.scope))
                {
                    graph
                        .conflicting_modules
                        .insert((index, declaration.name.clone()));
                }
            }
            for fact in syntax.facts.values().filter(|fact| {
                fact.syntax_kind().as_str() == "rust.namespace_barrier.signature"
                    && item_module(fact, &syntax.facts) == Some(parent.scope)
            }) {
                if let Some(tokens) = syntax.text.text(fact.span()).and_then(tokens)
                    && let Some(at) = tokens.iter().position(|token| token == "union")
                    && let Some(name) = tokens
                        .get(at + 1)
                        .and_then(|name| canonical_rust_identifier(name, usize::MAX))
                {
                    graph.conflicting_modules.insert((index, name.into_owned()));
                }
            }
            let mut module_names = BTreeSet::new();
            for fact in syntax.facts.values().filter(|fact| {
                fact.kind() == SyntaxFactKind::Module
                    && fact.syntax_kind().as_str() == "rust.module.module"
                    && item_module(fact, &syntax.facts) == Some(parent.scope)
            }) {
                builder.cancellation.check()?;
                let Some(written) = syntax.text.text(fact.span()).and_then(tokens) else {
                    continue;
                };
                let Some(mod_at) = written.iter().position(|token| token == "mod") else {
                    continue;
                };
                let Some(name) = written
                    .get(mod_at + 1)
                    .and_then(|name| canonical_rust_identifier(name, usize::MAX))
                else {
                    continue;
                };
                if !module_names.insert(name.to_string()) {
                    graph.conflicting_modules.insert((index, name.to_string()));
                }
                let visibility = visibility(&written[..mod_at]);
                if written
                    .get(..mod_at)
                    .is_some_and(|prefix| prefix.iter().any(|token| token == "unsafe"))
                {
                    continue;
                }
                let directory = format!("{}/{}", parent.directory, name);
                let mut witnesses = parent.witnesses.clone();
                witnesses.push(super::source_for_span(
                    builder.input_for_file(parent.file)?,
                    fact.span(),
                ));
                let child = match written.get(mod_at + 2).map(String::as_str) {
                    Some("{") => Some(Module {
                        file: parent.file,
                        scope: fact.local_id(),
                        root: parent.root,
                        parent: Some(index),
                        name: Some(name.to_string()),
                        directory,
                        visibility,
                        witnesses,
                    }),
                    Some(";") if written.len() == mod_at + 3 && name.is_ascii() => {
                        let paths = [format!("{directory}.rs"), format!("{directory}/mod.rs")];
                        let targets: Vec<_> = builder
                            .path_by_file
                            .iter()
                            .filter(|(_, path)| paths.contains(path))
                            .collect();
                        if let &[(&file, _)] = targets.as_slice() {
                            files
                                .get(&file)
                                .filter(|syntax| !syntax.opaque)
                                .map(|syntax| {
                                    witnesses.push(
                                        builder
                                            .input_for_file(file)
                                            .expect("retained file has a request input")
                                            .source()
                                            .source_ref()
                                            .clone(),
                                    );
                                    Module {
                                        file,
                                        scope: syntax.root,
                                        root: parent.root,
                                        parent: Some(index),
                                        name: Some(name.to_string()),
                                        directory,
                                        visibility,
                                        witnesses,
                                    }
                                })
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(child) = child {
                    let child_index = graph.modules.len();
                    graph
                        .children
                        .entry((index, name.into_owned()))
                        .or_default()
                        .push(child_index);
                    graph.insert(child);
                }
            }
            for fact in syntax.facts.values().filter(|fact| {
                fact.kind() == SyntaxFactKind::Import
                    && item_module(fact, &syntax.facts) == Some(parent.scope)
            }) {
                if let Some(import) = syntax.text.text(fact.span()).and_then(written_use) {
                    if graph.children.contains_key(&(index, import.0.clone())) {
                        graph.conflicting_modules.insert((index, import.0.clone()));
                    }
                    graph.imports.entry(index).or_default().push(WrittenUse {
                        local: import.0,
                        path: import.1,
                        source: super::source_for_span(
                            builder.input_for_file(parent.file)?,
                            fact.span(),
                        ),
                    });
                }
            }
            index += 1;
        }
        Ok(graph)
    }

    fn insert(&mut self, module: Module) {
        self.placements
            .entry((module.file, module.scope))
            .or_default()
            .push(self.modules.len());
        self.modules.push(module);
    }

    fn target_module<'a>(&self, caller: usize, path: &'a [String]) -> Option<(usize, &'a str)> {
        let (name, mut components) = path.split_last()?;
        let mut module = caller;
        if components.first().is_some_and(|part| part == "crate") {
            module = self.modules[caller].root;
            components = components.get(1..)?;
        } else if components.first().is_some_and(|part| part == "self") {
            components = components.get(1..)?;
        }
        while components.first().is_some_and(|part| part == "super") {
            module = self.modules[module].parent?;
            components = components.get(1..)?;
        }
        for part in components {
            if self.conflicting_modules.contains(&(module, part.clone())) {
                return None;
            }
            module = *unique(self.children.get(&(module, part.clone())))?;
            if !self.visible(
                self.modules[module].visibility,
                self.modules[module].parent?,
                caller,
            ) {
                return None;
            }
        }
        self.context_is_unique(module).then_some((module, name))
    }

    fn context_is_unique(&self, module: usize) -> bool {
        let mut current = Some(module);
        for _ in 0..self.modules.len() {
            let Some(index) = current else { return true };
            let entry = &self.modules[index];
            if unique(self.placements.get(&(entry.file, entry.scope))).is_none() {
                return false;
            }
            if let Some(parent) = entry.parent {
                let Some(name) = entry.name.as_ref() else {
                    return false;
                };
                let key = (parent, name.clone());
                if self.conflicting_modules.contains(&key)
                    || unique(self.children.get(&key)).copied() != Some(index)
                {
                    return false;
                }
            }
            current = entry.parent;
        }
        current.is_none()
    }

    fn visible(&self, visibility: Visibility, owner: usize, caller: usize) -> bool {
        let ancestor = match visibility {
            Visibility::Public => return self.modules[owner].root == self.modules[caller].root,
            Visibility::Private => owner,
            Visibility::Parent => match self.modules[owner].parent {
                Some(parent) => parent,
                None => owner,
            },
            Visibility::Unavailable => return false,
        };
        let mut current = Some(caller);
        for _ in 0..self.modules.len() {
            let Some(module) = current else { break };
            if module == ancestor {
                return true;
            }
            current = self.modules[module].parent;
        }
        false
    }
}

fn unique<T>(values: Option<&Vec<T>>) -> Option<&T> {
    match values?.as_slice() {
        [value] => Some(value),
        _ => None,
    }
}

fn complete_source_root(root: SourceSpan, input: &super::ProjectSourceInput<'_>) -> bool {
    let full = input.source().source_ref().span();
    if root == full {
        return true;
    }
    if full.start_byte() != 0 || root.file() != full.file() || root.end_byte() != full.end_byte() {
        return false;
    }
    // The parser excludes an admitted BOM from its source-file extent. Every
    // other physical byte must remain in the complete written context.
    let prefix: &[u8] = match input.encoding().as_str() {
        "utf-8" => &[0xef, 0xbb, 0xbf],
        "utf-16le" => &[0xff, 0xfe],
        "utf-16be" => &[0xfe, 0xff],
        _ => return false,
    };
    usize::try_from(root.start_byte()).ok() == Some(prefix.len())
        && input.source().bytes().starts_with(prefix)
}

fn full_context_reaches_root(
    context: &SyntaxFact,
    root: &SyntaxFact,
    facts: &[SyntaxFact],
) -> bool {
    // Equal-extent captures may parent the whole-file signature through the sole
    // item. Only an uninterrupted full-file chain can authenticate that context.
    let mut parent = context.parent();
    for _ in 0..facts.len() {
        let Some(current) = parent.and_then(|id| facts.iter().find(|fact| fact.local_id() == id))
        else {
            return false;
        };
        if current.span() != root.span() {
            return false;
        }
        if current.local_id() == root.local_id() {
            return true;
        }
        parent = current.parent();
    }
    false
}

fn item_module(fact: &SyntaxFact, facts: &BTreeMap<u64, &SyntaxFact>) -> Option<u64> {
    let mut parent = fact.parent();
    for _ in 0..facts.len() {
        let current = facts.get(&parent?)?;
        if matches!(
            current.kind(),
            SyntaxFactKind::Root | SyntaxFactKind::Module
        ) {
            return Some(current.local_id());
        }
        if matches!(
            current.kind(),
            SyntaxFactKind::Declaration | SyntaxFactKind::Scope
        ) {
            return None;
        }
        parent = current.parent();
    }
    None
}

fn containing_module(
    fact: &SyntaxFact,
    file: &FileSyntax<'_>,
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<Option<u64>, AdapterError> {
    let mut parent = fact.parent();
    for _ in 0..file.facts.len() {
        builder.cancellation.check()?;
        let Some(current) = parent.and_then(|id| file.facts.get(&id)) else {
            return Ok(None);
        };
        if matches!(
            current.kind(),
            SyntaxFactKind::Root | SyntaxFactKind::Module
        ) {
            return Ok(Some(current.local_id()));
        }
        if matches!(
            current.syntax_kind().as_str(),
            "rust.impl.scope" | "rust.trait.declaration"
        ) {
            return Ok(None);
        }
        if current.syntax_kind().as_str() == "rust.function.declaration"
            && file
                .text
                .text(current.span())
                .and_then(tokens)
                .is_none_or(|tokens| {
                    tokens
                        .iter()
                        .position(|token| token == "fn")
                        .and_then(|at| tokens.get(at + 2))
                        .is_none_or(|token| token != "(")
                })
        {
            return Ok(None);
        }
        parent = current.parent();
    }
    Ok(None)
}

fn module_item_named(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    file: &FileSyntax<'_>,
    scope: u64,
    name: &str,
) -> bool {
    builder.declarations.iter().any(|declaration| {
        declaration.name == name
            && file.facts.get(&declaration.local_id).is_some_and(|fact| {
                fact.span().file() == declaration.file
                    && item_module(fact, &file.facts) == Some(scope)
            })
    })
}

fn visibility(prefix: &[String]) -> Visibility {
    if prefix.is_empty()
        || prefix
            .iter()
            .all(|token| matches!(token.as_str(), "async" | "unsafe"))
    {
        return Visibility::Private;
    }
    match prefix
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["pub"] | ["pub", "async"] | ["pub", "unsafe"] | ["pub", "(", "crate", ")"] => {
            Visibility::Public
        }
        ["pub", "(", "self", ")"] => Visibility::Private,
        ["pub", "(", "super", ")"] => Visibility::Parent,
        _ => Visibility::Unavailable,
    }
}

fn written_use(text: &str) -> Option<(String, Vec<String>)> {
    let mut parts = tokens(text)?;
    if parts.first()?.as_str() != "use" || parts.pop()?.as_str() != ";" {
        return None;
    }
    parts.remove(0);
    let local = if parts.len() >= 3 && parts.get(parts.len() - 2)?.as_str() == "as" {
        let alias = parts.pop()?;
        parts.pop();
        canonical_rust_identifier(&alias, usize::MAX)?.into_owned()
    } else {
        canonical_rust_identifier(parts.last()?, usize::MAX)?.into_owned()
    };
    let path = path_parts(&parts)?;
    // Bare external crate paths require Cargo dependency/edition context.
    if !matches!(path.first()?.as_str(), "crate" | "self" | "super") {
        return None;
    }
    Some((local, path))
}

fn path_tokens(text: &str) -> Option<Vec<String>> {
    path_parts(&tokens(text)?)
}

fn generic_callee_suffix(tokens: &[String]) -> bool {
    if !tokens
        .iter()
        .take(3)
        .map(String::as_str)
        .eq([":", ":", "<"])
    {
        return false;
    }
    let mut depth = 1usize;
    for (index, token) in tokens.iter().enumerate().skip(3) {
        match token.as_str() {
            "<" => depth = depth.saturating_add(1),
            ">" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return tokens.get(index + 1).is_some_and(|token| token == "(");
                }
            }
            _ => {}
        }
    }
    false
}

/// Retains canonical Rust path names while callers keep authored physical spans.
pub(super) fn path_name(text: &str, maximum_bytes: usize) -> Option<(String, String)> {
    if text.len() > maximum_bytes {
        return None;
    }
    let mut path = path_tokens(text)?;
    let name = path.pop()?;
    (!path.is_empty()).then(|| (path.join("::"), name))
}

fn path_parts(tokens: &[String]) -> Option<Vec<String>> {
    if tokens.len().checked_add(2)?.checked_rem(3)? != 0 {
        return None;
    }
    let mut path = Vec::new();
    let mut parts = tokens.iter();
    loop {
        let Some(name) = parts.next() else {
            return (!path.is_empty()).then_some(path);
        };
        path.push(canonical_rust_identifier(name, usize::MAX)?.into_owned());
        match (parts.next(), parts.next()) {
            (None, None) => return Some(path),
            (Some(left), Some(right)) if left == ":" && right == ":" => {}
            _ => return None,
        }
    }
}

fn tokens(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < text.len() {
        let remaining = text.get(position..)?;
        if remaining.starts_with("//") {
            position += remaining.find('\n').unwrap_or(remaining.len());
            continue;
        }
        if let Some(comment) = remaining.strip_prefix("/*") {
            // Nested comments require the parser's byte extents, not a partial lexer.
            let end = comment.find("*/")?;
            if comment.get(..end)?.contains("/*") {
                return None;
            }
            position += end + 4;
            continue;
        }
        let character = remaining.chars().next()?;
        if character.is_whitespace() {
            position += character.len_utf8();
            continue;
        }
        let raw = remaining.starts_with("r#");
        let identifier = if raw { remaining.get(2..)? } else { remaining };
        if identifier.chars().next().is_some_and(|character| {
            character == '_'
                || canonical_rust_identifier(&character.to_string(), usize::MAX).is_some()
        }) {
            let end = identifier
                .char_indices()
                .find(|(_, character)| {
                    (character.is_whitespace() || character.is_ascii())
                        && !character.is_alphanumeric()
                        && *character != '_'
                })
                .map_or(identifier.len(), |(offset, _)| offset);
            let size = end + if raw { 2 } else { 0 };
            tokens.push(remaining.get(..size)?.to_owned());
            position += size;
        } else {
            tokens.push(character.to_string());
            position += character.len_utf8();
        }
    }
    Some(tokens)
}
