//! Source-proven Nix lexical and attribute identities over native captures.
//! Written value flow does not execute imports or callable values;
//! unavailable inner declarations must never expose an enclosing namesake.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::nix_static_attribute_name as static_binding_name;
use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

mod selections;

pub(super) struct NixBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    attributes: crate::nix_attributes::Attributes,
    bindings: BTreeMap<(u64, Cow<'a, str>), Option<SymbolId>>,
    unmodeled_scopes: BTreeSet<u64>,
    maximum_name_bytes: usize,
    selected: HashMap<u64, SymbolId>,
}

impl<'a> NixBindings<'a> {
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self {
            facts: BTreeMap::new(),
            attributes: crate::nix_attributes::Attributes::build(
                facts,
                source,
                maximum_name_bytes,
                cancellation,
            )?,
            bindings: BTreeMap::new(),
            unmodeled_scopes: BTreeSet::new(),
            maximum_name_bytes,
            selected: HashMap::new(),
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
        }
        result
            .unmodeled_scopes
            .extend(result.attributes.unknown.iter().copied());
        for (&id, group) in &result.attributes.groups {
            cancellation.check()?;
            if group.name == "__overrides"
                && (result
                    .attributes
                    .groups
                    .get(&group.parent)
                    .is_some_and(|parent| parent.recursive)
                    || result.facts.get(&group.parent).is_some_and(|parent| {
                        matches!(
                            parent.syntax_kind().as_str(),
                            "nix.rec_attrset.scope" | "nix.bound_rec_attrset.scope"
                        )
                    }))
            {
                // Nix evaluates __overrides before exposing a recursive set's
                // fields and can replace even its already-written lexical values.
                result.unmodeled_scopes.insert(group.parent);
            }
            if !group.valid {
                result.unmodeled_scopes.insert(id);
            }
            if !result.binding_scope(group.parent)? {
                continue;
            }
            let mut targets = group
                .members
                .iter()
                .map(|member| symbols.get(&member.definition).copied());
            let first = targets.next().flatten();
            let target = if group.valid && targets.all(|target| target == first) {
                first
            } else {
                None
            };
            result
                .bindings
                .insert((group.parent, Cow::Owned(group.name.clone())), target);
        }
        for fact in facts {
            cancellation.check()?;
            if result.attributes.owners.contains(&fact.local_id())
                || fact
                    .parent()
                    .is_some_and(|owner| result.attributes.owners.contains(&owner))
            {
                continue;
            }
            if matches!(
                fact.syntax_kind().as_str(),
                "nix.dynamic_path_variable.declaration" | "nix.dynamic_path_function.declaration"
            ) {
                // Old or incomplete providers may omit the native path parts.
                // Without a root identity, a computed suffix must not expose an
                // enclosing namesake.
                if let Some(scope) = result.nearest_boundary(fact.parent(), cancellation)?
                    && result.binding_scope(scope)?
                {
                    result.unmodeled_scopes.insert(scope);
                }
                continue;
            }
            if fact.syntax_kind().as_str() != "nix.binding_name.definition" {
                continue;
            }
            let Some(scope) = result.nearest_boundary(fact.parent(), cancellation)? else {
                continue;
            };
            if !result.binding_scope(scope)? {
                continue;
            }
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
            let written = std::str::from_utf8(bytes).map_err(|_| invalid_capture())?;
            // Qualified paths without native parts remain unmodeled. An
            // unavailable name cannot prove the absence of a shadowing binding.
            let Some(name) = static_binding_name(written, maximum_name_bytes, cancellation)? else {
                result.unmodeled_scopes.insert(scope);
                continue;
            };
            result
                .bindings
                .entry((scope, name))
                .and_modify(|binding| *binding = None)
                .or_insert_with(|| symbols.get(&fact.local_id()).copied());
        }
        result.selected = selections::resolve(&result, source, symbols, cancellation)?;
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "nix.selected_attribute.reference" {
            return Ok(self.selected.get(&fact.local_id()).copied());
        }
        if !matches!(
            fact.syntax_kind().as_str(),
            "nix.identifier.reference" | "nix.inherited_name.reference"
        ) {
            return Ok(None);
        }
        let Some(name) = static_binding_name(name, self.maximum_name_bytes, cancellation)? else {
            return Ok(None);
        };
        self.lookup(
            fact,
            &name,
            fact.syntax_kind().as_str() == "nix.inherited_name.reference",
            cancellation,
        )
    }

    fn lookup(
        &self,
        fact: &SyntaxFact,
        name: &str,
        inherited: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        let mut scope = self.nearest_boundary(fact.parent(), cancellation)?;
        if inherited && let Some(owner) = scope {
            // Bare inherit reads the environment outside the containing set/let,
            // whereas inherit (expr) evaluates expr in the ordinary environment.
            scope = self.outer_boundary(owner, cancellation)?;
        }
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if self.unmodeled_scopes.contains(&current) {
                return Ok(None);
            }
            if let Some(binding) = self.bindings.get(&(current, Cow::Borrowed(name))) {
                return Ok(*binding);
            }
            if self
                .facts
                .get(&current)
                .is_some_and(|owner| owner.syntax_kind().as_str() == "nix.file.module")
            {
                // Embedded examples are separate Nix units, not one shared scope.
                return Ok(None);
            }
            scope = self.outer_boundary(current, cancellation)?;
        }
        Err(invalid_capture())
    }

    fn binding_scope(&self, id: u64) -> Result<bool, AdapterError> {
        if let Some(group) = self.attributes.groups.get(&id) {
            return Ok(group.recursive && group.valid);
        }
        Ok(introduces_bindings(self.fact(id)?))
    }

    fn outer_boundary(
        &self,
        id: u64,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        if let Some(group) = self.attributes.groups.get(&id) {
            return Ok(Some(group.parent));
        }
        self.nearest_boundary(self.fact(id)?.parent(), cancellation)
    }

    fn nearest_boundary(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            if let Some(&scope) = self
                .attributes
                .scopes
                .get(&id)
                .or_else(|| self.attributes.values.get(&id))
            {
                return Ok(Some(scope));
            }
            let fact = self.fact(id)?;
            if introduces_bindings(fact)
                || matches!(
                    fact.syntax_kind().as_str(),
                    "nix.attrset.scope" | "nix.bound_attrset.scope" | "nix.file.module"
                )
            {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn fact(&self, id: u64) -> Result<&'a SyntaxFact, AdapterError> {
        self.facts.get(&id).copied().ok_or_else(invalid_capture)
    }
}

