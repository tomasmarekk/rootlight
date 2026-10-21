//! Possible implementations for a written, ordinary `&dyn Trait` parameter.
//! The module graph identifies the trait; native owners and current source
//! identify implementations. This never proves runtime dispatch completeness.

use super::*;

pub(in crate::project_semantics) struct TraitCallProof {
    pub(in crate::project_semantics) symbols: Vec<SymbolId>,
    pub(in crate::project_semantics) derivation: Vec<FactRef>,
}

/// A method declaration is not a competing module-level name. Unknown nested
/// declarations still poison the module graph instead of becoming free items.
pub(super) fn direct_member(
    fact: &SyntaxFact,
    facts: &BTreeMap<u64, &SyntaxFact>,
    text: &RustSourceText<'_>,
) -> bool {
    fact.syntax_kind().as_str() == "rust.function.declaration"
        && text
            .text(fact.span())
            .is_some_and(|source| source.starts_with("fn ") || source.starts_with("pub fn "))
        && fact
            .parent()
            .and_then(|parent| facts.get(&parent))
            .is_some_and(|owner| {
                matches!(
                    owner.syntax_kind().as_str(),
                    "rust.impl.scope" | "rust.trait.declaration"
                ) && contains_span(owner.span(), fact.span())
                    && text.text(owner.span()).is_some_and(|source| {
                        source.starts_with("impl ")
                            || source.starts_with("trait ")
                            || source.starts_with("pub trait ")
                    })
            })
}

