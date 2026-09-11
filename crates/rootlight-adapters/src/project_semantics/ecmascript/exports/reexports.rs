//! Demand-driven export graph resolution for native ECMAScript modules.
//! Iterative traversal preserves cycle and diamond semantics without a recursive
//! stack or eagerly copying every star-exported name into every importing file.

use super::*;

mod namespace_members;

type ExportKey = (FileId, String);

enum ExportResolution {
    Targets(Vec<ExportTarget>),
    Unavailable(&'static str),
}

struct Edge {
    targets: Vec<FileId>,
    imported: Option<String>,
    type_only: bool,
}

#[derive(Default)]
struct ExportGraph {
    named: BTreeMap<ExportKey, Vec<Edge>>,
    stars: BTreeMap<FileId, Vec<Edge>>,
    queries: BTreeMap<ExportKey, BTreeSet<SourceSpan>>,
}

impl ProjectFactsBuilder<'_, '_, '_> {
    pub(in crate::project_semantics) fn materialize_reexports(
        &mut self,
    ) -> Result<(), AdapterError> {
        if !matches!(
            self.analyzer.language,
            SemanticProjectLanguage::JavaScript | SemanticProjectLanguage::TypeScript
        ) {
            return Ok(());
        }
        let mut graph = ExportGraph::default();
        let module_entities: BTreeMap<_, _> = self
            .entities
            .iter()
            .filter(|entity| self.module_by_file.get(&entity.file) == Some(&entity.symbol))
            .map(|entity| (entity.file, entity.clone()))
            .collect();
        let mut modules = self.import_target_index();
        let mut gaps = Vec::new();
        for input in &self.parsed {
            self.cancellation.check()?;
            let file = input.input.source().source_ref().span().file();
            let source = input.input.source().bytes();
            let mut metadata = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
            for (index, fact) in input.facts.iter().enumerate() {
                check_periodically(index, self.cancellation)?;
                if is_export_metadata(fact) {
                    metadata
                        .entry(fact.span().start_byte())
                        .or_default()
                        .push(fact);
                }
            }
            let mut statements = BTreeMap::<u64, (SourceSpan, Vec<FileId>)>::new();
            for fact in metadata.values().flatten() {
                self.cancellation.check()?;
                let label = fact.syntax_kind().as_str();
                if !label.ends_with("_statement.signature") {
                    continue;
                }
                let module_fact = within(&metadata, fact.span()).find(|child| {
                    child
                        .syntax_kind()
                        .as_str()
                        .ends_with(".export_source.signature")
                });
                let module = match module_fact {
                    Some(child) => source_text(source, child.span())
                        .map(|text| string_literal::decode(text, self.cancellation))
                        .transpose()?
                        .flatten(),
                    None => None,
                };
                let Some(module) = module else {
                    gaps.push((fact.span(), "ecmascript-reexport-evidence-unavailable"));
                    continue;
                };
                let targets = modules
                    .entry((file, module.clone()))
                    .or_insert_with(|| {
                        let path = self
                            .path_by_file
                            .get(&file)
                            .map(String::as_str)
                            .unwrap_or_default();
                        self.path_by_file
                            .iter()
                            .filter_map(|(target, target_path)| {
                                module_matches(self.analyzer.language, path, &module, target_path)
                                    .then_some(*target)
                            })
                            .collect()
                    })
                    .clone();
                if targets.is_empty() {
                    gaps.push((fact.span(), "ecmascript-reexport-evidence-unavailable"));
                }
                statements.insert(fact.span().start_byte(), (fact.span(), targets.clone()));
                if label.ends_with(".export_star_statement.signature")
                    || label.ends_with(".export_type_star_statement.signature")
                {
                    graph.stars.entry(file).or_default().push(Edge {
                        targets,
                        imported: None,
                        type_only: label.ends_with(".export_type_star_statement.signature"),
                    });
                } else if label.ends_with(".export_unsupported_statement.signature") {
                    for name in within(&metadata, fact.span()).filter(|child| {
                        child
                            .syntax_kind()
                            .as_str()
                            .ends_with(".export_namespace_name.signature")
                    }) {
                        if let Some(public) = export_name(
                            source,
                            name.span(),
                            self.request.limits().ir().max_string_bytes,
                            self.cancellation,
                        )? {
                            // Unsupported namespace objects still reserve their
                            // explicit name ahead of unrelated star exports.
                            self.exports
                                .entry(file)
                                .or_default()
                                .entry(public)
                                .or_default();
                        }
                    }
                    gaps.push((fact.span(), "ecmascript-reexport-evidence-unavailable"));
                } else if label.ends_with(".export_namespace_statement.signature")
                    || label.ends_with(".export_type_namespace_statement.signature")
                {
                    for name in within(&metadata, fact.span()).filter(|child| {
                        child
                            .syntax_kind()
                            .as_str()
                            .ends_with(".export_namespace_name.signature")
                    }) {
                        if let Some(public) = export_name(
                            source,
                            name.span(),
                            self.request.limits().ir().max_string_bytes,
                            self.cancellation,
                        )? {
                            self.export_reference_names
                                .insert(name.span(), public.clone());
                            let namespace_targets: Vec<_> = targets
                                .iter()
                                .filter_map(|target| {
                                    let entity = module_entities.get(target)?;
                                    Some(ExportTarget {
                                        entity: entity.clone(),
                                        type_only: label.ends_with(
                                            ".export_type_namespace_statement.signature",
                                        ),
                                        module_namespace: true,
                                    })
                                })
                                .collect();
                            self.exports
                                .entry(file)
                                .or_default()
                                .entry(public)
                                .or_default()
                                .extend(namespace_targets);
                        }
                    }
                }
            }
            for fact in metadata.values().flatten() {
                self.cancellation.check()?;
                let label = fact.syntax_kind().as_str();
                let remote = label.ends_with(".export_remote_specifier.signature")
                    || label.ends_with(".export_remote_type_specifier.signature");
                let local = label.ends_with(".export_local_specifier.signature")
                    || label.ends_with(".export_type_specifier.signature");
                if !remote && !local {
                    continue;
                }
                let type_only = label.ends_with(".export_type_specifier.signature")
                    || label.ends_with(".export_remote_type_specifier.signature");
                let fields: Vec<_> = within(&metadata, fact.span()).collect();
                let name_fact = fields.iter().find(|child| {
                    child
                        .syntax_kind()
                        .as_str()
                        .ends_with(".export_binding_name.signature")
                });
                let alias_fact = fields.iter().find(|child| {
                    child
                        .syntax_kind()
                        .as_str()
                        .ends_with(".export_binding_alias.signature")
                });
                let imported = match name_fact {
                    Some(child) if remote => export_name(
                        source,
                        child.span(),
                        self.request.limits().ir().max_string_bytes,
                        self.cancellation,
                    )?,
                    Some(child) => source_text(source, child.span())
                        .and_then(|name| {
                            canonical_ecmascript_identifier(
                                name,
                                self.request.limits().ir().max_string_bytes,
                            )
                        })
                        .map(|name| name.into_owned()),
                    None => None,
                };
                let Some(imported) = imported else {
                    if remote {
                        gaps.push((fact.span(), "ecmascript-reexport-evidence-unavailable"));
                    }
                    continue;
                };
                let public = match alias_fact {
                    Some(child) => export_name(
                        source,
                        child.span(),
                        self.request.limits().ir().max_string_bytes,
                        self.cancellation,
                    )?,
                    None => Some(imported.clone()),
                };
                let Some(public) = public else {
                    if remote {
                        gaps.push((fact.span(), "ecmascript-reexport-evidence-unavailable"));
                    }
                    continue;
                };
                for child in &fields {
                    if child
                        .syntax_kind()
                        .as_str()
                        .ends_with(".export_binding_alias.signature")
                        || (remote
                            && child
                                .syntax_kind()
                                .as_str()
                                .ends_with(".export_binding_name.signature"))
                    {
                        self.export_reference_names
                            .insert(child.span(), public.clone());
                    }
                }
                let key = (file, public);
                graph
                    .queries
                    .entry(key.clone())
                    .or_default()
                    .insert(fact.span());
                if remote {
                    let targets = statements
                        .range(..=fact.span().start_byte())
                        .next_back()
                        .filter(|(_, (span, _))| contains_span(*span, fact.span()))
                        .map(|(_, (_, targets))| targets.clone())
                        .unwrap_or_default();
                    graph.named.entry(key).or_default().push(Edge {
                        targets,
                        imported: Some(imported),
                        type_only,
                    });
                } else if self
                    .exports
                    .get(&file)
                    .and_then(|exports| exports.get(&key.1))
                    .is_none_or(|targets| targets.is_empty())
                {
                    for import in self.imports.iter().filter(|import| import.file == file) {
                        for binding in &import.bindings {
                            if let ImportBinding::Namespace { local } = binding
                                && local == &imported
                            {
                                let targets: Vec<_> = modules
                                    .get(&(file, import.module.clone()))
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|target| module_entities.get(target))
                                    .map(|entity| ExportTarget {
                                        entity: entity.clone(),
                                        type_only: type_only || import.type_only.contains(local),
                                        module_namespace: true,
                                    })
                                    .collect();
                                self.exports
                                    .entry(file)
                                    .or_default()
                                    .entry(key.1.clone())
                                    .or_default()
                                    .extend(targets);
                            }
                            if let ImportBinding::Named {
                                local,
                                imported: target_name,
                            } = binding
                                && local == &imported
                            {
                                let targets = modules
                                    .get(&(file, import.module.clone()))
                                    .cloned()
                                    .unwrap_or_default();
                                graph.named.entry(key.clone()).or_default().push(Edge {
                                    targets,
                                    imported: Some(target_name.clone()),
                                    type_only: type_only || import.type_only.contains(local),
                                });
                            }
                        }
                    }
                }
            }
        }
        for import in &self.imports {
            self.cancellation.check()?;
            let targets = modules
                .get(&(import.file, import.module.clone()))
                .map(Vec::as_slice)
                .unwrap_or_default();
            for binding in &import.bindings {
                let names: BTreeSet<_> = match binding {
                    ImportBinding::Named { imported, .. } => BTreeSet::from([imported.as_str()]),
                    ImportBinding::Namespace { local } => self
                        .occurrences
                        .iter()
                        .filter(|occurrence| {
                            occurrence.file == import.file
                                && occurrence.qualifier.as_deref() == Some(local)
                        })
                        .map(|occurrence| occurrence.name.as_str())
                        .collect(),
                    _ => BTreeSet::new(),
                };
                for target in targets {
                    for name in &names {
                        graph
                            .queries
                            .entry((*target, (*name).to_owned()))
                            .or_default()
                            .insert(import.span);
                    }
                }
            }
        }
        let mut remaining = self.request.limits().ir().max_total_nested_items;
        let mut resolved = Vec::new();
        for (key, spans) in &graph.queries {
            let result = graph.resolve(key, &self.exports, &mut remaining, self.cancellation)?;
            match result {
                ExportResolution::Targets(targets) => {
                    if targets.is_empty() {
                        gaps.extend(
                            spans
                                .iter()
                                .map(|span| (*span, "ecmascript-reexport-target-unavailable")),
                        );
                    }
                    resolved.push((key.clone(), targets));
                }
                ExportResolution::Unavailable(detail) => {
                    gaps.extend(spans.iter().map(|span| (*span, detail)));
                    resolved.push((key.clone(), Vec::new()));
                }
            }
        }
        for ((file, name), targets) in resolved {
            self.exports.entry(file).or_default().insert(name, targets);
        }
        self.materialize_namespace_members(&graph, &modules, &mut remaining, &mut gaps)?;
        gaps.sort_unstable();
        gaps.dedup();
        for (span, detail) in gaps {
            self.cancellation.check()?;
            self.push_relation_gap(self.input_for_file(span.file())?, span, detail)?;
        }
        Ok(())
    }
}

