//! Demand-driven member lookup through ECMAScript module namespace objects.
//! Namespace identities refer to source-backed file modules; member paths reuse
//! the bounded export graph rather than flattening every transitive namespace.

use super::*;

impl ProjectFactsBuilder<'_, '_, '_> {
    pub(super) fn materialize_namespace_members(
        &mut self,
        graph: &ExportGraph,
        modules: &BTreeMap<(FileId, String), Vec<FileId>>,
        remaining: &mut usize,
        gaps: &mut Vec<(SourceSpan, &'static str)>,
    ) -> Result<(), AdapterError> {
        let module_entities: BTreeMap<_, _> = self
            .entities
            .iter()
            .filter(|entity| self.module_by_file.get(&entity.file) == Some(&entity.symbol))
            .map(|entity| (entity.file, entity))
            .collect();
        let mut bindings_by_local = BTreeMap::<_, Vec<_>>::new();
        let mut member_cache = BTreeMap::new();
        for import in &self.imports {
            for binding in &import.bindings {
                self.cancellation.check()?;
                let (local, imported) = match binding {
                    ImportBinding::Named { local, imported } => (local.as_str(), Some(imported)),
                    ImportBinding::Namespace { local } => (local.as_str(), None),
                    _ => continue,
                };
                bindings_by_local
                    .entry((import.file, local))
                    .or_default()
                    .push((import, imported));
            }
        }
        for occurrence in &self.occurrences {
            self.cancellation.check()?;
            if !matches!(
                occurrence.role,
                OccurrenceRole::Reference | OccurrenceRole::CallSite | OccurrenceRole::TypeUse
            ) {
                continue;
            }
            let qualifier = occurrence.qualifier.as_deref().unwrap_or(&occurrence.name);
            let Some(root) = qualifier.split('.').next() else {
                continue;
            };
            let mut handled = false;
            let mut selected = BTreeMap::<SymbolId, ExportTarget>::new();
            for (import, imported) in bindings_by_local
                .get(&(occurrence.file, root))
                .into_iter()
                .flatten()
            {
                self.cancellation.check()?;
                let mut targets = Vec::new();
                for file in modules
                    .get(&(import.file, import.module.clone()))
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = imported {
                        // Import names have already been resolved once by the
                        // export graph. Repeated reads must not spend graph work again.
                        targets.extend(
                            self.exports
                                .get(file)
                                .and_then(|exports| exports.get(name.as_str()))
                                .into_iter()
                                .flatten()
                                .cloned(),
                        );
                    } else if let Some(entity) = module_entities.get(file) {
                        targets.push(ExportTarget {
                            entity: (*entity).clone(),
                            type_only: false,
                            module_namespace: true,
                        });
                    }
                }
                if !targets.is_empty() && !targets.iter().any(|target| target.module_namespace) {
                    // Ordinary imported objects need their own property or
                    // dispatch analysis, not module-export lookup.
                    continue;
                }
                handled = true;
                for target in &mut targets {
                    target.type_only |= import.type_only.contains(root);
                }
                for name in qualifier.split('.').skip(1).chain(
                    occurrence
                        .qualifier
                        .as_ref()
                        .map(|_| occurrence.name.as_str()),
                ) {
                    let mut members = BTreeMap::<SymbolId, ExportTarget>::new();
                    for target in targets.iter().filter(|target| target.module_namespace) {
                        match graph.resolve_cached(
                            (target.entity.file, name.to_owned()),
                            &self.exports,
                            &mut member_cache,
                            remaining,
                            self.cancellation,
                        )? {
                            ExportResolution::Targets(found) => {
                                for mut member in found.iter().cloned() {
                                    self.cancellation.check()?;
                                    member.type_only |= target.type_only;
                                    members
                                        .entry(member.entity.symbol)
                                        .and_modify(|known| known.type_only &= member.type_only)
                                        .or_insert(member);
                                }
                            }
                            ExportResolution::Unavailable(detail) => {
                                gaps.push((occurrence.source.span(), detail))
                            }
                        }
                    }
                    targets = members.into_values().collect();
                    if targets.is_empty() {
                        break;
                    }
                }
                for target in targets {
                    selected
                        .entry(target.entity.symbol)
                        .and_modify(|known| known.type_only &= target.type_only)
                        .or_insert(target);
                }
            }
            if handled {
                let type_position = occurrence.role == OccurrenceRole::TypeUse
                    || occurrence.syntax_kind == "typescript.type_query_value.reference";
                let mut typed_value = false;
                let mut called_namespace = false;
                selected.retain(|_, target| {
                    typed_value |= target.type_only && !type_position;
                    called_namespace |=
                        target.module_namespace && occurrence.role == OccurrenceRole::CallSite;
                    (!target.type_only || type_position)
                        && (!target.module_namespace || occurrence.role != OccurrenceRole::CallSite)
                });
                if typed_value {
                    gaps.push((occurrence.source.span(), "ecmascript-type-only-value-use"));
                }
                if called_namespace {
                    gaps.push((
                        occurrence.source.span(),
                        "ecmascript-namespace-not-callable",
                    ));
                }
                if selected.is_empty() {
                    gaps.push((
                        occurrence.source.span(),
                        "ecmascript-namespace-member-unavailable",
                    ));
                }
                self.namespace_occurrence_targets
                    .insert(occurrence.source.span(), selected.into_values().collect());
            }
        }
        Ok(())
    }
}

impl ExportGraph {
    fn resolve_cached<'a>(
        &self,
        key: ExportKey,
        direct: &BTreeMap<FileId, BTreeMap<String, Vec<ExportTarget>>>,
        cache: &'a mut BTreeMap<ExportKey, ExportResolution>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<&'a ExportResolution, AdapterError> {
        cancellation.check()?;
        Ok(match cache.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let resolved = self.resolve(entry.key(), direct, remaining, cancellation)?;
                entry.insert(resolved)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_namespace_members_share_graph_work_and_preserve_cancellation() {
        let graph = ExportGraph::default();
        let file = FileId::from_bytes([1; 20]);
        let key = (file, "Public".to_owned());
        let direct = BTreeMap::new();
        let mut cache = BTreeMap::new();
        let mut remaining = 1;
        let cancellation = Cancellation::new();
        for _ in 0..3 {
            assert!(
                matches!(graph.resolve_cached(key.clone(), &direct, &mut cache, &mut remaining, &cancellation).unwrap(), ExportResolution::Targets(targets) if targets.is_empty())
            );
            assert_eq!(remaining, 0);
        }
        let missing = (file, "Other".to_owned());
        assert!(matches!(
            graph
                .resolve_cached(missing, &direct, &mut cache, &mut remaining, &cancellation)
                .unwrap(),
            ExportResolution::Unavailable("ecmascript-reexport-work-limit")
        ));
        assert_eq!(cache.len(), 2);
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            graph.resolve_cached(key, &direct, &mut cache, &mut remaining, &cancellation),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }
}
