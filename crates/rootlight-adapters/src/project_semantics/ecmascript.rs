//! Module bindings reconstructed from native grammar-field evidence.
//! Source-ordered metadata avoids rescanning whole files per import; absent or
//! unsupported literal evidence cannot fall back to guessed textual bindings.

use super::*;

pub(super) mod exports;
pub(super) mod lexical;
pub(super) mod references;
pub(super) mod scheduling;
mod string_literal;

pub(super) fn is_export_metadata(fact: &SyntaxFact) -> bool {
    is_default_export(fact)
        || (fact.kind() == SyntaxFactKind::Signature
            && (fact
                .syntax_kind()
                .as_str()
                .starts_with("typescript.export_")
                || fact
                    .syntax_kind()
                    .as_str()
                    .starts_with("javascript.export_")))
}

struct NativeImports<'a> {
    facts: BTreeMap<u64, Vec<&'a SyntaxFact>>,
}

pub(super) struct ParsedImport {
    pub(super) module: String,
    pub(super) bindings: Vec<ImportBinding>,
    pub(super) type_only: BTreeSet<String>,
}

pub(super) fn collect_imports(
    facts: &[SyntaxFact],
    source: &[u8],
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, ParsedImport>, AdapterError> {
    cancellation.check()?;
    let mut imports = BTreeMap::new();
    if !facts.iter().any(is_native_import) {
        return Ok(imports);
    }
    let metadata = NativeImports::new(facts, cancellation)?;
    for (index, fact) in facts.iter().enumerate() {
        check_periodically(index, cancellation)?;
        if is_native_import(fact)
            && let Some(import) = metadata.parse(fact.span(), source, maximum, cancellation)?
        {
            imports.insert(fact.local_id(), import);
        }
    }
    Ok(imports)
}

pub(super) fn is_native_import(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Import
        && fact
            .syntax_kind()
            .as_str()
            .ends_with(".native_import.import")
}

pub(super) fn is_default_export(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "javascript.default_export_declaration.signature"
                | "typescript.default_export_declaration.signature"
                | "javascript.default_export_value.signature"
                | "typescript.default_export_value.signature"
        )
}

impl ParsedImport {
    pub(super) fn runtime_bindings(&self) -> impl Iterator<Item = &ImportBinding> {
        self.bindings.iter().filter(|binding| match binding {
            ImportBinding::Named { local, .. } | ImportBinding::Namespace { local } => {
                !self.type_only.contains(local)
            }
            ImportBinding::SideEffect | ImportBinding::Wildcard | ImportBinding::Dart(_) => true,
        })
    }
}

impl<'a> NativeImports<'a> {
    fn new(facts: &'a [SyntaxFact], cancellation: &Cancellation) -> Result<Self, AdapterError> {
        let mut selected = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        for (index, fact) in facts.iter().enumerate() {
            check_periodically(index, cancellation)?;
            if (fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str().contains(".import_"))
                || structural_entity_kind(fact) == Some(EntityKind::Import)
            {
                selected
                    .entry(fact.span().start_byte())
                    .or_default()
                    .push(fact);
            }
        }
        Ok(Self { facts: selected })
    }

    fn within(&self, span: SourceSpan) -> impl Iterator<Item = &'a SyntaxFact> + '_ {
        self.facts
            .range(span.start_byte()..span.end_byte())
            .flat_map(|(_, facts)| facts.iter().copied())
            .filter(move |fact| contains_span(span, fact.span()))
    }