fn within<'a>(
    metadata: &'a BTreeMap<u64, Vec<&'a SyntaxFact>>,
    span: SourceSpan,
) -> impl Iterator<Item = &'a SyntaxFact> {
    metadata
        .range(span.start_byte()..span.end_byte())
        .flat_map(|(_, facts)| facts.iter().copied())
        .filter(move |fact| contains_span(span, fact.span()))
}

impl ExportGraph {
    fn resolve(
        &self,
        root: &ExportKey,
        direct: &BTreeMap<FileId, BTreeMap<String, Vec<ExportTarget>>>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<ExportResolution, AdapterError> {
        let mut pending = vec![(root.clone(), false)];
        let mut visited = BTreeSet::from([(root.clone(), false)]);
        let mut found = BTreeMap::<SymbolId, ExportTarget>::new();
        while let Some((key, type_only)) = pending.pop() {
            cancellation.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                return Ok(ExportResolution::Unavailable(
                    "ecmascript-reexport-work-limit",
                ));
            };
            *remaining = next;
            let local = direct.get(&key.0).and_then(|exports| exports.get(&key.1));
            if let Some(targets) = local.filter(|targets| !targets.is_empty()) {
                for target in targets {
                    cancellation.check()?;
                    let only = type_only || target.type_only;
                    found
                        .entry(target.entity.symbol)
                        .and_modify(|known| known.type_only &= only)
                        .or_insert_with(|| ExportTarget {
                            entity: target.entity.clone(),
                            type_only: only,
                            module_namespace: target.module_namespace,
                        });
                }
                continue;
            }
            let edges = if let Some(edges) = self.named.get(&key) {
                Some(edges)
            } else if local.is_some() || key.1 == "default" {
                None
            } else {
                self.stars.get(&key.0)
            };
            for edge in edges.into_iter().flatten() {
                cancellation.check()?;
                if edge.targets.is_empty() {
                    return Ok(ExportResolution::Unavailable(
                        "ecmascript-reexport-module-unavailable",
                    ));
                }
                for target in &edge.targets {
                    let next = (
                        (*target, edge.imported.as_ref().unwrap_or(&key.1).clone()),
                        type_only || edge.type_only,
                    );
                    if visited.contains(&next) {
                        continue;
                    }
                    // Reserve room before growing the frontier; repeated paths
                    // cannot allocate duplicate work beyond the shared budget.
                    if pending.len() >= *remaining {
                        return Ok(ExportResolution::Unavailable(
                            "ecmascript-reexport-work-limit",
                        ));
                    }
                    visited.insert(next.clone());
                    pending.push(next);
                }
            }
        }
        // A diamond reaching the same binding is valid; two distinct defining
        // bindings are ambiguous even when later namespace filtering finds one.
        let origins: BTreeSet<_> = found
            .values()
            .map(|target| (target.entity.file, target.entity.name.as_str()))
            .collect();
        if origins.len() > 1 {
            return Ok(ExportResolution::Unavailable(
                "ecmascript-reexport-ambiguous",
            ));
        }
        Ok(ExportResolution::Targets(found.into_values().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_graph_cycles_obey_work_budget_and_cancellation() {
        let first = FileId::from_bytes([1; 20]);
        let second = FileId::from_bytes([2; 20]);
        let mut graph = ExportGraph::default();
        graph.stars.insert(
            first,
            vec![Edge {
                targets: vec![second],
                imported: None,
                type_only: false,
            }],
        );
        graph.stars.insert(
            second,
            vec![Edge {
                targets: vec![first],
                imported: None,
                type_only: false,
            }],
        );
        let key = (first, "Public".to_owned());
        let direct = BTreeMap::new();
        let cancellation = Cancellation::new();
        let mut exact = 2;
        assert!(
            matches!(graph.resolve(&key, &direct, &mut exact, &cancellation).unwrap(), ExportResolution::Targets(targets) if targets.is_empty())
        );
        assert_eq!(exact, 0);
        let mut short = 1;
        assert!(matches!(
            graph
                .resolve(&key, &direct, &mut short, &cancellation)
                .unwrap(),
            ExportResolution::Unavailable("ecmascript-reexport-work-limit")
        ));
        assert_eq!(short, 0);
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            graph.resolve(&key, &direct, &mut exact, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }
}
