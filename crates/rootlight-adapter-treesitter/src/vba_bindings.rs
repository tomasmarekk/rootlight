//! Source-bound VBA procedure and module variable environments.
//! Missing, duplicate or conditional declarations still shadow outer names;
//! runtime dispatch and foreign-name comparison require separate evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind, structural_captured_name_for_fact,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

pub(super) struct VbaBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    bindings: BTreeMap<u64, BTreeMap<String, Option<SymbolId>>>,
    foreign_scopes: BTreeSet<u64>,
    constant_symbols: BTreeSet<SymbolId>,
    maximum_name_bytes: usize,
}

impl<'a> VbaBindings<'a> {
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &[u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self {
            facts: BTreeMap::new(),
            bindings: BTreeMap::new(),
            foreign_scopes: BTreeSet::new(),
            constant_symbols: BTreeSet::new(),
            maximum_name_bytes,
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "vba.identifier.definition" {
                continue;
            }
            let Some(declaration) = result.declaration(fact.parent(), cancellation)? else {
                continue;
            };
            let Some((mut owner, conditional)) = result.scope(fact.parent(), cancellation)? else {
                continue;
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let text = std::str::from_utf8(source.get(start..end).ok_or_else(invalid_capture)?)
                .map_err(|_| invalid_capture())?;
            let name = structural_captured_name_for_fact("vba", fact, text, maximum_name_bytes);
            let symbol = if !conditional
                && matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.parameter.declaration"
                        | "vba.variable.declaration"
                        | "vba.constant.declaration"
                ) {
                symbols.get(&fact.local_id()).copied()
            } else {
                None
            };
            if declaration.syntax_kind().as_str() == "vba.constant.declaration"
                && let Some(symbol) = symbol
            {
                result.constant_symbols.insert(symbol);
            }
            if owns_scope(declaration) {
                // A function/Get name also denotes an implicit result variable.
                // Its written procedure declaration is not that variable's identity.
                if matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.function.declaration" | "vba.get_property.declaration"
                ) {
                    result.insert(owner, name.as_deref(), None);
                }
                let Some((parent, _)) = result.scope(result.fact(owner)?.parent(), cancellation)?
                else {
                    continue;
                };
                owner = parent;
            }
            result.insert(owner, name.as_deref(), symbol);
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        let default_constant =
            fact.syntax_kind().as_str() == "vba.default_constant_identifier.reference";
        if fact.syntax_kind().as_str() != "vba.identifier.reference" && !default_constant {
            return Ok(None);
        }
        let Some(name) =
            structural_captured_name_for_fact("vba", fact, text, self.maximum_name_bytes)
                .filter(|name| name.is_ascii())
        else {
            return Ok(None);
        };
        let key = name.to_ascii_lowercase();
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            let scope = self.fact(owner)?;
            // Parameter defaults are declaration-time constants, outside the
            // procedure body's parameter/local-variable environment.
            if default_constant && scope.syntax_kind().as_str() != "vba.file.scope" {
                parent = scope.parent();
                continue;
            }
            if self.foreign_scopes.contains(&owner) {
                return Ok(None);
            }
            if let Some(symbol) = self.bindings.get(&owner).and_then(|names| names.get(&key)) {
                return Ok(symbol
                    .filter(|symbol| !default_constant || self.constant_symbols.contains(symbol)));
            }
            if scope.syntax_kind().as_str() == "vba.file.scope" {
                return Ok(None);
            }
            parent = scope.parent();
        }
        Err(invalid_capture())
    }

    fn insert(&mut self, owner: u64, name: Option<&str>, symbol: Option<SymbolId>) {
        let Some(name) = name.filter(|name| name.is_ascii()) else {
            // Foreign/undecodable names need the project's comparison profile.
            // Without it, an apparent miss must not expose a same-named outer binding.
            self.foreign_scopes.insert(owner);
            return;
        };
        self.bindings
            .entry(owner)
            .or_default()
            .entry(name.to_ascii_lowercase())
            .and_modify(|existing| *existing = None)
            .or_insert(symbol);
    }

    fn scope(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, bool)>, AdapterError> {
        let mut conditional = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Scope {
                let kind = fact.syntax_kind().as_str();
                if !kind.starts_with("vba.") {
                    return Ok(None);
                }
                if kind == "vba.preprocessor.scope" {
                    conditional = true;
                } else {
                    return Ok(Some((
                        id,
                        conditional || kind.starts_with("vba.conditional_"),
                    )));
                }
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn declaration(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Declaration {
                return Ok(Some(fact));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn fact(&self, id: u64) -> Result<&'a SyntaxFact, AdapterError> {
        self.facts.get(&id).copied().ok_or_else(invalid_capture)
    }
}

fn owns_scope(declaration: &SyntaxFact) -> bool {
    !matches!(
        declaration.syntax_kind().as_str(),
        "vba.parameter.declaration"
            | "vba.variable.declaration"
            | "vba.constant.declaration"
            | "vba.field.declaration"
            | "vba.enum_constant.declaration"
    )
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("vba-binding-capture").expect("built-in VBA diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: u64,
        end: u64,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 17),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "vba.variable.declaration",
                0,
                5,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                0,
                5,
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Scope,
                "vba.sub_function.scope",
                6,
                17,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Declaration,
                "vba.variable.declaration",
                6,
                11,
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                6,
                11,
            ),
            fact(
                7,
                Some(4),
                SyntaxFactKind::Occurrence,
                "vba.identifier.reference",
                12,
                17,
            ),
        ]
    }

    #[test]
    fn unavailable_inner_identity_and_capture_order_preserve_shadowing() {
        let mut facts = facts();
        let inner = SymbolId::from_bytes([8; 20]);
        let outer = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        for _ in 0..2 {
            for present in [false, true] {
                let mut symbols = HashMap::from([(3, outer)]);
                if present {
                    symbols.insert(6, inner);
                }
                let plan =
                    VbaBindings::new(&facts, b"Count count COUNT", &symbols, 64, &cancellation)
                        .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 7).unwrap();
                assert_eq!(
                    plan.resolve(reference, "COUNT", &cancellation).unwrap(),
                    present.then_some(inner)
                );
            }
            facts.reverse();
        }
    }

    #[test]
    fn malformed_ancestry_and_cancelled_work_are_bounded() {
        let cancellation = Cancellation::new();
        for parent in [5, 99] {
            let mut facts = facts();
            facts[4] = fact(
                5,
                Some(parent),
                SyntaxFactKind::Declaration,
                "vba.variable.declaration",
                6,
                11,
            );
            assert!(
                VbaBindings::new(
                    &facts,
                    b"Count count COUNT",
                    &HashMap::new(),
                    64,
                    &cancellation
                )
                .is_err()
            );
        }
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(VbaBindings::new(&[], b"", &HashMap::new(), 64, &cancellation).is_err());
    }
}