    fn parse(
        &self,
        span: SourceSpan,
        source: &[u8],
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Option<ParsedImport>, AdapterError> {
        let mut module = None;
        let mut bindings = Vec::new();
        let mut type_only = BTreeSet::new();
        for (index, fact) in self.within(span).enumerate() {
            check_periodically(index, cancellation)?;
            let label = fact.syntax_kind().as_str();
            if label.ends_with(".type_import_binding.declaration") {
                let Some(name) = source_text(source, fact.span())
                    .and_then(|name| canonical_ecmascript_identifier(name, maximum))
                else {
                    return Ok(None);
                };
                type_only.insert(name.into_owned());
            } else if label.ends_with(".import_source.signature") {
                let Some(text) = source_text(source, fact.span()) else {
                    return Ok(None);
                };
                let Some(value) = string_literal::decode(text, cancellation)? else {
                    return Ok(None);
                };
                if module.replace(value).is_some() {
                    return Ok(None);
                }
            } else if label.ends_with(".import_default.signature")
                || label.ends_with(".import_namespace.signature")
            {
                let Some(local) = source_text(source, fact.span())
                    .and_then(|name| canonical_ecmascript_identifier(name, maximum))
                else {
                    return Ok(None);
                };
                bindings.push(if label.ends_with(".import_default.signature") {
                    ImportBinding::Named {
                        local: local.into_owned(),
                        imported: "default".to_owned(),
                    }
                } else {
                    ImportBinding::Namespace {
                        local: local.into_owned(),
                    }
                });
            } else if label.ends_with(".import_specifier.signature") {
                let mut local = None;
                let mut imported = None;
                for (child_index, child) in self.within(fact.span()).enumerate() {
                    check_periodically(child_index, cancellation)?;
                    if structural_entity_kind(child) == Some(EntityKind::Import) {
                        let Some(name) = source_text(source, child.span())
                            .and_then(|name| canonical_ecmascript_identifier(name, maximum))
                        else {
                            return Ok(None);
                        };
                        if local.replace(name).is_some() {
                            return Ok(None);
                        }
                    } else if child
                        .syntax_kind()
                        .as_str()
                        .ends_with(".import_name.signature")
                    {
                        let Some(text) = source_text(source, child.span()) else {
                            return Ok(None);
                        };
                        let name =
                            if let Some(name) = canonical_ecmascript_identifier(text, maximum) {
                                name.into_owned()
                            } else {
                                let Some(name) = string_literal::decode(text, cancellation)? else {
                                    return Ok(None);
                                };
                                name
                            };
                        if imported.replace(name).is_some() {
                            return Ok(None);
                        }
                    }
                }
                let (Some(local), Some(imported)) = (local, imported) else {
                    return Ok(None);
                };
                bindings.push(ImportBinding::Named {
                    local: local.into_owned(),
                    imported,
                });
            }
        }
        let Some(module) = module else {
            return Ok(None);
        };
        if bindings.is_empty() {
            bindings.push(ImportBinding::SideEffect);
        }
        Ok(Some(ParsedImport {
            module,
            bindings,
            type_only,
        }))
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::SyntaxKindLabel;

    use super::*;

    #[test]
    fn native_call_scheduling_never_falls_back_or_admits_type_only_bindings() {
        let source = "import {Run} from './dep'; Run();";
        let file = FileId::from_bytes([1; 20]);
        let facts = [
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Import,
                SourceSpan::new(
                    file,
                    0,
                    u64::try_from(source.find(';').unwrap() + 1).unwrap(),
                )
                .unwrap(),
                0,
                SyntaxKindLabel::new("typescript.native_import.import").unwrap(),
            ),
            SyntaxFact::new(
                2,
                None,
                SyntaxFactKind::Occurrence,
                SourceSpan::new(
                    file,
                    u64::try_from(source.rfind("Run").unwrap()).unwrap(),
                    u64::try_from(source.len() - 1).unwrap(),
                )
                .unwrap(),
                0,
                SyntaxKindLabel::new("typescript.call").unwrap(),
            ),
            SyntaxFact::new(
                3,
                None,
                SyntaxFactKind::Occurrence,
                SourceSpan::new(
                    file,
                    u64::try_from(source.rfind("Run").unwrap()).unwrap(),
                    u64::try_from(source.rfind("Run").unwrap() + "Run".len()).unwrap(),
                )
                .unwrap(),
                0,
                SyntaxKindLabel::new("typescript.call_name").unwrap(),
            ),
        ];
        let by_id = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        let names =
            scheduling::SchedulingNames::new(&facts, source.as_bytes(), 1024, &Cancellation::new())
                .unwrap();
        let schedule = |imports: &BTreeMap<u64, ParsedImport>| {
            imported_call_syntax_fact_groups(
                SemanticProjectLanguage::TypeScript,
                source.as_bytes(),
                &by_id,
                &terminal_call_names(&facts),
                &BTreeSet::new(),
                imports,
                Some(&names),
            )
        };
        assert!(schedule(&BTreeMap::new()).is_empty());
        let mut imports = BTreeMap::from([(
            1,
            ParsedImport {
                module: "./dep".to_owned(),
                bindings: vec![ImportBinding::Named {
                    local: "Run".to_owned(),
                    imported: "Run".to_owned(),
                }],
                type_only: BTreeSet::new(),
            },
        )]);
        let groups = schedule(&imports);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].call_ids, [2]);
        assert_eq!(groups[0].import_id, 1);
        imports
            .get_mut(&1)
            .unwrap()
            .type_only
            .insert("Run".to_owned());
        assert!(schedule(&imports).is_empty());
    }

    #[test]
    fn runtime_import_bindings_preserve_only_value_capable_names() {
        let import = ParsedImport {
            module: "./dep".to_owned(),
            bindings: vec![
                ImportBinding::Named {
                    local: "Typed".to_owned(),
                    imported: "Item".to_owned(),
                },
                ImportBinding::Namespace {
                    local: "Types".to_owned(),
                },
                ImportBinding::Named {
                    local: "Value".to_owned(),
                    imported: "Item".to_owned(),
                },
                ImportBinding::Namespace {
                    local: "Values".to_owned(),
                },
            ],
            type_only: BTreeSet::from(["Typed".to_owned(), "Types".to_owned()]),
        };
        assert_eq!(
            import.runtime_bindings().collect::<Vec<_>>(),
            [&import.bindings[2], &import.bindings[3]]
        );
    }
}