fn introduces_bindings(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "nix.function.scope"
            | "nix.let.scope"
            | "nix.rec_attrset.scope"
            | "nix.bound_rec_attrset.scope"
    )
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("nix-binding-capture")
            .expect("built-in Nix capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::{SyntaxFactKind, SyntaxKindLabel};
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;

    #[test]
    fn static_names_follow_nix_escapes_without_evaluating_paths_or_interpolations() {
        for (written, expected) in [
            ("name", Some("name")),
            (r#""name""#, Some("name")),
            (r#""\name""#, Some("\name")),
            (r#""\x""#, Some("x")),
            (r#""\u0078""#, Some("u0078")),
            (r#""\t\r\n\\\"""#, Some("\t\r\n\\\"")),
            (r#""λ😀""#, Some("λ😀")),
            (r#""\λ😀""#, Some("λ😀")),
            (r#""\${literal}""#, Some("${literal}")),
            ("\"a\r\nb\rc\nd\"", Some("a\nb\nc\nd")),
            ("\"a\\\r\nb\"", Some("a\r\nb")),
            (r#""""#, Some("")),
            (r#""a.b""#, Some("a.b")),
            ("a.b", None),
            (r#"a."b""#, None),
            (r#""a".b"#, None),
            (r#""${value}""#, None),
            ("\"unterminated", None),
            ("\"trailing\\\"", None),
            ("\"nul\0\"", None),
            ("\"escaped\\\0\"", None),
        ] {
            assert_eq!(
                static_binding_name(written, 64, &Cancellation::new())
                    .unwrap()
                    .as_deref(),
                expected,
                "{written:?}",
            );
        }
        assert!(
            static_binding_name(r#""\x""#, 3, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            static_binding_name("", 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn equivalent_quoted_names_block_duplicate_or_missing_inner_identities() {
        let outer = SymbolId::from_bytes([8; 20]);
        let inner = SymbolId::from_bytes([9; 20]);
        let source = br#"x "\x" x x"#;
        for (duplicate, present, limit) in [
            (false, true, 64),
            (false, false, 64),
            (true, true, 64),
            (false, true, 3),
        ] {
            let mut facts = vec![
                fact(1, None, "nix.let.scope", 0, 10),
                fact(2, Some(1), "nix.binding_name.definition", 0, 1),
                fact(3, Some(1), "nix.let.scope", 2, 10),
                fact(4, Some(3), "nix.binding_name.definition", 2, 6),
                fact(5, Some(3), "nix.identifier.reference", 9, 10),
            ];
            if duplicate {
                facts.push(fact(6, Some(3), "nix.binding_name.definition", 7, 8));
            }
            let mut symbols = HashMap::from([(2, outer), (6, inner)]);
            if present {
                symbols.insert(4, inner);
            }
            for _ in 0..2 {
                let plan = NixBindings::new(&facts, source, &symbols, limit, &Cancellation::new())
                    .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 5).unwrap();
                assert_eq!(
                    plan.resolve(reference, "x", &Cancellation::new()).unwrap(),
                    (present && !duplicate && limit == 64).then_some(inner)
                );
                facts.reverse();
            }
        }
    }

    fn fact(id: u64, parent: Option<u64>, label: &str, start: u64, end: u64) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            if label.ends_with("scope") {
                SyntaxFactKind::Scope
            } else {
                SyntaxFactKind::Occurrence
            },
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, "nix.let.scope", 0, 8),
            fact(2, Some(1), "nix.binding_name.definition", 0, 1),
            fact(3, Some(1), "nix.let.scope", 2, 8),
            fact(4, Some(3), "nix.binding_name.definition", 2, 3),
            fact(5, Some(3), "nix.identifier.reference", 4, 5),
        ]
    }

    #[test]
    fn missing_or_ambiguous_inner_identities_never_expose_outer_bindings() {
        let outer = SymbolId::from_bytes([8; 20]);
        let inner = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        for (duplicate, present) in [(false, false), (false, true), (true, true)] {
            let mut facts = facts();
            if duplicate {
                facts.push(fact(6, Some(3), "nix.binding_name.definition", 2, 3));
            }
            let mut symbols = HashMap::from([(2, outer)]);
            if present {
                symbols.insert(4, inner);
            }
            for _ in 0..2 {
                let plan =
                    NixBindings::new(&facts, b"x x x   ", &symbols, 64, &cancellation).unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 5).unwrap();
                assert_eq!(
                    plan.resolve(reference, "x", &cancellation).unwrap(),
                    (present && !duplicate).then_some(inner),
                );
                facts.reverse();
            }
        }
    }

    #[test]
    fn malformed_capture_graphs_fail_without_unbounded_ancestry_work() {
        let cancellation = Cancellation::new();
        for bad in [
            fact(2, Some(1), "nix.binding_name.definition", 0, 1),
            fact(6, Some(99), "nix.binding_name.definition", 0, 1),
            fact(6, Some(6), "nix.binding_name.definition", 0, 1),
            fact(6, Some(3), "nix.binding_name.definition", 9, 10),
        ] {
            let mut facts = facts();
            facts.push(bad);
            assert!(matches!(
                NixBindings::new(&facts, b"x x x   ", &HashMap::new(), 64, &cancellation),
                Err(AdapterError::ProviderFailed { .. }),
            ));
        }
    }

    #[test]
    fn cancellation_stops_empty_planning_and_existing_resolution() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            NixBindings::new(&[], b"", &HashMap::new(), 64, &cancellation),
            Err(AdapterError::Cancelled { .. }),
        ));
        let facts = facts();
        let plan = NixBindings::new(
            &facts,
            b"x x x   ",
            &HashMap::new(),
            64,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(
            plan.resolve(&facts[4], "x", &cancellation),
            Err(AdapterError::Cancelled { .. }),
        ));
    }

    #[test]
    fn selected_capture_cycles_and_missing_owners_are_rejected() {
        for parent in [7, 99] {
            let captures = [fact(
                7,
                Some(parent),
                "nix.selected_attribute.reference",
                0,
                1,
            )];
            assert!(matches!(
                NixBindings::new(&captures, b"x", &HashMap::new(), 64, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
        for label in ["nix.selection.scope", "nix.attrset.scope"] {
            let captures = [
                fact(1, None, label, 0, 3),
                fact(2, None, label, 0, 3),
                fact(3, Some(1), "nix.selected_attribute.reference", 1, 2),
            ];
            assert!(matches!(
                NixBindings::new(&captures, b"xxx", &HashMap::new(), 64, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
        let captures = [
            fact(1, None, "nix.selection.scope", 0, 3),
            fact(2, Some(1), "nix.selected_attribute.reference", 0, 1),
            fact(3, Some(1), "nix.selected_attribute.reference", 0, 2),
        ];
        assert!(matches!(
            NixBindings::new(&captures, b"xxx", &HashMap::new(), 64, &Cancellation::new()),
            Err(AdapterError::ProviderFailed { .. })
        ));
        let bindings =
            NixBindings::new(&[], b"", &HashMap::new(), 64, &Cancellation::new()).unwrap();
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            selections::resolve(&bindings, b"", &HashMap::new(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
