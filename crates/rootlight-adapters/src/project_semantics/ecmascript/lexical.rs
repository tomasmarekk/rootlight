//! Lexical visibility of local bindings that can shadow module imports.
//! Native scope ancestry and var-statement metadata distinguish function bodies
//! from parameter initializers and sibling blocks without guessing from names.

use super::*;

#[derive(Default)]
pub(in crate::project_semantics) struct LocalBindings {
    by_scope: BTreeMap<SourceSpan, BTreeMap<String, Vec<LocalBinding>>>,
    scope_spans: BTreeMap<SymbolId, SourceSpan>,
    parents: BTreeMap<SourceSpan, SourceSpan>,
}

struct LocalBinding {
    declaration: u64,
    kind: EntityKind,
}

pub(in crate::project_semantics) fn is_binding_metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "javascript.hoisted_binding.signature" | "typescript.hoisted_binding.signature"
        )
}

impl LocalBindings {
    pub(in crate::project_semantics) fn register_scope(
        &mut self,
        fact: &SyntaxFact,
        symbol: SymbolId,
        module: SymbolId,
        facts: &BTreeMap<u64, &SyntaxFact>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.scope_spans.insert(symbol, fact.span());
        let mut parent = fact.parent();
        let mut enclosing = None;
        let mut root = None;
        for _ in 0..facts.len() {
            cancellation.check()?;
            let Some(ancestor) = parent.and_then(|id| facts.get(&id).copied()) else {
                break;
            };
            if enclosing.is_none()
                && ancestor.kind() == SyntaxFactKind::Scope
                && ancestor.span() != fact.span()
            {
                enclosing = Some(ancestor.span());
            }
            root = Some(ancestor.span());
            parent = ancestor.parent();
        }
        if let Some(root) = root {
            self.scope_spans.insert(module, root);
        }
        if let Some(parent) = enclosing.or(root).filter(|parent| *parent != fact.span()) {
            self.parents.insert(fact.span(), parent);
        }
        Ok(())
    }

    pub(in crate::project_semantics) fn insert(
        &mut self,
        draft: &DeclarationDraft,
        fact: &SyntaxFact,
        facts: &BTreeMap<u64, &SyntaxFact>,
        hoisted_bindings: &BTreeMap<u64, SourceSpan>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        if matches!(draft.kind, EntityKind::Method | EntityKind::Field)
            || fact.syntax_kind().as_str().contains("import_binding")
        {
            return Ok(());
        }
        let mut parent = fact.parent();
        let mut scopes = Vec::new();
        let mut root = None;
        let variable = fact
            .syntax_kind()
            .as_str()
            .ends_with(".variable.declaration");
        let mut hoisted =
            variable && hoisted_bindings.get(&fact.span().start_byte()) == Some(&fact.span());
        let mut binding_ancestry = variable;
        for _ in 0..facts.len() {
            cancellation.check()?;
            let Some(ancestor) = parent.and_then(|id| facts.get(&id).copied()) else {
                break;
            };
            if binding_ancestry {
                hoisted |=
                    hoisted_bindings.get(&ancestor.span().start_byte()) == Some(&ancestor.span());
                binding_ancestry = ancestor.kind() != SyntaxFactKind::Scope;
            }
            if ancestor.kind() == SyntaxFactKind::Scope {
                // Function declarations bind in the surrounding block; a named
                // function expression instead binds only inside its lambda scope.
                if ancestor.span() != fact.span()
                    || ancestor.syntax_kind().as_str().ends_with(".lambda.scope")
                {
                    scopes.push(ancestor);
                }
            }
            root = Some(ancestor.span());
            parent = ancestor.parent();
        }
        let scope = if hoisted {
            let boundary = scopes.iter().position(|scope| is_var_scope(scope));
            if let Some(index) = boundary {
                // Body vars are not visible while parameter defaults execute.
                scopes
                    .get(..index)
                    .and_then(|inner| {
                        inner
                            .iter()
                            .rev()
                            .find(|scope| scope.syntax_kind().as_str().ends_with(".block.scope"))
                    })
                    .map(|scope| scope.span())
                    .or_else(|| scopes.get(index).map(|scope| scope.span()))
            } else {
                root
            }
        } else {
            scopes.first().map(|scope| scope.span()).or(root)
        };
        if let Some(scope) = scope {
            self.by_scope
                .entry(scope)
                .or_default()
                .entry(draft.name.clone())
                .or_default()
                .push(LocalBinding {
                    declaration: draft.local_id,
                    kind: draft.kind,
                });
        }
        Ok(())
    }

    pub(in crate::project_semantics) fn visible(
        &self,
        occurrence: &OccurrenceDraft,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Vec<u64>, AdapterError> {
        let mut scope = occurrence
            .enclosing
            .and_then(|symbol| self.scope_spans.get(&symbol).copied());
        for _ in 0..=self.parents.len() {
            cancellation.check()?;
            let Some(current) = scope else { break };
            let mut selected = Vec::new();
            for binding in self
                .by_scope
                .get(&current)
                .and_then(|names| names.get(name))
                .into_iter()
                .flatten()
            {
                cancellation.check()?;
                let namespace_position = occurrence.role == OccurrenceRole::TypeUse
                    && occurrence.qualifier.is_some()
                    || matches!(
                        occurrence.syntax_kind.as_str(),
                        "typescript.type_namespace_root.reference"
                            | "typescript.type_namespace_member.reference"
                    );
                // A qualified type root is a namespace lookup. A type parameter
                // or local class can shadow a type without shadowing that namespace.
                let admitted = if namespace_position {
                    matches!(
                        binding.kind,
                        EntityKind::Namespace | EntityKind::Module | EntityKind::Enum
                    )
                } else if occurrence.role == OccurrenceRole::TypeUse {
                    matches!(
                        binding.kind,
                        EntityKind::Class
                            | EntityKind::Enum
                            | EntityKind::Interface
                            | EntityKind::TypeAlias
                            | EntityKind::TypeParameter
                    )
                } else {
                    !matches!(
                        binding.kind,
                        EntityKind::Interface | EntityKind::TypeAlias | EntityKind::TypeParameter
                    )
                };
                if admitted {
                    selected.push(binding.declaration);
                }
            }
            if !selected.is_empty() {
                return Ok(selected);
            }
            scope = self.parents.get(&current).copied();
        }
        Ok(Vec::new())
    }
}

fn is_var_scope(fact: &SyntaxFact) -> bool {
    [
        ".function.scope",
        ".lambda.scope",
        ".method.scope",
        ".static_block.scope",
    ]
    .iter()
    .any(|suffix| fact.syntax_kind().as_str().ends_with(suffix))
}
