//! Import bindings reconstructed from native grammar-field evidence.
//! Source-ordered metadata avoids rescanning whole files per import; absent or
//! unsupported literal evidence cannot fall back to guessed textual bindings.

use super::*;

pub(super) struct NativeImports<'a> {
    facts: BTreeMap<u64, Vec<&'a SyntaxFact>>,
}

pub(super) struct ParsedImport {
    pub(super) module: String,
    pub(super) bindings: Vec<ImportBinding>,
    pub(super) type_only: BTreeSet<String>,
}

impl<'a> NativeImports<'a> {
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
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

    pub(super) fn parse(
        &self,
        span: SourceSpan,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<ParsedImport>, AdapterError> {
        let mut module = None;
        let mut bindings = Vec::new();
        let mut type_only = BTreeSet::new();
        for (index, fact) in self.within(span).enumerate() {
            check_periodically(index, cancellation)?;
            let label = fact.syntax_kind().as_str();
            if label.ends_with(".type_import_binding.declaration") {
                let Some(name) =
                    source_text(source, fact.span()).filter(|name| is_identifier(name))
                else {
                    return Ok(None);
                };
                type_only.insert(name.to_owned());
            } else if label.ends_with(".import_source.signature") {
                let Some(value) = source_text(source, fact.span()).and_then(quoted_literal) else {
                    return Ok(None);
                };
                if module.replace(value.to_owned()).is_some() {
                    return Ok(None);
                }
            } else if label.ends_with(".import_default.signature")
                || label.ends_with(".import_namespace.signature")
            {
                let Some(local) =
                    source_text(source, fact.span()).filter(|name| is_identifier(name))
                else {
                    return Ok(None);
                };
                bindings.push(if label.ends_with(".import_default.signature") {
                    ImportBinding::Named {
                        local: local.to_owned(),
                        imported: "default".to_owned(),
                    }
                } else {
                    ImportBinding::Namespace {
                        local: local.to_owned(),
                    }
                });
            } else if label.ends_with(".import_specifier.signature") {
                let mut local = None;
                let mut imported = None;
                for (child_index, child) in self.within(fact.span()).enumerate() {
                    check_periodically(child_index, cancellation)?;
                    if structural_entity_kind(child) == Some(EntityKind::Import) {
                        let Some(name) =
                            source_text(source, child.span()).filter(|name| is_identifier(name))
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
                        let Some(name) = source_text(source, child.span()).and_then(|name| {
                            if is_identifier(name) {
                                Some(name)
                            } else {
                                quoted_literal(name)
                            }
                        }) else {
                            return Ok(None);
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
                    local: local.to_owned(),
                    imported: imported.to_owned(),
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

fn quoted_literal(source: &str) -> Option<&str> {
    let inner = source
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .or_else(|| {
            source
                .strip_prefix('\'')
                .and_then(|text| text.strip_suffix('\''))
        })?;
    // Escaped specifiers require ECMAScript string-value decoding. Keeping
    // their bytes as a module path would invent a different dependency.
    (!inner.contains(['\\', '\r', '\n'])).then_some(inner)
}