pub(super) fn resolve(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    files: &BTreeMap<FileId, FileSyntax<'_>>,
    graph: &Graph,
) -> Result<BTreeMap<SourceSpan, TraitCallProof>, AdapterError> {
    let mut output = BTreeMap::new();
    for call in builder.occurrences.iter().filter(|call| {
        call.role == OccurrenceRole::CallSite && call.syntax_kind == "rust.call.call"
    }) {
        builder.cancellation.check()?;
        let Some(file) = files.get(&call.file).filter(|file| !file.opaque) else {
            continue;
        };
        let Some(fact) = one(file.facts.values().copied().filter(|fact| {
            fact.span() == call.source.span() && fact.syntax_kind().as_str() == call.syntax_kind
        })) else {
            continue;
        };
        let Some(callee) = file.text.text(fact.span()) else {
            continue;
        };
        let Some(parts) = lex(callee, builder.cancellation)? else {
            continue;
        };
        let [receiver, ".", method] = parts.as_slice() else {
            continue;
        };
        if !lexed_identifier(receiver) || !lexed_identifier(method) {
            continue;
        }
        let Some(tail) = SourceSpan::new(
            call.file,
            fact.span().end_byte(),
            file.facts[&file.root].span().end_byte(),
        )
        .ok()
        .and_then(|span| file.text.text(span)) else {
            continue;
        };
        if !empty_arguments(tail, builder.cancellation)? {
            continue;
        }
        let probe = SyntaxFact::new(
            fact.local_id(),
            fact.parent(),
            SyntaxFactKind::Occurrence,
            fact.span(),
            fact.depth(),
            SyntaxKindLabel::new("rust.value.reference")
                .map_err(|_| provider_failure("rust-trait-receiver-kind"))?,
        );
        let RustBindingLookup::Definition(definition_id) =
            file.bindings
                .lookup(&probe, receiver, builder.cancellation)?
        else {
            continue;
        };
        let Some(definition) = file
            .facts
            .get(&definition_id)
            .copied()
            .filter(|definition| definition.syntax_kind().as_str() == "rust.parameter.definition")
        else {
            continue;
        };
        let Some(parameter) = definition
            .parent()
            .and_then(|id| file.facts.get(&id))
            .copied()
            .filter(|parameter| {
                parameter.syntax_kind().as_str() == "rust.parameter.declaration"
                    && parameter.span() == definition.span()
            })
        else {
            continue;
        };
        if file
            .facts
            .values()
            .filter(|fact| {
                fact.span() == parameter.span()
                    && matches!(fact.kind(), SyntaxFactKind::Declaration)
            })
            .count()
            != 1
            || file.text.text(definition.span()) != Some(*receiver)
        {
            continue;
        }
        let Some(function) = ancestor_function(parameter, file) else {
            continue;
        };
        if ancestor_function(fact, file).map(SyntaxFact::local_id) != Some(function.local_id()) {
            continue;
        }
        let Some(header) = function_header(function, file) else {
            continue;
        };
        let Some(header_tokens) = lex(header, builder.cancellation)? else {
            continue;
        };
        let header_tokens = header_tokens
            .strip_prefix(&["pub"])
            .unwrap_or(&header_tokens);
        let [
            "fn",
            function_name,
            "(",
            parameter_name,
            ":",
            "&",
            "dyn",
            trait_name,
            ")",
        ] = header_tokens
        else {
            continue;
        };
        if !lexed_identifier(function_name)
            || parameter_name != receiver
            || !lexed_identifier(trait_name)
        {
            continue;
        }
        let Some(scope) = containing_module(function, file, builder)? else {
            continue;
        };
        let Some(&caller) = unique(graph.placements.get(&(call.file, scope))) else {
            continue;
        };
        let Some(trait_item) =
            named_item(builder, files, graph, caller, trait_name, EntityKind::Trait)?
        else {
            continue;
        };
        let trait_file = &files[&trait_item.file];
        let Some(trait_text) = trait_file.text.text(trait_item.span) else {
            continue;
        };
        if !trait_declares(trait_text, trait_item.name, method, builder.cancellation)? {
            continue;
        }

        let mut implementations = BTreeMap::<SymbolId, Vec<(SymbolId, FileId)>>::new();
        for (&implementation_file, syntax) in files.iter().filter(|(_, syntax)| !syntax.opaque) {
            for implementation in syntax
                .facts
                .values()
                .copied()
                .filter(|fact| fact.syntax_kind().as_str() == "rust.impl.scope")
            {
                builder.cancellation.check()?;
                let Some(trait_capture) = one(syntax.facts.values().copied().filter(|fact| {
                    fact.parent() == Some(implementation.local_id())
                        && fact.syntax_kind().as_str().ends_with(".scope_trait")
                })) else {
                    continue;
                };
                let Some(self_capture) = one(syntax.facts.values().copied().filter(|fact| {
                    fact.parent() == Some(implementation.local_id())
                        && fact.syntax_kind().as_str().ends_with(".scope_type")
                })) else {
                    continue;
                };
                if trait_capture.syntax_kind().as_str() != "rust.impl_trait.scope_trait"
                    || self_capture.syntax_kind().as_str() != "rust.impl_type.scope_type"
                {
                    continue;
                }
                let Some(prefix) = SourceSpan::new(
                    implementation_file,
                    implementation.span().start_byte(),
                    self_capture.span().end_byte(),
                )
                .ok()
                .and_then(|span| syntax.text.text(span)) else {
                    continue;
                };
                let Some(prefix) = lex(prefix, builder.cancellation)? else {
                    continue;
                };
                let ["impl", written_trait, "for", written_type] = prefix.as_slice() else {
                    continue;
                };
                if syntax.text.text(trait_capture.span()) != Some(*written_trait)
                    || syntax.text.text(self_capture.span()) != Some(*written_type)
                    || !lexed_identifier(written_trait)
                    || !lexed_identifier(written_type)
                {
                    continue;
                }
                let Some(scope) = containing_module(implementation, syntax, builder)? else {
                    continue;
                };
                let Some(&owner) = unique(graph.placements.get(&(implementation_file, scope)))
                else {
                    continue;
                };
                let Some(resolved_trait) = named_item(
                    builder,
                    files,
                    graph,
                    owner,
                    written_trait,
                    EntityKind::Trait,
                )?
                else {
                    continue;
                };
                if resolved_trait.symbol != trait_item.symbol {
                    continue;
                }
                let Some(self_type) = named_item(
                    builder,
                    files,
                    graph,
                    owner,
                    written_type,
                    EntityKind::Struct,
                )?
                else {
                    continue;
                };
                // Keep every authored impl, including a competing one with no
                // method. Withholding a method cannot make duplicate impls unique.
                let mut methods = Vec::new();
                for declaration in builder.declarations.iter().filter(|declaration| {
                    declaration.file == implementation_file
                        && declaration.name == *method
                        && declaration.kind == EntityKind::Method
                }) {
                    builder.cancellation.check()?;
                    let Some(raw) =
                        syntax
                            .facts
                            .get(&declaration.local_id)
                            .copied()
                            .filter(|raw| {
                                raw.parent() == Some(implementation.local_id())
                                    && direct_member(raw, &syntax.facts, &syntax.text)
                            })
                    else {
                        continue;
                    };
                    let Some(header) = function_header(raw, syntax) else {
                        continue;
                    };
                    let Some(parts) = lex(header, builder.cancellation)? else {
                        continue;
                    };
                    if parts.as_slice() != ["fn", *method, "(", "&", "self", ")"] {
                        continue;
                    }
                    let Some(definition) = one(syntax.facts.values().copied().filter(|fact| {
                        fact.parent() == Some(raw.local_id())
                            && fact.kind() == SyntaxFactKind::Occurrence
                            && fact.syntax_kind().as_str().ends_with(".definition")
                    }))
                    .filter(|definition| {
                        definition.syntax_kind().as_str() == "rust.identifier.definition"
                            && syntax.text.text(definition.span()) == Some(*method)
                            && contains_span(raw.span(), definition.span())
                    }) else {
                        continue;
                    };
                    let _ = definition;
                    if let Some(symbol) = builder
                        .symbol_by_declaration
                        .get(&(implementation_file, declaration.local_id))
                    {
                        methods.push((*symbol, implementation_file));
                    }
                }
                let entry = implementations.entry(self_type.symbol).or_default();
                // A sentinel preserves each invalid or absent method as a refusal.
                if let [(symbol, file)] = methods.as_slice() {
                    entry.push((*symbol, *file));
                } else {
                    entry.push((self_type.symbol, implementation_file));
                }
            }
        }
        let mut symbols = BTreeSet::new();
        let mut evidence_files = BTreeSet::from([call.file, trait_item.file]);
        for methods in implementations.values() {
            if let [(symbol, file)] = methods.as_slice()
                && builder
                    .entities
                    .iter()
                    .any(|entity| entity.symbol == *symbol && entity.kind == EntityKind::Method)
            {
                symbols.insert(*symbol);
                evidence_files.insert(*file);
            }
        }
        if symbols.is_empty() {
            continue;
        }
        let mut derivation = BTreeSet::from([FactRef::Entity(trait_item.symbol)]);
        for file in evidence_files {
            derivation.insert(FactRef::File(file));
            derivation.insert(FactRef::Fact(builder.provenance_for_file(file)?));
        }
        derivation.extend(symbols.iter().copied().map(FactRef::Entity));
        if derivation.len() > builder.request.limits().ir().max_nested_items_per_record {
            continue;
        }
        output.insert(
            call.source.span(),
            TraitCallProof {
                symbols: symbols.into_iter().collect(),
                derivation: derivation.into_iter().collect(),
            },
        );
    }
    Ok(output)
}

