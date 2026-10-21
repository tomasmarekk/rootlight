//! Static test relevance inherited from an explicitly owned Rust module import.
//! This proof does not expand macros, resolve their operands, or establish that
//! an imported item is executed. The call graph retains its stricter policy.

use super::*;
use crate::project_semantics::{
    EvidenceKind, FactDomain, FactEvidence, IrRecord, RelationEndpoint, RelationPredicate,
    RelationRecord, SyntaxFactKind, confidence, derive_relation_record_id,
};

pub(in crate::project_semantics) struct TestImport {
    test: SymbolId,
    target: SymbolId,
    source: SourceRef,
    derivation: Vec<FactRef>,
}

pub(super) fn admits_file(
    file: &FileSyntax<'_>,
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<bool, AdapterError> {
    let mut attributes = BTreeSet::new();
    for fact in file.facts.values() {
        builder.cancellation.check()?;
        let kind = fact.syntax_kind().as_str();
        if kind == "rust.attribute.signature" {
            let Some(text) = file.text.text(fact.span()) else {
                return Ok(false);
            };
            if text.len() > builder.request.limits().ir().max_string_bytes.min(4096)
                || !known_attribute(text)
                || !attributes.insert(fact.span().start_byte())
            {
                return Ok(false);
            }
        } else if kind.contains("macro") {
            if !in_function(fact, file) {
                return Ok(false);
            }
        } else if kind.contains("extern_crate")
            || kind.contains("foreign")
            || fact.kind() == SyntaxFactKind::Import
                && (item_module(fact, &file.facts).is_none()
                    || file.text.text(fact.span()).and_then(written_use).is_none())
            || kind == "rust.namespace_barrier.signature"
                && file.text.text(fact.span()).is_some_and(|text| {
                    text.starts_with("extern") || text.starts_with("macro_rules")
                })
        {
            return Ok(false);
        }
    }
    let encoding = builder
        .input_for_file(file.facts[&file.root].span().file())?
        .encoding();
    let utf16 = matches!(encoding.as_str(), "utf-16le" | "utf-16be");
    let mut physical = if utf16 { 2u64 } else { 0 };
    // A withheld attribute capture must not hide an activation/expansion
    // barrier. Unmodeled hashes, including those in strings/comments, refuse
    // this narrow static proof rather than requiring another Rust lexer.
    for (index, character) in file.text.as_str().chars().enumerate() {
        super::super::check_periodically(index, builder.cancellation)?;
        if character == '#' && !attributes.contains(&physical) {
            return Ok(false);
        }
        let width = if utf16 {
            character.len_utf16() * 2
        } else {
            character.len_utf8()
        };
        physical = physical
            .checked_add(
                u64::try_from(width).map_err(|_| super::super::SinkError::AccountingOverflow)?,
            )
            .ok_or(super::super::SinkError::AccountingOverflow)?;
    }
    Ok(true)
}

fn known_attribute(text: &str) -> bool {
    let Some(parts) = tokens(text) else {
        return false;
    };
    let names: Vec<_> = parts.iter().map(String::as_str).collect();
    matches!(
        names.as_slice(),
        ["#", "[", "test", "]"]
            | ["#", "[", "cfg", "(", "test", ")", "]"]
            | ["#", "[", "derive", "(", "Clone", ",", "Copy", ")", "]"]
    ) || path_attribute(text).is_some()
}

fn path_attribute(text: &str) -> Option<&str> {
    let text = text.trim().strip_prefix("#[")?.trim_start();
    let text = text
        .strip_prefix("path")?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let text = text.strip_prefix('"')?;
    let (path, tail) = text.split_once('"')?;
    (tail.trim() == "]"
        && !path.is_empty()
        && path.len() <= 4096
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte)))
    .then_some(path)
}

fn in_function(fact: &SyntaxFact, file: &FileSyntax<'_>) -> bool {
    let mut parent = fact.parent();
    for _ in 0..file.facts.len() {
        let Some(owner) = parent.and_then(|id| file.facts.get(&id)) else {
            return false;
        };
        if owner.syntax_kind().as_str() == "rust.function.declaration" {
            return item_module(owner, &file.facts).is_some();
        }
        if matches!(owner.kind(), SyntaxFactKind::Module | SyntaxFactKind::Root) {
            return false;
        }
        parent = owner.parent();
    }
    false
}

