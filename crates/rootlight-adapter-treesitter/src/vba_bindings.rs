//! Source-bound VBA procedure, module and explicit instance-member environments.
//! Missing, duplicate or conditional declarations still shadow outer names;
//! runtime dispatch and foreign-name comparison require separate evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind, structural_captured_name_for_fact,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

#[derive(Clone, Copy)]
enum Binding {
    Value(SymbolId),
    Procedure(SymbolId),
    Function {
        symbol: SymbolId,
        accepts_empty: bool,
    },
    ExternalProcedure(SymbolId),
    FunctionResult(SymbolId),
    PropertyResult(SymbolId),
    Property(PropertyAccessors),
}

#[derive(Clone, Copy, Default)]
struct PropertyAccessors {
    get: Option<SymbolId>,
    let_value: Option<SymbolId>,
    set: Option<SymbolId>,
}

impl PropertyAccessors {
    fn merge(self, other: Self) -> Option<Self> {
        // Accessors share a name, but repeated accessors or mixed declaration
        // kinds are ambiguous regardless of capture order.
        if (self.get.is_some() && other.get.is_some())
            || (self.let_value.is_some() && other.let_value.is_some())
            || (self.set.is_some() && other.set.is_some())
        {
            return None;
        }
        Some(Self {
            get: self.get.or(other.get),
            let_value: self.let_value.or(other.let_value),
            set: self.set.or(other.set),
        })
    }
}

pub(super) struct ResolvedBinding {
    pub(super) symbol: SymbolId,
    pub(super) is_call: bool,
}

