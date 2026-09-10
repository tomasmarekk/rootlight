//! Source-proven Nix lexical identities over parser-independent captures.
//! Binding identities do not evaluate attributes, imports or callable values;
//! unavailable inner declarations must never expose an enclosing namesake.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

pub(super) struct NixBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    bindings: BTreeMap<(u64, &'a str), Option<SymbolId>>,
    unmodeled_scopes: BTreeSet<u64>,
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
            bindings: BTreeMap::new(),
            unmodeled_scopes: BTreeSet::new(),
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "nix.dynamic_path_variable.declaration" | "nix.dynamic_path_function.declaration"
            ) {
                // A computed leaf still introduces its static path root. That
                // implicit owner has no materialized definition identity yet.
                if let Some(scope) = result.nearest_boundary(fact.parent(), cancellation)?
                    && introduces_bindings(result.fact(scope)?)
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
            if !introduces_bindings(result.fact(scope)?) {
                continue;
            }
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
            // A quoted key or path may introduce the same root as a bare name.
            // Until that equivalence is modeled, this scope cannot prove absence
            // of a shadowing declaration, even when its raw spelling differs.
            if bytes.len() > maximum_name_bytes || !bare_identifier(bytes) {
                result.unmodeled_scopes.insert(scope);
                continue;
            }
            let name = std::str::from_utf8(bytes).map_err(|_| invalid_capture())?;
            result
                .bindings
                .entry((scope, name))
                .and_modify(|binding| *binding = None)
                .or_insert_with(|| symbols.get(&fact.local_id()).copied());
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if !matches!(
            fact.syntax_kind().as_str(),
            "nix.identifier.reference" | "nix.inherited_name.reference"
        ) || !bare_identifier(name.as_bytes())
        {
            return Ok(None);
        }
        let mut scope = self.nearest_boundary(fact.parent(), cancellation)?;
        if fact.syntax_kind().as_str() == "nix.inherited_name.reference"
            && let Some(owner) = scope
        {
            // Bare inherit reads the environment outside the containing set/let,
            // whereas inherit (expr) evaluates expr in the ordinary environment.
            scope = self.nearest_boundary(self.fact(owner)?.parent(), cancellation)?;
        }
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if self.unmodeled_scopes.contains(&current) {
                return Ok(None);
            }
            if let Some(binding) = self.bindings.get(&(current, name)) {
                return Ok(*binding);
            }
            let owner = self.fact(current)?;
            if owner.syntax_kind().as_str() == "nix.file.module" {
                // Embedded examples are separate Nix units, not one shared scope.
                return Ok(None);
            }
            scope = self.nearest_boundary(owner.parent(), cancellation)?;
        }
        Err(invalid_capture())
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
            let fact = self.fact(id)?;
            if introduces_bindings(fact)
                || matches!(
                    fact.syntax_kind().as_str(),
                    "nix.attrset.scope" | "nix.file.module"
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
        "nix.function.scope" | "nix.let.scope" | "nix.rec_attrset.scope"
    )
}

fn bare_identifier(bytes: &[u8]) -> bool {
    bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'\''))
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
}