pub(super) fn module_paths(
    file: &FileSyntax<'_>,
    module: &SyntaxFact,
    parent: &Module,
    directory: &str,
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<Vec<String>, AdapterError> {
    let mut end = module.span().start_byte();
    let mut path = None;
    loop {
        builder.cancellation.check()?;
        let mut prior = None;
        for fact in file.facts.values() {
            builder.cancellation.check()?;
            if fact.syntax_kind().as_str() == "rust.attribute.signature"
                && fact.span().end_byte() <= end
                && prior.is_none_or(|prior: &SyntaxFact| {
                    prior.span().end_byte() < fact.span().end_byte()
                })
            {
                prior = Some(*fact);
            }
        }
        let Some(prior) = prior else { break };
        let gap = SourceSpan::new(module.span().file(), prior.span().end_byte(), end)
            .map_err(|_| provider_failure("rust-test-import-attribute-span"))?;
        let Some(gap) = file.text.text(gap) else {
            return Ok(Vec::new());
        };
        let mut whitespace = true;
        for (index, character) in gap.chars().enumerate() {
            super::super::check_periodically(index, builder.cancellation)?;
            if !character.is_whitespace() {
                whitespace = false;
                break;
            }
        }
        if !whitespace {
            break;
        }
        let Some(text) = file.text.text(prior.span()) else {
            return Ok(Vec::new());
        };
        if let Some(value) = path_attribute(text)
            && path.replace(value).is_some()
        {
            return Ok(Vec::new());
        }
        end = prior.span().start_byte();
    }
    let Some(path) = path else {
        return Ok(vec![
            format!("{directory}.rs"),
            format!("{directory}/mod.rs"),
        ]);
    };
    // Inline module path bases have additional language rules. This finite
    // route accepts overrides only in an independently authenticated file root.
    if parent.scope != file.root || path.starts_with('/') {
        return Ok(Vec::new());
    }
    let physical_path = &builder.path_by_file[&parent.file];
    let base = physical_path.rsplit_once('/').map_or("", |(base, _)| base);
    let mut components: Vec<_> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => return Ok(Vec::new()),
            ".." => {
                if components.pop().is_none() {
                    return Ok(Vec::new());
                }
            }
            part => components.push(part),
        }
    }
    Ok(vec![components.join("/")])
}

pub(super) fn resolve(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    files: &BTreeMap<FileId, FileSyntax<'_>>,
) -> Result<Vec<TestImport>, AdapterError> {
    if !builder
        .declarations
        .iter()
        .any(|declaration| declaration.is_test)
    {
        return Ok(Vec::new());
    }
    let graph = Graph::with_policy(builder, files, true)?;
    let mut proofs = Vec::new();
    for (&module, imports) in &graph.imports {
        builder.cancellation.check()?;
        if !graph.context_is_unique(module) {
            continue;
        }
        let owner = &graph.modules[module];
        let file = &files[&owner.file];
        for import in imports {
            builder.cancellation.check()?;
            if imports
                .iter()
                .filter(|other| other.local == import.local)
                .count()
                != 1
                || module_item_named(builder, file, owner.scope, &import.local)
            {
                continue;
            }
            let Some((target_module, name)) = graph.target_module(module, &import.path) else {
                continue;
            };
            let target_owner = &graph.modules[target_module];
            let target_file = &files[&target_owner.file];
            let mut matches = Vec::new();
            for fact in target_file.facts.values().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && item_module(fact, &target_file.facts) == Some(target_owner.scope)
            }) {
                builder.cancellation.check()?;
                if let Some(header) = header(target_file, fact, builder)?
                    && header.name == name
                {
                    matches.push((fact, header));
                }
            }
            let [(target_fact, header)] = matches.as_slice() else {
                continue;
            };
            let definitions: Vec<_> = target_file
                .facts
                .values()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Occurrence
                        && fact.syntax_kind().as_str().ends_with(".definition")
                        && fact.parent() == Some(target_fact.local_id())
                })
                .collect();
            let [definition] = definitions.as_slice() else {
                continue;
            };
            if definition.span() != header.span
                || target_file.text.text(definition.span()) != Some(name)
            {
                continue;
            }
            let Some(target) = builder
                .symbol_by_declaration
                .get(&(target_owner.file, target_fact.local_id()))
                .copied()
            else {
                continue;
            };
            let Some(declaration) = builder.declarations.iter().find(|declaration| {
                declaration.file == target_owner.file
                    && declaration.local_id == target_fact.local_id()
                    && declaration.name == name
                    && declaration.kind == header.kind
                    && !declaration.synthetic
            }) else {
                continue;
            };
            let Some(prefix) = target_file.text.text(
                SourceSpan::new(
                    target_owner.file,
                    target_fact.span().start_byte(),
                    definition.span().start_byte(),
                )
                .map_err(|_| provider_failure("rust-test-import-target-span"))?,
            ) else {
                continue;
            };
            let Some(prefix) = tokens(prefix) else {
                continue;
            };
            let Some((_, visibility_prefix)) = prefix.split_last() else {
                continue;
            };
            if !graph.visible(visibility(visibility_prefix), target_module, module) {
                continue;
            }
            for test in builder
                .declarations
                .iter()
                .filter(|test| test.file == owner.file && test.is_test)
            {
                builder.cancellation.check()?;
                let Some(test_fact) = file.facts.get(&test.local_id) else {
                    continue;
                };
                if item_module(test_fact, &file.facts) != Some(owner.scope) {
                    continue;
                }
                let Some(test_symbol) = builder
                    .symbol_by_declaration
                    .get(&(owner.file, test.local_id))
                    .copied()
                else {
                    continue;
                };
                let mut derivation = BTreeSet::new();
                for witness in owner.witnesses.iter().chain(&target_owner.witnesses) {
                    derivation.insert(FactRef::File(witness.span().file()));
                    derivation.insert(FactRef::Fact(
                        builder.provenance_for_file(witness.span().file())?,
                    ));
                }
                derivation.insert(FactRef::Entity(test_symbol));
                derivation.insert(FactRef::Entity(target));
                derivation.insert(FactRef::File(declaration.file));
                if derivation.len() > builder.request.limits().ir().max_nested_items_per_record {
                    continue;
                }
                proofs.push(TestImport {
                    test: test_symbol,
                    target,
                    source: import.source.clone(),
                    derivation: derivation.into_iter().collect(),
                });
            }
        }
    }
    Ok(proofs)
}

