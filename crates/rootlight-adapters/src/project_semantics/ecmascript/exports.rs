//! Source-backed module export entries for native ECMAScript project binding.
//! Public names are distinct from local declaration names; lexical scopes and
//! type-only exports constrain which authored identities an import may expose.

use super::*;

mod reexports;

#[derive(Clone)]
pub(in crate::project_semantics) struct ExportTarget {
    pub(in crate::project_semantics) entity: SemanticEntity,
    pub(in crate::project_semantics) type_only: bool,
    pub(in crate::project_semantics) module_namespace: bool,
}

impl ProjectFactsBuilder<'_, '_, '_> {
    pub(in crate::project_semantics) fn materialize_named_exports(
        &mut self,
    ) -> Result<(), AdapterError> {
        if !matches!(
            self.analyzer.language,
            SemanticProjectLanguage::JavaScript | SemanticProjectLanguage::TypeScript
        ) {
            return Ok(());
        }
        let by_symbol: BTreeMap<_, _> = self
            .entities
            .iter()
            .map(|entity| (entity.symbol, entity))
            .collect();
        let mut additions = Vec::new();
        let mut gaps = Vec::new();
        let mut explicit = Vec::new();
        for input in &self.parsed {
            self.cancellation.check()?;
            if !input.facts.iter().any(is_export_metadata) {
                continue;
            }
            let file = input.input.source().source_ref().span().file();
            let by_id: BTreeMap<_, _> = input
                .facts
                .iter()
                .map(|fact| (fact.local_id(), fact))
                .collect();
            let mut locals = BTreeMap::<String, Vec<&SemanticEntity>>::new();
            let mut declarations = BTreeMap::<u64, Vec<&SemanticEntity>>::new();
            let mut metadata = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
            for (index, fact) in input.facts.iter().enumerate() {
                check_periodically(index, self.cancellation)?;
                if is_export_metadata(fact) {
                    metadata
                        .entry(fact.span().start_byte())
                        .or_default()
                        .push(fact);
                }
                if structural_entity_kind(fact).is_none()
                    || !module_declaration(fact, &by_id, self.cancellation)?
                {
                    continue;
                }
                if let Some(entity) = self
                    .symbol_by_declaration
                    .get(&(file, fact.local_id()))
                    .and_then(|symbol| by_symbol.get(symbol))
                    .copied()
                    && entity.kind != EntityKind::Import
                {
                    locals.entry(entity.name.clone()).or_default().push(entity);
                    declarations
                        .entry(entity.span.start_byte())
                        .or_default()
                        .push(entity);
                }
            }
            for (index, fact) in input.facts.iter().enumerate() {
                check_periodically(index, self.cancellation)?;
                if !is_export_metadata(fact) {
                    continue;
                }
                let label = fact.syntax_kind().as_str();
                let mut selected = Vec::new();
                let mut deferred = false;
                if label.ends_with(".export_named_declaration.signature") {
                    for (_, entities) in
                        declarations.range(fact.span().start_byte()..fact.span().end_byte())
                    {
                        for entity in entities {
                            if contains_span(fact.span(), entity.span) {
                                selected.push((entity.name.clone(), *entity, false));
                            }
                        }
                    }
                } else if label.ends_with(".export_local_specifier.signature")
                    || label.ends_with(".export_type_specifier.signature")
                {
                    let mut local = None;
                    let mut alias = None;
                    for (child_index, child) in metadata
                        .range(fact.span().start_byte()..fact.span().end_byte())
                        .flat_map(|(_, facts)| facts)
                        .enumerate()
                    {
                        check_periodically(child_index, self.cancellation)?;
                        if !contains_span(fact.span(), child.span()) {
                            continue;
                        }
                        let child_label = child.syntax_kind().as_str();
                        if child_label.ends_with(".export_binding_name.signature") {
                            // Local export references must be identifiers even
                            // though public names may be string literals.
                            local = source_text(input.input.source().bytes(), child.span())
                                .filter(|name| is_identifier(name))
                                .map(str::to_owned);
                        } else if child_label.ends_with(".export_binding_alias.signature") {
                            alias = Some(export_name(
                                input.input.source().bytes(),
                                child.span(),
                                self.cancellation,
                            )?);
                        }
                    }
                    if let Some(local) = local {
                        let public = match alias {
                            None => Some(local.clone()),
                            Some(alias) => alias,
                        };
                        let type_only = label.ends_with(".export_type_specifier.signature");
                        if let Some(public) = public {
                            explicit.push((file, public.clone()));
                            if let Some(entities) = locals.get(&local) {
                                selected.extend(
                                    entities
                                        .iter()
                                        .map(|entity| (public.clone(), *entity, type_only)),
                                );
                            } else {
                                deferred = self.imports.iter().filter(|import| import.file == file).any(|import| import.bindings.iter().any(|binding| matches!(binding, ImportBinding::Named { local: name, .. } | ImportBinding::Namespace { local: name } if name == &local)));
                            }
                        }
                    }
                } else if !label.ends_with(".export_unsupported_specifier.signature") {
                    continue;
                }
                if selected.is_empty() && !deferred {
                    gaps.push(fact.span());
                }
                additions.extend(selected.into_iter().map(|(name, entity, type_only)| {
                    (
                        file,
                        name,
                        ExportTarget {
                            entity: entity.clone(),
                            type_only,
                            module_namespace: false,
                        },
                    )
                }));
            }
        }
        for (file, name) in explicit {
            self.exports
                .entry(file)
                .or_default()
                .entry(name)
                .or_default();
        }
        for (file, name, target) in additions {
            self.exports
                .entry(file)
                .or_default()
                .entry(name)
                .or_default()
                .push(target);
        }
        for span in gaps {
            self.cancellation.check()?;
            self.push_relation_gap(
                self.input_for_file(span.file())?,
                span,
                "ecmascript-export-entry-target-unavailable",
            )?;
        }
        Ok(())
    }
}

fn module_declaration(
    fact: &SyntaxFact,
    by_id: &BTreeMap<u64, &SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = fact.parent();
    while let Some(ancestor) = parent.and_then(|id| by_id.get(&id)) {
        cancellation.check()?;
        if structural_entity_kind(ancestor).is_some_and(|kind| kind != EntityKind::Module)
            || (ancestor.kind() == SyntaxFactKind::Scope && ancestor.span() != fact.span())
        {
            return Ok(false);
        }
        parent = ancestor.parent();
    }
    Ok(true)
}

/// Decodes a public export name from its exact native identifier or string field.
pub(in crate::project_semantics) fn export_name(
    source: &[u8],
    span: SourceSpan,
    cancellation: &Cancellation,
) -> Result<Option<String>, AdapterError> {
    let Some(text) = source_text(source, span) else {
        return Ok(None);
    };
    if is_identifier(text) {
        Ok(Some(text.to_owned()))
    } else {
        string_literal::decode(text, cancellation)
    }
}