struct Item<'a> {
    file: FileId,
    name: &'a str,
    span: SourceSpan,
    symbol: SymbolId,
}

fn named_item<'a>(
    builder: &'a ProjectFactsBuilder<'_, '_, '_>,
    files: &BTreeMap<FileId, FileSyntax<'_>>,
    graph: &Graph,
    caller: usize,
    name: &str,
    kind: EntityKind,
) -> Result<Option<Item<'a>>, AdapterError> {
    builder.cancellation.check()?;
    if !graph.context_is_unique(caller) {
        return Ok(None);
    }
    let module = &graph.modules[caller];
    let Some(file) = files.get(&module.file).filter(|file| !file.opaque) else {
        return Ok(None);
    };
    let imports: Vec<_> = graph
        .imports
        .get(&caller)
        .into_iter()
        .flatten()
        .filter(|import| import.local == name)
        .collect();
    let mut path = vec![name.to_owned()];
    if let [import] = imports.as_slice() {
        if module_item_named(builder, file, module.scope, name)
            || graph.children.contains_key(&(caller, name.to_owned()))
        {
            return Ok(None);
        }
        path = import.path.clone();
    } else if !imports.is_empty() {
        return Ok(None);
    }
    let Some((owner, name)) = graph.target_module(caller, &path) else {
        return Ok(None);
    };
    let module = &graph.modules[owner];
    let Some(file) = files.get(&module.file).filter(|file| !file.opaque) else {
        return Ok(None);
    };
    let Some(declaration) = one(builder.declarations.iter().filter(|declaration| {
        declaration.file == module.file
            && declaration.name == name
            && file
                .facts
                .get(&declaration.local_id)
                .is_some_and(|fact| item_module(fact, &file.facts) == Some(module.scope))
    }))
    .filter(|declaration| {
        declaration.kind == kind && !declaration.synthetic && !declaration.rust_local
    }) else {
        return Ok(None);
    };
    let Some(text) = file.text.text(declaration.span) else {
        return Ok(None);
    };
    let Some(parts) = lex_up_to(text, builder.cancellation, 5)? else {
        return Ok(None);
    };
    let at = usize::from(parts.first() == Some(&"pub"));
    let keyword = if kind == EntityKind::Trait {
        "trait"
    } else {
        "struct"
    };
    if parts.get(at) != Some(&keyword)
        || parts.get(at + 1) != Some(&name)
        || !matches!(parts.get(at + 2), Some(&"{" | &";"))
    {
        return Ok(None);
    }
    let mut authored_names = 0usize;
    for raw in file.facts.values().copied().filter(|raw| {
        raw.kind() == SyntaxFactKind::Declaration
            && item_module(raw, &file.facts) == Some(module.scope)
    }) {
        builder.cancellation.check()?;
        let Some(raw_text) = file.text.text(raw.span()) else {
            return Ok(None);
        };
        let Some(parts) = lex_up_to(raw_text, builder.cancellation, 3)? else {
            return Ok(None);
        };
        let at = usize::from(parts.first() == Some(&"pub"));
        if !matches!(
            parts.get(at),
            Some(&"fn" | &"trait" | &"struct" | &"enum" | &"type" | &"const" | &"static")
        ) {
            return Ok(None);
        }
        if parts.get(at + 1) == Some(&name) {
            authored_names += 1;
        }
    }
    if authored_names != 1 {
        return Ok(None);
    }
    if !graph.visible(
        visibility(
            &parts[..at]
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>(),
        ),
        owner,
        caller,
    ) {
        return Ok(None);
    }
    let Some(raw) = file.facts.get(&declaration.local_id) else {
        return Ok(None);
    };
    let Some(definition) = one(file.facts.values().copied().filter(|fact| {
        fact.parent() == Some(raw.local_id())
            && fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str().ends_with(".definition")
    }))
    .filter(|definition| file.text.text(definition.span()) == Some(name)) else {
        return Ok(None);
    };
    let _ = definition;
    Ok(builder
        .symbol_by_declaration
        .get(&(module.file, declaration.local_id))
        .map(|symbol| Item {
            file: module.file,
            name: &declaration.name,
            span: declaration.span,
            symbol: *symbol,
        }))
}