struct Header {
    name: String,
    span: SourceSpan,
    kind: EntityKind,
}

fn header(
    file: &FileSyntax<'_>,
    fact: &SyntaxFact,
    builder: &ProjectFactsBuilder<'_, '_, '_>,
) -> Result<Option<Header>, AdapterError> {
    let Some(text) = file.text.text(fact.span()) else {
        return Ok(None);
    };
    let maximum = builder.request.limits().ir().max_string_bytes.min(4096);
    let mut words = Vec::new();
    let mut start = None;
    for (at, character) in text.char_indices() {
        super::super::check_periodically(at, builder.cancellation)?;
        if at > maximum {
            return Ok(None);
        }
        if character.is_alphanumeric() || character == '_' {
            start.get_or_insert(at);
        } else {
            if let Some(begin) = start.take() {
                words.push((begin, at));
            }
            if !character.is_whitespace() {
                if matches!(character, '{' | '(' | '=' | ';' | '<' | ':') {
                    break;
                }
                return Ok(None);
            }
        }
        if words.len() >= 3 {
            break;
        }
    }
    let mut parts = words.iter().map(|&(start, end)| &text[start..end]);
    let Some(mut keyword) = parts.next() else {
        return Ok(None);
    };
    let index = if keyword == "pub" {
        let Some(next) = parts.next() else {
            return Ok(None);
        };
        keyword = next;
        2
    } else {
        1
    };
    let kind = match keyword {
        "fn" => EntityKind::Function,
        "struct" => EntityKind::Struct,
        "enum" => EntityKind::Enum,
        "trait" => EntityKind::Trait,
        "type" => EntityKind::TypeAlias,
        "const" => EntityKind::Constant,
        "static" => EntityKind::Variable,
        _ => return Ok(None),
    };
    let Some(&(start, end)) = words.get(index) else {
        return Ok(None);
    };
    let Some(name) = canonical_rust_identifier(&text[start..end], maximum) else {
        return Ok(None);
    };
    let encoding = builder.input_for_file(fact.span().file())?.encoding();
    let offset = |end| -> Result<u64, AdapterError> {
        let prefix = &text[..end];
        let width = if matches!(encoding.as_str(), "utf-16le" | "utf-16be") {
            prefix
                .encode_utf16()
                .count()
                .checked_mul(2)
                .ok_or(super::super::SinkError::AccountingOverflow)?
        } else {
            prefix.len()
        };
        fact.span()
            .start_byte()
            .checked_add(
                u64::try_from(width).map_err(|_| super::super::SinkError::AccountingOverflow)?,
            )
            .ok_or_else(|| super::super::SinkError::AccountingOverflow.into())
    };
    let span = SourceSpan::new(fact.span().file(), offset(start)?, offset(end)?)
        .map_err(|_| provider_failure("rust-test-import-name-span"))?;
    Ok(Some(Header {
        name: name.into_owned(),
        span,
        kind,
    }))
}

pub(super) fn materialize(
    builder: &mut ProjectFactsBuilder<'_, '_, '_>,
    proofs: Vec<TestImport>,
) -> Result<(), AdapterError> {
    for proof in proofs {
        builder.cancellation.check()?;
        let file = proof.source.span().file();
        let mut record = RelationRecord {
            id: super::super::FactId::from_bytes([0; 20]),
            repository: proof.source.repository(),
            generation: proof.source.generation(),
            subject: RelationEndpoint::Entity(proof.test),
            predicate: RelationPredicate::Imports,
            object: RelationEndpoint::Entity(proof.target),
            confidence: confidence(super::super::IMPORT_CONFIDENCE)?,
            evidence_kind: EvidenceKind::Derived,
            provenance: builder.provenance_for_file(file)?,
            evidence: FactEvidence {
                source: Some(proof.source.clone()),
                derivation: proof.derivation,
            },
        };
        record.id = derive_relation_record_id(&record)
            .map_err(|_| provider_failure("rust-test-import-identity"))?;
        builder.records.push(IrRecord::Relation(record));
        builder
            .state_mut_by_file(file)?
            .increment(FactDomain::Relations)?;
        let input = builder.input_for_file(file)?.clone();
        builder.push_source_gap(
            &input,
            proof.source.span(),
            FactDomain::Relations,
            "rust-test-scope-import-runtime-usage-unverified",
        )?;
    }
    Ok(())
}