pub(super) struct VbaBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    bindings: BTreeMap<u64, BTreeMap<String, Option<Binding>>>,
    foreign_scopes: BTreeSet<u64>,
    constant_symbols: BTreeSet<SymbolId>,
    class_module: Option<&'a SyntaxFact>,
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
            class_module: None,
            maximum_name_bytes,
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "vba.class.module"
                && result.class_module.replace(fact).is_some()
            {
                return Err(invalid_capture());
            }
        }
        let mut required_parameter_scopes = BTreeSet::new();
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "vba.parameter.declaration"
                && let Some((owner, _)) = result.scope(fact.parent(), cancellation)?
            {
                required_parameter_scopes.insert(owner);
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
            let mut symbol = if conditional {
                None
            } else {
                symbols.get(&fact.local_id()).copied().and_then(|symbol| {
                    match declaration.syntax_kind().as_str() {
                        "vba.parameter.declaration"
                        | "vba.omittable_parameter.declaration"
                        | "vba.variable.declaration"
                        | "vba.constant.declaration" => Some(Binding::Value(symbol)),
                        "vba.sub_function.declaration" => Some(Binding::Procedure(symbol)),
                        "vba.function.declaration" => Some(Binding::Function {
                            symbol,
                            accepts_empty: !required_parameter_scopes.contains(&owner),
                        }),
                        "vba.external_sub_function.declaration"
                        | "vba.external_function.declaration" => {
                            Some(Binding::ExternalProcedure(symbol))
                        }
                        "vba.get_property.declaration" => {
                            Some(Binding::Property(PropertyAccessors {
                                get: Some(symbol),
                                ..PropertyAccessors::default()
                            }))
                        }
                        "vba.let_property.declaration" => {
                            Some(Binding::Property(PropertyAccessors {
                                let_value: Some(symbol),
                                ..PropertyAccessors::default()
                            }))
                        }
                        "vba.set_property.declaration" => {
                            Some(Binding::Property(PropertyAccessors {
                                set: Some(symbol),
                                ..PropertyAccessors::default()
                            }))
                        }
                        _ => None,
                    }
                })
            };
            if declaration.syntax_kind().as_str() == "vba.constant.declaration"
                && let Some(Binding::Value(symbol)) = symbol
            {
                result.constant_symbols.insert(symbol);
            }
            if owns_scope(declaration) {
                let Some((parent, conditional)) =
                    result.scope(result.fact(owner)?.parent(), cancellation)?
                else {
                    continue;
                };
                if conditional {
                    symbol = None;
                }
                // MS-VBAL 5.6.2.1 evaluates a bare enclosing Function/Get name
                // as its return value. Keep that declaration reference local;
                // falling through would invent a recursive accessor call.
                if matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.function.declaration" | "vba.get_property.declaration"
                ) {
                    let result_binding = match symbol {
                        Some(Binding::Function { symbol, .. }) => {
                            Some(Binding::FunctionResult(symbol))
                        }
                        Some(Binding::Property(PropertyAccessors {
                            get: Some(symbol), ..
                        })) => Some(Binding::PropertyResult(symbol)),
                        _ => None,
                    };
                    result.insert(owner, name.as_deref(), result_binding);
                }
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
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        cancellation.check()?;
        let default_constant =
            fact.syntax_kind().as_str() == "vba.default_constant_identifier.reference";
        let self_member = matches!(
            fact.syntax_kind().as_str(),
            "vba.self_member.reference"
                | "vba.self_invoked_member.reference"
                | "vba.self_get_member.reference"
                | "vba.self_let_member.reference"
                | "vba.self_set_member.reference"
                | "vba.self_invoked_get_member.reference"
                | "vba.self_invoked_let_member.reference"
                | "vba.self_invoked_set_member.reference"
        );
        let invoked = matches!(
            fact.syntax_kind().as_str(),
            "vba.invoked_identifier.reference"
                | "vba.invoked_get_identifier.reference"
                | "vba.self_invoked_member.reference"
                | "vba.self_invoked_get_member.reference"
        );
        let value_use = matches!(
            fact.syntax_kind().as_str(),
            "vba.value_identifier.reference" | "vba.self_get_member.reference"
        );
        let assignment = matches!(
            fact.syntax_kind().as_str(),
            "vba.let_identifier.reference"
                | "vba.set_identifier.reference"
                | "vba.invoked_let_identifier.reference"
                | "vba.invoked_set_identifier.reference"
        );
        if fact.syntax_kind().as_str() != "vba.identifier.reference"
            && !assignment
            && !value_use
            && !default_constant
            && !invoked
            && !self_member
        {
            return Ok(None);
        }
        // A type character requires comparison with the declared member type;
        // spelling normalization alone cannot prove that compatibility.
        if self_member && text.ends_with(['$', '%', '&', '#', '@', '^', '!']) {
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
        let mut instance_context = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            let scope = self.fact(owner)?;
            if self_member {
                if scope.syntax_kind().as_str() != "vba.file.scope" {
                    instance_context |= matches!(
                        scope.syntax_kind().as_str(),
                        "vba.sub_function.scope"
                            | "vba.function.scope"
                            | "vba.get_property.scope"
                            | "vba.let_property.scope"
                            | "vba.set_property.scope"
                            | "vba.conditional_sub_function.scope"
                            | "vba.conditional_function.scope"
                            | "vba.conditional_property.scope"
                    );
                    // Explicit Me lookup bypasses procedure-local shadowing.
                    parent = scope.parent();
                    continue;
                }
                if !instance_context
                    || !self
                        .class_module
                        .is_some_and(|class| class.span() == scope.span())
                {
                    return Ok(None);
                }
            }
            // Parameter defaults are declaration-time constants, outside the
            // procedure body's parameter/local-variable environment.
            if default_constant && scope.syntax_kind().as_str() != "vba.file.scope" {
                parent = scope.parent();
                continue;
            }
            if self.foreign_scopes.contains(&owner) {
                return Ok(None);
            }
            if let Some(binding) = self.bindings.get(&owner).and_then(|names| names.get(&key)) {
                return Ok(match binding {
                    Some(Binding::FunctionResult(symbol) | Binding::PropertyResult(symbol))
                        if matches!(
                            fact.syntax_kind().as_str(),
                            "vba.value_identifier.reference"
                                | "vba.let_identifier.reference"
                                | "vba.set_identifier.reference"
                        ) && !text.ends_with(['$', '%', '&', '#', '@', '^', '!']) =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: false,
                        })
                    }
                    Some(Binding::Value(symbol))
                        if (!default_constant || self.constant_symbols.contains(symbol))
                            && (!self_member || !self.constant_symbols.contains(symbol)) =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: false,
                        })
                    }
                    Some(Binding::Procedure(symbol) | Binding::FunctionResult(symbol))
                        if invoked =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: true,
                        })
                    }
                    Some(Binding::ExternalProcedure(symbol)) if invoked && !self_member => {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: true,
                        })
                    }
                    Some(Binding::Function {
                        symbol,
                        accepts_empty,
                    }) if invoked
                        || (value_use
                            && *accepts_empty
                            && !text.ends_with(['$', '%', '&', '#', '@', '^', '!'])) =>
                    {
                        // Only a complete native parameter inventory admits an
                        // empty argument list. The function's own result binding
                        // remains distinct from this implicit call.
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: true,
                        })
                    }
                    Some(Binding::Property(accessors))
                        if !text.ends_with(['$', '%', '&', '#', '@', '^', '!']) =>
                    {
                        let symbol = match fact.syntax_kind().as_str() {
                            "vba.self_get_member.reference"
                            | "vba.value_identifier.reference"
                            | "vba.invoked_get_identifier.reference"
                            | "vba.self_invoked_get_member.reference" => accessors.get,
                            "vba.self_let_member.reference"
                            | "vba.let_identifier.reference"
                            | "vba.invoked_let_identifier.reference"
                            | "vba.self_invoked_let_member.reference" => accessors.let_value,
                            "vba.self_set_member.reference"
                            | "vba.set_identifier.reference"
                            | "vba.invoked_set_identifier.reference"
                            | "vba.self_invoked_set_member.reference" => accessors.set,
                            _ => None,
                        };
                        symbol.map(|symbol| ResolvedBinding {
                            symbol,
                            is_call: true,
                        })
                    }
                    _ => None,
                });
            }
            if scope.syntax_kind().as_str() == "vba.file.scope" {
                return Ok(None);
            }
            parent = scope.parent();
        }
        Err(invalid_capture())
    }

    fn insert(&mut self, owner: u64, name: Option<&str>, symbol: Option<Binding>) {
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
            .and_modify(|existing| {
                *existing = match (*existing, symbol) {
                    (Some(Binding::Property(left)), Some(Binding::Property(right))) => {
                        left.merge(right).map(Binding::Property)
                    }
                    _ => None,
                };
            })
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
            | "vba.omittable_parameter.declaration"
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
    fn function_parameter_inventory_is_order_independent_without_parameter_symbols() {
        let symbol = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        for label in [
            "vba.parameter.declaration",
            "vba.omittable_parameter.declaration",
        ] {
            let mut facts = vec![
                fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 17),
                fact(
                    2,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "vba.function.scope",
                    0,
                    11,
                ),
                fact(
                    3,
                    Some(2),
                    SyntaxFactKind::Declaration,
                    "vba.function.declaration",
                    0,
                    11,
                ),
                fact(
                    4,
                    Some(3),
                    SyntaxFactKind::Occurrence,
                    "vba.identifier.definition",
                    0,
                    5,
                ),
                fact(5, Some(3), SyntaxFactKind::Declaration, label, 6, 11),
                fact(
                    6,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "vba.sub_function.scope",
                    12,
                    17,
                ),
                fact(
                    7,
                    Some(6),
                    SyntaxFactKind::Occurrence,
                    "vba.value_identifier.reference",
                    12,
                    17,
                ),
            ];
            for _ in 0..2 {
                let plan = VbaBindings::new(
                    &facts,
                    b"Count input COUNT",
                    &HashMap::from([(4, symbol)]),
                    64,
                    &cancellation,
                )
                .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 7).unwrap();
                assert_eq!(
                    plan.resolve(reference, "COUNT", &cancellation)
                        .unwrap()
                        .map(|binding| (binding.symbol, binding.is_call)),
                    (label == "vba.omittable_parameter.declaration").then_some((symbol, true))
                );
                facts.reverse();
            }
        }
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
                    plan.resolve(reference, "COUNT", &cancellation)
                        .unwrap()
                        .map(|binding| binding.symbol),
                    present.then_some(inner)
                );
            }
            facts.reverse();
        }
    }

    #[test]
    fn self_members_require_matching_class_scope_and_instance_context() {
        let outer = SymbolId::from_bytes([9; 20]);
        let inner = SymbolId::from_bytes([8; 20]);
        for (class_end, parent, expected) in [
            (None, 4, None),
            (Some(17), 4, Some(outer)),
            (Some(16), 4, None),
            (Some(17), 1, None),
        ] {
            let mut facts = facts();
            facts[6] = fact(
                7,
                Some(parent),
                SyntaxFactKind::Occurrence,
                "vba.self_member.reference",
                12,
                17,
            );
            if let Some(end) = class_end {
                facts.push(fact(
                    8,
                    None,
                    SyntaxFactKind::Module,
                    "vba.class.module",
                    0,
                    end,
                ));
            }
            let cancellation = Cancellation::new();
            let plan = VbaBindings::new(
                &facts,
                b"Count count COUNT",
                &HashMap::from([(3, outer), (6, inner)]),
                64,
                &cancellation,
            )
            .unwrap();
            assert_eq!(
                plan.resolve(&facts[6], "COUNT", &cancellation)
                    .unwrap()
                    .map(|binding| binding.symbol),
                expected
            );
        }
    }

    #[test]
    fn property_capture_order_and_missing_identities_preserve_barriers() {
        let getter = SymbolId::from_bytes([8; 20]);
        let setter = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 14),
            fact(2, None, SyntaxFactKind::Module, "vba.class.module", 0, 14),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Scope,
                "vba.get_property.scope",
                0,
                4,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Declaration,
                "vba.get_property.declaration",
                0,
                4,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                0,
                4,
            ),
            fact(
                6,
                Some(1),
                SyntaxFactKind::Scope,
                "vba.let_property.scope",
                5,
                9,
            ),
            fact(
                7,
                Some(6),
                SyntaxFactKind::Declaration,
                "vba.let_property.declaration",
                5,
                9,
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                5,
                9,
            ),
            fact(
                9,
                Some(1),
                SyntaxFactKind::Scope,
                "vba.sub_function.scope",
                10,
                14,
            ),
            fact(
                10,
                Some(9),
                SyntaxFactKind::Occurrence,
                "vba.self_get_member.reference",
                10,
                14,
            ),
        ];
        for _ in 0..2 {
            for present in [false, true] {
                let mut symbols = HashMap::from([(5, getter)]);
                if present {
                    symbols.insert(8, setter);
                }
                let plan = VbaBindings::new(&facts, b"Item Item ITEM", &symbols, 64, &cancellation)
                    .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 10).unwrap();
                assert_eq!(
                    plan.resolve(reference, "ITEM", &cancellation)
                        .unwrap()
                        .map(|binding| binding.symbol),
                    present.then_some(getter)
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