fn ancestor_function<'a>(fact: &SyntaxFact, file: &'a FileSyntax<'_>) -> Option<&'a SyntaxFact> {
    let mut parent = fact.parent();
    for _ in 0..file.facts.len() {
        let owner = file.facts.get(&parent?)?;
        if owner.kind() == SyntaxFactKind::Declaration {
            return (owner.syntax_kind().as_str() == "rust.function.declaration").then_some(*owner);
        }
        parent = owner.parent();
    }
    None
}

fn function_header<'a>(function: &SyntaxFact, file: &'a FileSyntax<'_>) -> Option<&'a str> {
    let signature = one(file.facts.values().copied().filter(|fact| {
        fact.syntax_kind().as_str() == "rust.parameters.signature"
            && ancestor_function(fact, file).map(SyntaxFact::local_id) == Some(function.local_id())
    }))?;
    (signature.span().start_byte() == function.span().start_byte()
        && contains_span(function.span(), signature.span()))
    .then(|| file.text.text(signature.span()))
    .flatten()
}

fn trait_declares(
    text: &str,
    name: &str,
    method: &str,
    cancellation: &rootlight_cancel::Cancellation,
) -> Result<bool, AdapterError> {
    let Some(parts) = lex(text, cancellation)? else {
        return Ok(false);
    };
    let parts = parts.strip_prefix(&["pub"]).unwrap_or(&parts);
    let ["trait", declared, "{", rest @ ..] = parts else {
        return Ok(false);
    };
    if *declared != name {
        return Ok(false);
    }
    let Some(mut rest) = rest.strip_suffix(&["}"]) else {
        return Ok(false);
    };
    let mut methods = BTreeSet::new();
    while !rest.is_empty() {
        cancellation.check()?;
        let ["fn", name, "(", "&", "self", ")", ";", tail @ ..] = rest else {
            return Ok(false);
        };
        if !lexed_identifier(name) || !methods.insert(*name) {
            return Ok(false);
        }
        rest = tail;
    }
    Ok(methods.contains(method))
}

fn lexed_identifier(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && value != "_"
}

fn one<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

fn empty_arguments(
    text: &str,
    cancellation: &rootlight_cancel::Cancellation,
) -> Result<bool, AdapterError> {
    let mut wanted = ['(', ')'].into_iter();
    let mut expected = wanted.next();
    for (index, character) in text.chars().enumerate() {
        super::super::check_periodically(index, cancellation)?;
        if character.is_whitespace() {
            continue;
        }
        if Some(character) != expected {
            return Ok(false);
        }
        expected = wanted.next();
        if expected.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lex<'a>(
    text: &'a str,
    cancellation: &rootlight_cancel::Cancellation,
) -> Result<Option<Vec<&'a str>>, AdapterError> {
    lex_up_to(text, cancellation, usize::MAX)
}

fn lex_up_to<'a>(
    text: &'a str,
    cancellation: &rootlight_cancel::Cancellation,
    maximum_tokens: usize,
) -> Result<Option<Vec<&'a str>>, AdapterError> {
    let mut output = Vec::new();
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        super::super::check_periodically(index, cancellation)?;
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if !bytes[index].is_ascii() || matches!(bytes[index], b'"' | b'\'' | b'/' | b'#') {
            return Ok(None);
        }
        let start = index;
        index += 1;
        if bytes[start].is_ascii_alphabetic() || bytes[start] == b'_' {
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                super::super::check_periodically(index, cancellation)?;
                index += 1;
            }
        }
        output
            .try_reserve(1)
            .map_err(|_| provider_failure("rust-trait-token-allocation"))?;
        output.push(&text[start..index]);
        if output.len() == maximum_tokens {
            break;
        }
    }
    cancellation.check()?;
    Ok(Some(output))
}
