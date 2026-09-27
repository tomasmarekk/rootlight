//! Source-bound VBA procedure, module, declared-type and member environments.
//! Missing, duplicate or conditional declarations still shadow outer names;
//! runtime dispatch and foreign-name comparison require separate evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind, structural_captured_name_for_fact,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::EntityVisibility;

#[derive(Clone, Copy)]
enum Binding {
    Value(SymbolId),
    EnumMember {
        symbol: SymbolId,
        defined_at: u64,
    },
    EnumType {
        symbol: SymbolId,
        scope: u64,
        defined_at: u64,
    },
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

enum ConditionalBinding<'a> {
    Absent,
    Value(&'a Option<Binding>),
    Ambiguous,
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

#[derive(Clone, Copy)]
struct DeclaredType {
    symbol: SymbolId,
    field_scope: Option<u64>,
}

struct StatementLabel {
    definition: u64,
    symbol: Option<SymbolId>,
}

pub(super) struct VbaBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    source: &'a [u8],
    applications: BTreeMap<u64, Vec<&'a SyntaxFact>>,
    invocations: BTreeMap<u64, Vec<&'a SyntaxFact>>,
    array_only_invocations: BTreeMap<u64, Vec<&'a SyntaxFact>>,
    member_references: BTreeMap<u64, u64>,
    callable_scopes: BTreeMap<SymbolId, u64>,
    named_parameters: BTreeMap<u64, BTreeMap<String, Option<SymbolId>>>,
    property_value_symbols: BTreeSet<SymbolId>,
    incomplete_parameter_scopes: BTreeSet<u64>,
    bindings: BTreeMap<u64, BTreeMap<String, Option<Binding>>>,
    // MS-VBAL 5.2.4.3 permits events to share names with ordinary members.
    // Their ambiguity and foreign-name barriers must not cross namespaces.
    events: BTreeMap<u64, BTreeMap<String, Option<SymbolId>>>,
    foreign_event_scopes: BTreeSet<u64>,
    statement_labels: BTreeMap<u64, BTreeMap<String, Vec<StatementLabel>>>,
    foreign_label_scopes: BTreeSet<u64>,
    branch_bindings: BTreeMap<u64, BTreeMap<String, Option<Binding>>>,
    // A shared None also represents conditional shadowing, so it cannot tell
    // whether a same-named unconditional declaration makes a branch ambiguous.
    unconditional_names: BTreeMap<u64, BTreeSet<String>>,
    foreign_scopes: BTreeSet<u64>,
    constant_symbols: BTreeSet<SymbolId>,
    array_symbols: BTreeSet<SymbolId>,
    class_module: Option<&'a SyntaxFact>,
    class_symbol: Option<SymbolId>,
    procedural_module: Option<(String, SymbolId)>,
    maximum_name_bytes: usize,
    types: BTreeMap<u64, BTreeMap<String, Option<DeclaredType>>>,
    foreign_type_scopes: BTreeSet<u64>,
    fields: BTreeMap<u64, BTreeMap<String, Option<SymbolId>>>,
    foreign_field_scopes: BTreeSet<u64>,
    value_types: HashMap<SymbolId, u64>,
    visibilities: BTreeMap<u64, EntityVisibility>,
}

impl<'a> VbaBindings<'a> {
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
            source,
            applications: BTreeMap::new(),
            invocations: BTreeMap::new(),
            array_only_invocations: BTreeMap::new(),
            member_references: BTreeMap::new(),
            callable_scopes: BTreeMap::new(),
            named_parameters: BTreeMap::new(),
            property_value_symbols: BTreeSet::new(),
            incomplete_parameter_scopes: BTreeSet::new(),
            bindings: BTreeMap::new(),
            events: BTreeMap::new(),
            foreign_event_scopes: BTreeSet::new(),
            statement_labels: BTreeMap::new(),
            foreign_label_scopes: BTreeSet::new(),
            branch_bindings: BTreeMap::new(),
            unconditional_names: BTreeMap::new(),
            foreign_scopes: BTreeSet::new(),
            constant_symbols: BTreeSet::new(),
            array_symbols: BTreeSet::new(),
            class_module: None,
            class_symbol: None,
            procedural_module: None,
            maximum_name_bytes,
            types: BTreeMap::new(),
            foreign_type_scopes: BTreeSet::new(),
            fields: BTreeMap::new(),
            foreign_field_scopes: BTreeSet::new(),
            value_types: HashMap::new(),
            visibilities: BTreeMap::new(),
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
            match fact.syntax_kind().as_str() {
                "vba.application.reference" => result
                    .applications
                    .entry(fact.span().start_byte())
                    .or_default()
                    .push(fact),
                "vba.invoked_identifier.reference"
                | "vba.invoked_get_identifier.reference"
                | "vba.self_invoked_member.reference"
                | "vba.self_invoked_get_member.reference"
                | "vba.self_invoked_let_member.reference"
                | "vba.self_invoked_set_member.reference" => {
                    // Named arguments use the exact callee span. Self array
                    // applications additionally need the receiver-start index below.
                    result
                        .invocations
                        .entry(fact.span().start_byte())
                        .or_default()
                        .push(fact);
                }
                "vba.invoked_let_identifier.reference" | "vba.invoked_set_identifier.reference" => {
                    result
                        .array_only_invocations
                        .entry(fact.span().start_byte())
                        .or_default()
                        .push(fact);
                }
                "vba.member.reference" | "vba.invoked_member.reference" => {
                    result
                        .member_references
                        .entry(fact.span().start_byte())
                        .and_modify(|end| *end = (*end).max(fact.span().end_byte()))
                        .or_insert(fact.span().end_byte());
                }
                _ => {}
            }
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "vba.class.module"
                && result.class_module.replace(fact).is_some()
            {
                return Err(invalid_capture());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if is_self_array_invocation(fact)
                && let Some(start) = result.array_invocation_start(fact)?
            {
                result
                    .array_only_invocations
                    .entry(start)
                    .or_default()
                    .push(fact);
            }
        }
        result.class_symbol = result
            .class_module
            .and_then(|fact| symbols.get(&fact.local_id()).copied());
        if result.class_module.is_none()
            && facts
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.file.module")
                .count()
                == 1
        {
            let mut names = facts
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.module_name.definition");
            if let Some(name) = names.next()
                && names.next().is_none()
                && let Some(symbol) = symbols.get(&name.local_id()).copied()
                && let (Ok(start), Ok(end)) = (
                    usize::try_from(name.span().start_byte()),
                    usize::try_from(name.span().end_byte()),
                )
                && let Some(text) = source
                    .get(start..end)
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                && let Some(key) =
                    structural_captured_name_for_fact("vba", name, text, maximum_name_bytes)
                        .and_then(|name| member_key(&name))
            {
                result.procedural_module = Some((key, symbol));
            }
        }
        let mut required_parameter_scopes = BTreeSet::new();
        let mut property_value_parameters = BTreeMap::<u64, (u64, u64)>::new();
        let mut enum_module_bindings = Vec::new();
        for fact in facts {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
            ) && let Some((owner, _)) = result.scope(fact.parent(), cancellation)?
            {
                if fact.syntax_kind().as_str() == "vba.parameter.declaration" {
                    required_parameter_scopes.insert(owner);
                }
                if matches!(
                    result.fact(owner)?.syntax_kind().as_str(),
                    "vba.let_property.scope" | "vba.set_property.scope"
                ) {
                    let candidate = (fact.span().start_byte(), fact.local_id());
                    property_value_parameters
                        .entry(owner)
                        .and_modify(|last| {
                            if candidate.0 > last.0 {
                                *last = candidate;
                            }
                        })
                        .or_insert(candidate);
                }
            }
        }
        for fact in facts {
            cancellation.check()?;
            if !matches!(
                fact.syntax_kind().as_str(),
                "vba.identifier.definition" | "vba.statement_label.definition"
            ) {
                continue;
            }
            let Some(declaration) = result.declaration(fact.parent(), cancellation)? else {
                continue;
            };
            let Some((mut owner, mut conditional)) = result.scope(fact.parent(), cancellation)?
            else {
                continue;
            };
            let declaration_scope = owner;
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let text = std::str::from_utf8(source.get(start..end).ok_or_else(invalid_capture)?)
                .map_err(|_| invalid_capture())?;
            let name = structural_captured_name_for_fact("vba", fact, text, maximum_name_bytes);
            if declaration.syntax_kind().as_str() == "vba.statement_label.declaration" {
                // Labels belong to the procedure, independently of ordinary
                // names and nested executable blocks (MS-VBAL 5.4.1.1).
                if is_instance_procedure_scope(result.fact(owner)?) {
                    if let Some(key) = (text.len() <= maximum_name_bytes)
                        .then(|| statement_label_key(text))
                        .flatten()
                    {
                        result
                            .statement_labels
                            .entry(owner)
                            .or_default()
                            .entry(key)
                            .or_default()
                            .push(StatementLabel {
                                definition: fact.local_id(),
                                symbol: symbols.get(&fact.local_id()).copied(),
                            });
                    } else {
                        result.foreign_label_scopes.insert(owner);
                    }
                }
                continue;
            }
            let candidate = symbols.get(&fact.local_id()).copied().and_then(|symbol| {
                match declaration.syntax_kind().as_str() {
                    "vba.parameter.declaration"
                    | "vba.omittable_parameter.declaration"
                    | "vba.variable.declaration"
                    | "vba.constant.declaration" => Some(Binding::Value(symbol)),
                    "vba.enum_constant.declaration" => Some(Binding::EnumMember {
                        symbol,
                        defined_at: declaration.span().start_byte(),
                    }),
                    "vba.enum.declaration" => Some(Binding::EnumType {
                        symbol,
                        scope: owner,
                        defined_at: declaration.span().start_byte(),
                    }),
                    "vba.sub_function.declaration" => Some(Binding::Procedure(symbol)),
                    "vba.function.declaration" => Some(Binding::Function {
                        symbol,
                        accepts_empty: !required_parameter_scopes.contains(&owner),
                    }),
                    "vba.external_sub_function.declaration"
                    | "vba.external_function.declaration" => {
                        Some(Binding::ExternalProcedure(symbol))
                    }
                    "vba.get_property.declaration" => Some(Binding::Property(PropertyAccessors {
                        get: Some(symbol),
                        ..PropertyAccessors::default()
                    })),
                    "vba.let_property.declaration" => Some(Binding::Property(PropertyAccessors {
                        let_value: Some(symbol),
                        ..PropertyAccessors::default()
                    })),
                    "vba.set_property.declaration" => Some(Binding::Property(PropertyAccessors {
                        set: Some(symbol),
                        ..PropertyAccessors::default()
                    })),
                    _ => None,
                }
            });
            // Only the declaration's array designator proves indexed identity;
            // a later ReDim or runtime Variant value cannot supply this proof.
            if let Some(Binding::Value(symbol)) = candidate
                && declaration_has_array_designator(declaration, fact, source)?
            {
                result.array_symbols.insert(symbol);
            }
            let mut symbol = if conditional { None } else { candidate };
            if matches!(
                declaration.syntax_kind().as_str(),
                "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
            ) {
                let parameter = match symbol {
                    Some(Binding::Value(symbol)) => Some(symbol),
                    _ => None,
                };
                if let (Some(name), Some(parameter)) =
                    (name.as_deref().filter(|name| name.is_ascii()), parameter)
                {
                    // A setter's final parameter is supplied by the assignment
                    // RHS, not by named arguments in the indexed property target.
                    if property_value_parameters
                        .get(&owner)
                        .is_some_and(|(_, id)| *id == declaration.local_id())
                    {
                        result.property_value_symbols.insert(parameter);
                    }
                    let names = result.named_parameters.entry(owner).or_default();
                    let duplicate = names
                        .entry(name.to_ascii_lowercase())
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(parameter))
                        .is_none();
                    if duplicate {
                        result.incomplete_parameter_scopes.insert(owner);
                    }
                } else {
                    result.incomplete_parameter_scopes.insert(owner);
                }
            }
            if conditional
                && matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.parameter.declaration"
                        | "vba.omittable_parameter.declaration"
                        | "vba.variable.declaration"
                        | "vba.constant.declaration"
                )
                && let Some(branch) =
                    result.conditional_owner(fact.parent(), owner, cancellation)?
                && let Some(name) = name.as_deref().filter(|name| name.is_ascii())
            {
                // Same-branch uses may bind to this declaration even when the
                // compile flag is unknown; the enclosing owner keeps a barrier.
                result
                    .branch_bindings
                    .entry(branch)
                    .or_default()
                    .entry(name.to_ascii_lowercase())
                    .and_modify(|existing| *existing = None)
                    .or_insert(candidate);
            }
            if declaration.syntax_kind().as_str() == "vba.constant.declaration"
                && let Some(Binding::Value(symbol)) = symbol
            {
                result.constant_symbols.insert(symbol);
            }
            let enum_member = declaration.syntax_kind().as_str() == "vba.enum_constant.declaration";
            if enum_member {
                // Qualified lookup uses the enum's members, while simple names
                // participate in the enclosing module tier (MS-VBAL 5.6.10).
                result.insert(owner, name.as_deref(), symbol);
            }
            if owns_scope(declaration) || enum_member {
                let Some((parent, parent_conditional)) =
                    result.scope(result.fact(owner)?.parent(), cancellation)?
                else {
                    continue;
                };
                if parent_conditional {
                    symbol = None;
                    conditional = true;
                }
                // MS-VBAL 5.6.2.1 evaluates a bare enclosing Function/Get name
                // as its return value. Keep that declaration reference local;
                // falling through would invent a recursive accessor call.
                if matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.function.declaration" | "vba.get_property.declaration"
                ) {
                    // The procedure body owns its result slot even when an
                    // enclosing compile-time branch hides the outer binding.
                    let result_binding = match candidate {
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
            if declaration.syntax_kind().as_str() == "vba.event.declaration" {
                if let Some(name) = name.as_deref().filter(|name| name.is_ascii()) {
                    let symbol = if conditional {
                        None
                    } else {
                        symbols.get(&fact.local_id()).copied()
                    };
                    result
                        .events
                        .entry(owner)
                        .or_default()
                        .entry(name.to_ascii_lowercase())
                        .and_modify(|prior| *prior = None)
                        .or_insert(symbol);
                } else {
                    result.foreign_event_scopes.insert(owner);
                }
                continue;
            }
            if !conditional
                && matches!(
                    declaration.syntax_kind().as_str(),
                    "vba.sub_function.declaration"
                        | "vba.function.declaration"
                        | "vba.get_property.declaration"
                        | "vba.let_property.declaration"
                        | "vba.set_property.declaration"
                )
                && let Some(symbol) = symbol.and_then(|binding| match binding {
                    Binding::Procedure(symbol)
                    | Binding::Function { symbol, .. }
                    | Binding::Property(PropertyAccessors {
                        get: Some(symbol), ..
                    })
                    | Binding::Property(PropertyAccessors {
                        let_value: Some(symbol),
                        ..
                    })
                    | Binding::Property(PropertyAccessors {
                        set: Some(symbol), ..
                    }) => Some(symbol),
                    _ => None,
                })
            {
                result.callable_scopes.insert(symbol, declaration_scope);
            }
            if !conditional && let Some(name) = name.as_deref().filter(|name| name.is_ascii()) {
                result
                    .unconditional_names
                    .entry(owner)
                    .or_default()
                    .insert(name.to_ascii_lowercase());
            }
            if enum_member || declaration.syntax_kind().as_str() == "vba.enum.declaration" {
                enum_module_bindings.push((declaration.span().start_byte(), owner, name, symbol));
            } else {
                result.insert(owner, name.as_deref(), symbol);
            }
        }
        // Enum type/member precedence is defined by source order, not capture
        // order. Select earlier candidates before duplicate names become barriers.
        enum_module_bindings.sort_by_key(|entry| entry.0);
        for (_, owner, name, symbol) in enum_module_bindings {
            cancellation.check()?;
            result.insert(owner, name.as_deref(), symbol);
        }
        result.collect_declared_types(facts, source, symbols, cancellation)?;
        for fact in facts {
            cancellation.check()?;
            let mut visibility = match fact.syntax_kind().as_str() {
                "vba.public.expression" => EntityVisibility::Public,
                "vba.private.expression" => EntityVisibility::Private,
                "vba.project_restricted.expression" => EntityVisibility::Restricted,
                "vba.visibility_unknown.expression" => EntityVisibility::Unknown,
                _ => continue,
            };
            if let Some(declaration) = result.declaration(fact.parent(), cancellation)?
                && declaration.span().start_byte() == fact.span().start_byte()
                && declaration.span().end_byte() >= fact.span().end_byte()
            {
                if visibility == EntityVisibility::Restricted
                    && (result.class_module.is_none()
                        || !matches!(
                            declaration.syntax_kind().as_str(),
                            "vba.sub_function.declaration"
                                | "vba.function.declaration"
                                | "vba.get_property.declaration"
                                | "vba.let_property.declaration"
                                | "vba.set_property.declaration"
                        ))
                {
                    visibility = EntityVisibility::Unknown;
                }
                result
                    .visibilities
                    .entry(declaration.local_id())
                    .and_modify(|prior| *prior = EntityVisibility::Unknown)
                    .or_insert(visibility);
            }
        }
        Ok(result)
    }

    pub(super) fn visibilities(&self) -> impl Iterator<Item = (u64, EntityVisibility)> + '_ {
        self.visibilities
            .iter()
            .map(|(&owner, &visibility)| (owner, visibility))
    }

    fn collect_declared_types(
        &mut self,
        facts: &[SyntaxFact],
        source: &[u8],
        symbols: &HashMap<u64, SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut declarations = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "vba.identifier.definition" {
                continue;
            }
            let Some(declaration) = self.declaration(fact.parent(), cancellation)? else {
                continue;
            };
            let Some((owner, conditional)) = self.scope(fact.parent(), cancellation)? else {
                continue;
            };
            let symbol = symbols
                .get(&fact.local_id())
                .copied()
                .filter(|_| !conditional);
            let name = self.source_name(fact, source)?;
            declarations.insert(declaration.local_id(), symbol.filter(|_| name.is_some()));
            match declaration.syntax_kind().as_str() {
                "vba.struct.declaration" | "vba.enum.declaration" => {
                    let Some((module, outer_conditional)) =
                        self.scope(self.fact(owner)?.parent(), cancellation)?
                    else {
                        continue;
                    };
                    if self.fact(module)?.syntax_kind().as_str() != "vba.file.scope" {
                        continue;
                    }
                    let Some(name) = name else {
                        self.foreign_type_scopes.insert(module);
                        continue;
                    };
                    let ty = symbol
                        .filter(|_| !outer_conditional)
                        .map(|symbol| DeclaredType {
                            symbol,
                            field_scope: (declaration.syntax_kind().as_str()
                                == "vba.struct.declaration")
                                .then_some(owner),
                        });
                    self.types
                        .entry(module)
                        .or_default()
                        .entry(name)
                        .and_modify(|value| *value = None)
                        .or_insert(ty);
                }
                "vba.field.declaration" => {
                    let Some(name) = name else {
                        self.foreign_field_scopes.insert(owner);
                        continue;
                    };
                    self.fields
                        .entry(owner)
                        .or_default()
                        .entry(name)
                        .and_modify(|value| *value = None)
                        .or_insert(symbol);
                }
                _ => {}
            }
        }
        for fact in facts {
            cancellation.check()?;
            if !matches!(
                fact.syntax_kind().as_str(),
                "vba.direct_type_identifier.reference" | "vba.type_identifier.reference"
            ) {
                continue;
            }
            let Some(declaration) = self.declaration(fact.parent(), cancellation)? else {
                continue;
            };
            let declared_value_type = fact.syntax_kind().as_str()
                == "vba.direct_type_identifier.reference"
                || (fact.syntax_kind().as_str() == "vba.type_identifier.reference"
                    && matches!(
                        declaration.syntax_kind().as_str(),
                        "vba.function.declaration" | "vba.get_property.declaration"
                    ));
            if !declared_value_type {
                continue;
            }
            let Some(&Some(symbol)) = declarations.get(&declaration.local_id()) else {
                continue;
            };
            let Some(name) = self.source_name(fact, source)? else {
                continue;
            };
            if let Some(DeclaredType {
                field_scope: Some(scope),
                ..
            }) = self.declared_type(fact, &name, cancellation)?
            {
                self.value_types.insert(symbol, scope);
            }
        }
        Ok(())
    }

    fn source_name(
        &self,
        fact: &SyntaxFact,
        source: &[u8],
    ) -> Result<Option<String>, AdapterError> {
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        let text = std::str::from_utf8(source.get(start..end).ok_or_else(invalid_capture)?)
            .map_err(|_| invalid_capture())?;
        if text.ends_with(['$', '%', '&', '#', '@', '^', '!']) {
            return Ok(None);
        }
        Ok(
            structural_captured_name_for_fact("vba", fact, text, self.maximum_name_bytes)
                .filter(|name| name.is_ascii())
                .map(|name| name.to_ascii_lowercase()),
        )
    }

    fn declared_type(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<DeclaredType>, AdapterError> {
        // MS-VBAL type binding searches module-level UDTs and enums before
        // project types; procedure-local values do not shadow this namespace.
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            let scope = self.fact(owner)?;
            if scope.syntax_kind().as_str() == "vba.file.scope" {
                if self.foreign_type_scopes.contains(&owner) {
                    return Ok(None);
                }
                return Ok(self
                    .types
                    .get(&owner)
                    .and_then(|names| names.get(name))
                    .copied()
                    .flatten());
            }
            parent = scope.parent();
        }
        Err(invalid_capture())
    }

    fn field_reference(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        if text.contains('(') {
            return self.returned_udt_field_reference(fact, text, cancellation);
        }
        // A written dot-chain can select enum constants or declared UDT fields.
        // Indexing and default-member dispatch need separate application evidence.
        let mut components = text.split('.');
        let Some(receiver_text) = components.next() else {
            return Ok(None);
        };
        // Me bypasses procedure locals, while escaped [Me] is an ordinary
        // identifier. Only the source class's own variable can start this chain.
        let instance = receiver_text.trim().eq_ignore_ascii_case("Me");
        let receiver_text = if instance {
            let Some(component) = components.next() else {
                return Ok(None);
            };
            component
        } else {
            receiver_text
        };
        let Some(receiver) = member_key(receiver_text) else {
            return Ok(None);
        };
        let mut parent = fact.parent();
        let mut receiver_binding = None;
        let mut instance_context = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                break;
            };
            let scope = self.fact(owner)?;
            if scope.syntax_kind().as_str() == "vba.enum.scope" {
                parent = scope.parent();
                continue;
            }
            if instance {
                if scope.syntax_kind().as_str() != "vba.file.scope" {
                    instance_context |= is_instance_procedure_scope(scope);
                    parent = scope.parent();
                    continue;
                }
                if !instance_context
                    || self.class_symbol.is_none()
                    || !self
                        .class_module
                        .is_some_and(|class| class.span() == scope.span())
                {
                    return Ok(None);
                }
            }
            if self.foreign_scopes.contains(&owner) {
                return Ok(None);
            }
            if let Some(binding) = self
                .bindings
                .get(&owner)
                .and_then(|names| names.get(&receiver))
            {
                receiver_binding = *binding;
                break;
            }
            if scope.syntax_kind().as_str() == "vba.file.scope" {
                break;
            }
            parent = scope.parent();
        }
        if let Some(Binding::EnumType { scope, .. }) = receiver_binding {
            if instance || self.foreign_scopes.contains(&scope) {
                return Ok(None);
            }
            let Some(name) = components.next().and_then(member_key) else {
                return Ok(None);
            };
            if components.next().is_some() {
                return Ok(None);
            }
            let Some(Some(Binding::EnumMember { symbol, defined_at })) = self
                .bindings
                .get(&scope)
                .and_then(|members| members.get(&name))
            else {
                return Ok(None);
            };
            return Ok(self
                .enum_member_precedes_initializer(fact, *defined_at, cancellation)?
                .then_some(ResolvedBinding {
                    symbol: *symbol,
                    is_call: false,
                }));
        }
        if receiver_binding.is_none()
            && !instance
            && let Some((_, scope)) = self.local_procedural_module(fact, &receiver, cancellation)?
        {
            let Some(name) = components.next().and_then(member_key) else {
                return Ok(None);
            };
            if components.next().is_some() {
                return Ok(None);
            }
            let invoked = fact.syntax_kind().as_str() == "vba.invoked_member.reference";
            return Ok(
                match self.bindings.get(&scope).and_then(|names| names.get(&name)) {
                    Some(Some(Binding::Procedure(symbol) | Binding::ExternalProcedure(symbol)))
                        if invoked =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: true,
                        })
                    }
                    Some(Some(Binding::Function { symbol, .. })) if invoked => {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: true,
                        })
                    }
                    Some(Some(Binding::Value(symbol))) if !invoked => Some(ResolvedBinding {
                        symbol: *symbol,
                        is_call: false,
                    }),
                    _ => None,
                },
            );
        }
        let Some(Binding::Value(mut symbol)) = receiver_binding else {
            return Ok(None);
        };
        let mut member_count = 0_usize;
        for component in components {
            cancellation.check()?;
            let Some(name) = member_key(component) else {
                return Ok(None);
            };
            let Some(scope) = self.value_types.get(&symbol) else {
                return Ok(None);
            };
            if self.foreign_field_scopes.contains(scope) {
                return Ok(None);
            }
            let Some(Some(field)) = self.fields.get(scope).and_then(|fields| fields.get(&name))
            else {
                return Ok(None);
            };
            symbol = *field;
            member_count = member_count.checked_add(1).ok_or_else(invalid_capture)?;
        }
        Ok((member_count > 0).then_some(ResolvedBinding {
            symbol,
            is_call: false,
        }))
    }

    fn returned_udt_field_reference(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        let span = fact.span();
        let start = span.start_byte();
        let mut applications = self
            .applications
            .get(&start)
            .into_iter()
            .flatten()
            .filter(|candidate| candidate.span().end_byte() < span.end_byte());
        let Some(application) = applications.next() else {
            return Ok(None);
        };
        if applications.next().is_some() {
            return Ok(None);
        }
        let application_bytes = usize::try_from(application.span().end_byte() - start)
            .map_err(|_| invalid_capture())?;
        let Some((application_text, suffix)) = text.split_at_checked(application_bytes) else {
            return Ok(None);
        };
        let Some(members) = suffix.strip_prefix('.') else {
            return Ok(None);
        };
        if !application_text.ends_with(')') {
            return Ok(None);
        }
        // An application may also index an array. Its callee must independently
        // resolve as a call before its declared UDT return type can own fields.
        let mut invocations = self
            .invocations
            .get(&start)
            .into_iter()
            .flatten()
            .filter(|candidate| candidate.span().end_byte() < application.span().end_byte());
        let Some(invocation) = invocations.next() else {
            return Ok(None);
        };
        if invocations.next().is_some() {
            return Ok(None);
        }
        let invocation_bytes =
            usize::try_from(invocation.span().end_byte() - start).map_err(|_| invalid_capture())?;
        let Some(invocation_text) = application_text.get(..invocation_bytes) else {
            return Ok(None);
        };
        let Some(ResolvedBinding {
            mut symbol,
            is_call: true,
        }) = self.resolve(invocation, invocation_text, cancellation)?
        else {
            return Ok(None);
        };
        for component in members.split('.') {
            cancellation.check()?;
            let Some(name) = member_key(component) else {
                return Ok(None);
            };
            let Some(scope) = self.value_types.get(&symbol) else {
                return Ok(None);
            };
            if self.foreign_field_scopes.contains(scope) {
                return Ok(None);
            }
            let Some(Some(field)) = self.fields.get(scope).and_then(|fields| fields.get(&name))
            else {
                return Ok(None);
            };
            symbol = *field;
        }
        Ok(Some(ResolvedBinding {
            symbol,
            is_call: false,
        }))
    }

    /// Checks whether a complete array application proves this exact callee target.
    ///
    /// # Errors
    ///
    /// Returns adapter errors for inconsistent captured evidence or cancellation.
    pub(super) fn array_invocation_target_is_proven(
        &self,
        fact: &SyntaxFact,
        target: SymbolId,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(start) = self.array_invocation_start(fact)? else {
            return Ok(false);
        };
        if !self.array_symbols.contains(&target)
            || !self
                .array_invocations_at(start)
                .any(|candidate| candidate.local_id() == fact.local_id())
        {
            return Ok(false);
        }
        for application in self.applications.get(&start).into_iter().flatten() {
            cancellation.check()?;
            let length = usize::try_from(application.span().end_byte() - start)
                .map_err(|_| invalid_capture())?;
            // Use the same admission as resolve, so an oversized or incomplete
            // application cannot retire a gap on its shorter callee capture.
            if length <= self.maximum_name_bytes
                && self
                    .array_application(application, cancellation)?
                    .is_some_and(|binding| !binding.is_call && binding.symbol == target)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn array_invocation_start(&self, fact: &SyntaxFact) -> Result<Option<u64>, AdapterError> {
        if !is_self_array_invocation(fact) {
            return Ok(Some(fact.span().start_byte()));
        }
        let Some((&start, _)) = self
            .applications
            .range(..fact.span().start_byte())
            .next_back()
        else {
            return Ok(None);
        };
        let prefix_length =
            usize::try_from(fact.span().start_byte() - start).map_err(|_| invalid_capture())?;
        if prefix_length > self.maximum_name_bytes {
            return Ok(None);
        }
        let begin = usize::try_from(start).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let Some(prefix) = self.source.get(begin..end) else {
            return Ok(None);
        };
        // The native self-member capture proves Me's syntax. Match its receiver
        // to this application, ignoring only whitespace and line continuation.
        Ok(prefix
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'_')
            .map(|byte| byte.to_ascii_lowercase())
            .eq(b"me.".iter().copied())
            .then_some(start))
    }

    fn array_invocations_at(&self, start: u64) -> impl Iterator<Item = &&'a SyntaxFact> {
        self.invocations.get(&start).into_iter().flatten().chain(
            self.array_only_invocations
                .get(&start)
                .into_iter()
                .flatten(),
        )
    }

    fn array_application(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        let start = fact.span().start_byte();
        let end = fact.span().end_byte();
        if self.applications.get(&start).is_some_and(|applications| {
            applications
                .iter()
                .any(|application| application.span().end_byte() < end)
        }) {
            return Ok(None);
        }
        let mut invocations = self
            .array_invocations_at(start)
            .filter(|invocation| invocation.span().end_byte() < end);
        let Some(invocation) = invocations.next() else {
            return Ok(None);
        };
        if invocations.next().is_some() {
            return Ok(None);
        }
        let (Ok(callee_start), Ok(callee_end), Ok(application_end)) = (
            usize::try_from(invocation.span().start_byte()),
            usize::try_from(invocation.span().end_byte()),
            usize::try_from(end),
        ) else {
            return Ok(None);
        };
        let statement_assignment = matches!(
            invocation.syntax_kind().as_str(),
            "vba.invoked_identifier.reference" | "vba.self_invoked_member.reference"
        );
        if !self
            .source
            .get(callee_end..application_end)
            .is_some_and(|suffix| complete_array_application_suffix(suffix, statement_assignment))
        {
            return Ok(None);
        }
        let Some(callee) = self
            .source
            .get(callee_start..callee_end)
            .and_then(|source| std::str::from_utf8(source).ok())
        else {
            return Ok(None);
        };
        let Some(binding) = self.resolve(invocation, callee, cancellation)? else {
            return Ok(None);
        };
        Ok((!binding.is_call && self.array_symbols.contains(&binding.symbol)).then_some(binding))
    }

    fn enum_member_precedes_initializer(
        &self,
        fact: &SyntaxFact,
        defined_at: u64,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        // Enum initializers may reference earlier members, including those in
        // earlier enums, but never themselves or later members (MS-VBAL 5.2.3.4).
        Ok(self
            .declaration(fact.parent(), cancellation)?
            .is_none_or(|declaration| {
                declaration.syntax_kind().as_str() != "vba.enum_constant.declaration"
                    || defined_at < declaration.span().start_byte()
            }))
    }

    fn containing_application(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        for (_, candidates) in self.applications.range(..=fact.span().start_byte()).rev() {
            cancellation.check()?;
            let mut containing = candidates
                .iter()
                .copied()
                .filter(|candidate| candidate.span().end_byte() >= fact.span().end_byte());
            if let Some(application) = containing.next() {
                return Ok((containing.next().is_none()).then_some(application));
            }
        }
        Ok(None)
    }

    fn argument_open(
        &self,
        application: &SyntaxFact,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        let start =
            usize::try_from(application.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let Some(source) = self.source.get(start..end) else {
            return Ok(None);
        };
        let mut closed = 0_usize;
        let mut quoted = false;
        let mut index = source.len();
        while index > 0 {
            index -= 1;
            if index % 4096 == 0 {
                cancellation.check()?;
            }
            match source[index] {
                b'"' if quoted && index > 0 && source[index - 1] == b'"' => index -= 1,
                b'"' => quoted = !quoted,
                b')' if !quoted => closed = closed.checked_add(1).ok_or_else(invalid_capture)?,
                b'(' if !quoted && closed == 0 => return Ok(Some(start + index)),
                b'(' if !quoted => closed -= 1,
                b'\'' if !quoted => return Ok(None),
                _ => {}
            }
        }
        Ok(None)
    }

    fn named_argument(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        let Some(tail) = self.source.get(end..) else {
            return Ok(None);
        };
        let separator = tail
            .iter()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        if !tail
            .get(separator..)
            .is_some_and(|tail| tail.starts_with(b":="))
        {
            return Ok(None);
        }
        let Some(name) =
            structural_captured_name_for_fact("vba", fact, text, self.maximum_name_bytes)
                .filter(|name| name.is_ascii())
        else {
            return Ok(None);
        };
        let Some(application) = self.containing_application(fact, cancellation)? else {
            return Ok(None);
        };
        let Some(open) = self.argument_open(application, fact, cancellation)? else {
            return Ok(None);
        };
        let mut callee = None;
        for invocations in self
            .invocations
            .range(
                application.span().start_byte()
                    ..u64::try_from(open).map_err(|_| invalid_capture())?,
            )
            .map(|(_, invocations)| invocations)
        {
            for invocation in invocations {
                cancellation.check()?;
                let invocation_end =
                    usize::try_from(invocation.span().end_byte()).map_err(|_| invalid_capture())?;
                // An identifier preceding a receiver application is not the
                // callee of this argument list, even if both spans overlap.
                if !self
                    .source
                    .get(invocation_end..open)
                    .is_some_and(|separator| separator.iter().all(u8::is_ascii_whitespace))
                {
                    continue;
                }
                if callee.replace(*invocation).is_some() {
                    return Ok(None);
                }
            }
        }
        let Some(callee) = callee else {
            return Ok(None);
        };
        let start = usize::try_from(callee.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(callee.span().end_byte()).map_err(|_| invalid_capture())?;
        let Some(callee_text) = self
            .source
            .get(start..end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            return Ok(None);
        };
        let Some(ResolvedBinding {
            symbol: procedure,
            is_call: true,
        }) = self.resolve(callee, callee_text, cancellation)?
        else {
            return Ok(None);
        };
        let Some(scope) = self.callable_scopes.get(&procedure).copied() else {
            return Ok(None);
        };
        if self.incomplete_parameter_scopes.contains(&scope) {
            return Ok(None);
        }
        Ok(self
            .named_parameters
            .get(&scope)
            .and_then(|parameters| parameters.get(&name.to_ascii_lowercase()))
            .copied()
            .flatten()
            .filter(|symbol| !self.property_value_symbols.contains(symbol))
            .map(|symbol| ResolvedBinding {
                symbol,
                is_call: false,
            }))
    }

    fn local_procedural_module(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<(SymbolId, u64)>, AdapterError> {
        let Some((module_name, symbol)) = &self.procedural_module else {
            return Ok(None);
        };
        if module_name != name {
            return Ok(None);
        }
        // MS-VBAL 5.6.12 admits procedural-module member access, but a lexical
        // declaration (including a conditional barrier) may own the receiver.
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            if self.foreign_scopes.contains(&owner)
                || self
                    .bindings
                    .get(&owner)
                    .is_some_and(|names| names.contains_key(name))
                || !matches!(
                    self.conditional_binding(parent, owner, name, cancellation)?,
                    ConditionalBinding::Absent
                )
            {
                return Ok(None);
            }
            let scope = self.fact(owner)?;
            if scope.syntax_kind().as_str() == "vba.file.scope" {
                return Ok(Some((*symbol, owner)));
            }
            parent = scope.parent();
        }
        Err(invalid_capture())
    }

    fn is_member_receiver(&self, fact: &SyntaxFact) -> bool {
        self.source
            .get(usize::try_from(fact.span().end_byte()).unwrap_or(usize::MAX))
            == Some(&b'.')
            && self
                .member_references
                .get(&fact.span().start_byte())
                .is_some_and(|end| *end > fact.span().end_byte())
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        cancellation.check()?;
        if text.len() > self.maximum_name_bytes {
            return Ok(None);
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.procedure_pointer.reference" | "vba.qualified_procedure_pointer.reference"
        ) {
            return self.procedure_pointer(fact, text, cancellation);
        }
        if fact.syntax_kind().as_str() == "vba.application.reference" {
            return self.array_application(fact, cancellation);
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.member.reference" | "vba.invoked_member.reference"
        ) {
            return self.field_reference(fact, text, cancellation);
        }
        if fact.syntax_kind().as_str() == "vba.nonvalue_identifier.reference" {
            return self.named_argument(fact, text, cancellation);
        }
        if fact.syntax_kind().as_str() == "vba.event_identifier.reference" {
            return self.event_reference(fact, text, cancellation);
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.statement_label.reference"
                | "vba.error_label.reference"
                | "vba.resume_label.reference"
        ) {
            let Some(key) = statement_label_key(text) else {
                return Ok(None);
            };
            // Zero changes the error/resume policy; it does not name label 0.
            if key == "0" && fact.syntax_kind().as_str() != "vba.statement_label.reference" {
                return Ok(None);
            }
            let Some((owner, _)) = self.scope(fact.parent(), cancellation)? else {
                return Ok(None);
            };
            if !is_instance_procedure_scope(self.fact(owner)?)
                || self.foreign_label_scopes.contains(&owner)
            {
                return Ok(None);
            }
            return Ok(self
                .statement_label_reference(owner, fact.parent(), &key, cancellation)?
                .map(|symbol| ResolvedBinding {
                    symbol,
                    is_call: false,
                }));
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.type_identifier.reference"
                | "vba.direct_type_identifier.reference"
                | "vba.tested_type_identifier.reference"
        ) {
            if text.ends_with(['$', '%', '&', '#', '@', '^', '!']) {
                return Ok(None);
            }
            let Some(name) =
                structural_captured_name_for_fact("vba", fact, text, self.maximum_name_bytes)
                    .filter(|name| name.is_ascii())
            else {
                return Ok(None);
            };
            return Ok(self
                .declared_type(fact, &name.to_ascii_lowercase(), cancellation)?
                .map(|ty| ResolvedBinding {
                    symbol: ty.symbol,
                    is_call: false,
                }));
        }
        let instance = matches!(
            fact.syntax_kind().as_str(),
            "vba.instance.reference" | "vba.invoked_instance.reference"
        );
        if instance && !text.eq_ignore_ascii_case("Me") {
            return Ok(None);
        }
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
            && !instance
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
            if scope.syntax_kind().as_str() == "vba.enum.scope" {
                parent = scope.parent();
                continue;
            }
            if self_member || instance {
                if scope.syntax_kind().as_str() != "vba.file.scope" {
                    instance_context |= is_instance_procedure_scope(scope);
                    // The instance keyword and explicit Me member lookup
                    // bypass ordinary procedure-local name lookup.
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
                if instance {
                    // The source class owns Me's declared type (MS-VBAL
                    // 5.6.11); this is not a runtime object or call identity.
                    return Ok(self.class_symbol.map(|symbol| ResolvedBinding {
                        symbol,
                        is_call: false,
                    }));
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
            let conditional =
                if self_member || instance || default_constant || self.branch_bindings.is_empty() {
                    ConditionalBinding::Absent
                } else {
                    self.conditional_binding(parent, owner, &key, cancellation)?
                };
            let binding = match conditional {
                ConditionalBinding::Absent => {
                    self.bindings.get(&owner).and_then(|names| names.get(&key))
                }
                ConditionalBinding::Value(binding) => Some(binding),
                ConditionalBinding::Ambiguous => return Ok(None),
            };
            if let Some(binding) = binding {
                return Ok(match binding {
                    Some(Binding::EnumMember { symbol, defined_at })
                        if !self_member
                            && !text.ends_with(['$', '%', '&', '#', '@', '^', '!'])
                            && self.enum_member_precedes_initializer(
                                fact,
                                *defined_at,
                                cancellation,
                            )? =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: false,
                        })
                    }
                    Some(Binding::EnumType { symbol, .. })
                        if !self_member
                            && !default_constant
                            && !text.ends_with(['$', '%', '&', '#', '@', '^', '!']) =>
                    {
                        Some(ResolvedBinding {
                            symbol: *symbol,
                            is_call: false,
                        })
                    }
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
                if fact.syntax_kind().as_str() == "vba.value_identifier.reference"
                    && self.is_member_receiver(fact)
                    && let Some((symbol, _)) =
                        self.local_procedural_module(fact, &key, cancellation)?
                {
                    return Ok(Some(ResolvedBinding {
                        symbol,
                        is_call: false,
                    }));
                }
                return Ok(None);
            }
            parent = scope.parent();
        }
        Err(invalid_capture())
    }

    fn procedure_pointer(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        let Some(path) = crate::vba_pointer::parse(text.as_bytes(), cancellation)? else {
            return Ok(None);
        };
        if path.components > 2 {
            return Ok(None);
        }
        let key = std::str::from_utf8(path.last).ok().and_then(member_key);
        let Some(key) = key else {
            return Ok(None);
        };
        // MS-VBAL 5.6.10 gives procedure pointers a separate namespace tier:
        // locals (including a function's result slot) cannot shadow callbacks.
        // Class instance callbacks have implementation-defined behavior; retain
        // their gap until the host supplies a supported calling-context proof.
        if self.class_module.is_some() || text.ends_with(['$', '%', '&', '#', '@', '^', '!']) {
            return Ok(None);
        }
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            let scope = self.fact(owner)?;
            if scope.syntax_kind().as_str() != "vba.file.scope" {
                parent = scope.parent();
                continue;
            }
            if self.foreign_scopes.contains(&owner) {
                return Ok(None);
            }
            if path.components == 2 {
                let Some((module, _)) = &self.procedural_module else {
                    return Ok(None);
                };
                if !path.first.eq_ignore_ascii_case(module.as_bytes()) {
                    return Ok(None);
                }
                // The pointer namespace ignores value declarations, but a
                // procedure/getter in the module tier outranks a module name.
                if self
                    .bindings
                    .get(&owner)
                    .and_then(|names| names.get(module))
                    .is_some_and(|binding| match binding {
                        None
                        | Some(
                            Binding::Procedure(_)
                            | Binding::Function { .. }
                            | Binding::ExternalProcedure(_),
                        ) => true,
                        Some(Binding::Property(accessors)) => accessors.get.is_some(),
                        _ => false,
                    })
                {
                    return Ok(None);
                }
            }
            // MS-VBAL 5.6.12 admits only Sub/Function through qualification;
            // Property Get is eligible only in the simple-name pointer context.
            let symbol = match self.bindings.get(&owner).and_then(|names| names.get(&key)) {
                Some(Some(Binding::Procedure(symbol) | Binding::Function { symbol, .. })) => {
                    Some(*symbol)
                }
                Some(Some(Binding::Property(accessors))) if path.components == 1 => accessors.get,
                _ => None,
            };
            return Ok(symbol.map(|symbol| ResolvedBinding {
                symbol,
                is_call: false,
            }));
        }
        Err(invalid_capture())
    }

    fn event_reference(
        &self,
        fact: &SyntaxFact,
        text: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<ResolvedBinding>, AdapterError> {
        let Some(name) = member_key(text) else {
            return Ok(None);
        };
        let mut parent = fact.parent();
        let mut instance_context = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some((owner, _)) = self.scope(parent, cancellation)? else {
                return Ok(None);
            };
            let scope = self.fact(owner)?;
            if scope.syntax_kind().as_str() != "vba.file.scope" {
                instance_context |= is_instance_procedure_scope(scope);
                parent = scope.parent();
                continue;
            }
            // RaiseEvent selects an explicitly declared event of this object
            // module, not a procedure-local value or a host-provided event.
            if !instance_context
                || !self
                    .class_module
                    .is_some_and(|class| class.span() == scope.span())
                || self.foreign_event_scopes.contains(&owner)
            {
                return Ok(None);
            }
            return Ok(
                match self.events.get(&owner).and_then(|names| names.get(&name)) {
                    Some(Some(symbol)) => Some(ResolvedBinding {
                        symbol: *symbol,
                        is_call: false,
                    }),
                    _ => None,
                },
            );
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
                    (
                        Some(left @ Binding::EnumType { defined_at: a, .. }),
                        Some(right @ Binding::EnumMember { defined_at: b, .. }),
                    )
                    | (
                        Some(left @ Binding::EnumMember { defined_at: a, .. }),
                        Some(right @ Binding::EnumType { defined_at: b, .. }),
                    ) => {
                        // The earlier declaration wins only this specific
                        // type/member collision; all other duplicates stay ambiguous.
                        Some(if a < b { left } else { right })
                    }
                    (Some(Binding::Property(left)), Some(Binding::Property(right))) => {
                        left.merge(right).map(Binding::Property)
                    }
                    _ => None,
                };
            })
            .or_insert(symbol);
    }

    fn statement_label_reference(
        &self,
        owner: u64,
        parent: Option<u64>,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        let Some(labels) = self
            .statement_labels
            .get(&owner)
            .and_then(|names| names.get(name))
        else {
            return Ok(None);
        };
        let reference_conditions = self.statement_label_conditions(parent, owner, cancellation)?;
        let mut matched = None;
        for label in labels {
            cancellation.check()?;
            let conditions = self.statement_label_conditions(
                self.fact(label.definition)?.parent(),
                owner,
                cancellation,
            )?;
            if conditions.iter().any(|(group, branch)| {
                reference_conditions
                    .get(group)
                    .is_some_and(|other| other != branch)
            }) {
                continue;
            }
            // MS-VBAL 3.4 removes excluded branches before name resolution.
            // Bind only a unique label present whenever this use is present;
            // a possible coexisting duplicate is ambiguous, not a shadowing label.
            if matched.is_some()
                || label.symbol.is_none()
                || conditions
                    .iter()
                    .any(|(group, branch)| reference_conditions.get(group) != Some(branch))
            {
                return Ok(None);
            }
            matched = label.symbol;
        }
        Ok(matched)
    }

    fn statement_label_conditions(
        &self,
        mut parent: Option<u64>,
        owner: u64,
        cancellation: &Cancellation,
    ) -> Result<BTreeMap<u64, u64>, AdapterError> {
        let mut conditions = BTreeMap::new();
        let mut branch = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let id = parent.ok_or_else(invalid_capture)?;
            if id == owner {
                return if branch.is_none() {
                    Ok(conditions)
                } else {
                    Err(invalid_capture())
                };
            }
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Scope {
                match fact.syntax_kind().as_str() {
                    "vba.preprocessor_branch.scope" => {
                        if branch.replace(id).is_some() {
                            return Err(invalid_capture());
                        }
                    }
                    "vba.preprocessor.scope" => {
                        let selected = branch.take().ok_or_else(invalid_capture)?;
                        if conditions.insert(id, selected).is_some() {
                            return Err(invalid_capture());
                        }
                    }
                    _ => {}
                }
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn conditional_owner(
        &self,
        mut parent: Option<u64>,
        owner: u64,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let id = parent.ok_or_else(invalid_capture)?;
            let fact = self.fact(id)?;
            if id == owner {
                return Ok(fact
                    .syntax_kind()
                    .as_str()
                    .starts_with("vba.conditional_")
                    .then_some(id));
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "vba.preprocessor_branch.scope"
            {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn conditional_binding<'b>(
        &'b self,
        mut parent: Option<u64>,
        owner: u64,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<ConditionalBinding<'b>, AdapterError> {
        let mut matched = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let id = parent.ok_or_else(invalid_capture)?;
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Scope
                && (fact.syntax_kind().as_str() == "vba.preprocessor_branch.scope"
                    || (id == owner && fact.syntax_kind().as_str().starts_with("vba.conditional_")))
                && let Some(binding) = self
                    .branch_bindings
                    .get(&id)
                    .and_then(|names| names.get(name))
            {
                if matched.is_some()
                    || self
                        .unconditional_names
                        .get(&owner)
                        .is_some_and(|names| names.contains(name))
                {
                    return Ok(ConditionalBinding::Ambiguous);
                }
                matched = Some(binding);
            }
            if id == owner {
                return Ok(matched.map_or(ConditionalBinding::Absent, ConditionalBinding::Value));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
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
                if matches!(
                    kind,
                    "vba.preprocessor.scope" | "vba.preprocessor_branch.scope"
                ) {
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

fn is_instance_procedure_scope(scope: &SyntaxFact) -> bool {
    matches!(
        scope.syntax_kind().as_str(),
        "vba.sub_function.scope"
            | "vba.function.scope"
            | "vba.get_property.scope"
            | "vba.let_property.scope"
            | "vba.set_property.scope"
            | "vba.conditional_sub_function.scope"
            | "vba.conditional_function.scope"
            | "vba.conditional_property.scope"
    )
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

/// Compares ASCII labels case-insensitively and integer labels by nonnegative
/// VBA data value; unsupported spellings remain unbound instead of being guessed.
pub(crate) fn statement_label_key(text: &str) -> Option<String> {
    let (literal, suffix) = match text.as_bytes().last() {
        Some(b'%' | b'&' | b'^') => (
            text.get(..text.len().checked_sub(1)?)?,
            text.as_bytes().last().copied(),
        ),
        _ => (text, None),
    };
    if text
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'&')
    {
        let (digits, radix) = if let Some(digits) = literal
            .strip_prefix("&H")
            .or_else(|| literal.strip_prefix("&h"))
        {
            (digits, 16)
        } else if let Some(digits) = literal
            .strip_prefix("&O")
            .or_else(|| literal.strip_prefix("&o"))
        {
            (digits, 8)
        } else if let Some(digits) = literal.strip_prefix('&') {
            (digits, 8)
        } else {
            (literal, 10)
        };
        if digits.is_empty() || !digits.bytes().all(|byte| char::from(byte).is_digit(radix)) {
            return None;
        }
        let value = u32::from_str_radix(digits, radix).ok()?;
        // MS-VBAL integer tokens can encode signed 16-bit hex/octal values;
        // labels require a nonnegative data value bounded by Long's maximum.
        if value > i32::MAX.unsigned_abs()
            || (suffix == Some(b'%') && value > 32_767)
            || (radix != 10 && suffix.is_none() && (32_768..=65_535).contains(&value))
        {
            return None;
        }
        return Some(value.to_string());
    }
    if text.eq_ignore_ascii_case("Randomize")
        || !text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(text.to_ascii_lowercase())
}

fn member_key(text: &str) -> Option<String> {
    let text = text.trim();
    let name = text
        .strip_prefix('[')
        .and_then(|name| name.strip_suffix(']'))
        .unwrap_or(text);
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

fn declaration_has_array_designator(
    declaration: &SyntaxFact,
    name: &SyntaxFact,
    source: &[u8],
) -> Result<bool, AdapterError> {
    let parameter = matches!(
        declaration.syntax_kind().as_str(),
        "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
    );
    if !parameter && declaration.syntax_kind().as_str() != "vba.variable.declaration" {
        return Ok(false);
    }
    let name_end = usize::try_from(name.span().end_byte()).map_err(|_| invalid_capture())?;
    let declaration_end =
        usize::try_from(declaration.span().end_byte()).map_err(|_| invalid_capture())?;
    let suffix = source
        .get(name_end..declaration_end)
        .ok_or_else(invalid_capture)?
        .trim_ascii_start();
    let Some(bounds) = suffix.strip_prefix(b"(") else {
        return Ok(false);
    };
    if !parameter {
        return Ok(true);
    }
    // Parameters require an empty designator and cannot pass arrays ByVal
    // (MS-VBAL 5.3.1.5), even when the native parser accepts that syntax.
    if !bounds.trim_ascii_start().starts_with(b")") {
        return Ok(false);
    }
    let declaration_start =
        usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_start = usize::try_from(name.span().start_byte()).map_err(|_| invalid_capture())?;
    let prefix = source
        .get(declaration_start..name_start)
        .ok_or_else(invalid_capture)?;
    Ok(prefix
        .split(u8::is_ascii_whitespace)
        .filter(|word| !word.is_empty() && *word != b"_")
        .all(|word| {
            word.eq_ignore_ascii_case(b"ByRef")
                || word.eq_ignore_ascii_case(b"Optional")
                || word.eq_ignore_ascii_case(b"ParamArray")
        }))
}

fn is_self_array_invocation(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "vba.self_invoked_member.reference"
            | "vba.self_invoked_get_member.reference"
            | "vba.self_invoked_let_member.reference"
            | "vba.self_invoked_set_member.reference"
    )
}

fn complete_array_application_suffix(suffix: &[u8], statement_assignment: bool) -> bool {
    let suffix = suffix.trim_ascii_start();
    if suffix.first() != Some(&b'(') {
        return false;
    }
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut index = 0_usize;
    while let Some(&byte) = suffix.get(index) {
        if quoted {
            if byte == b'"' {
                if suffix.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'(' => depth += 1,
                b')' => {
                    let Some(outer_depth) = depth.checked_sub(1) else {
                        return false;
                    };
                    depth = outer_depth;
                    if depth == 0 {
                        let Some(tail) = suffix.get(index + 1..) else {
                            return false;
                        };
                        let tail = tail.trim_ascii();
                        // Native call statements can represent an indexed assignment
                        // as a comparison argument. Only a proven array may resolve
                        // that reading; the occurrence retains the entire statement
                        // and the right-hand expression keeps its own binding gaps.
                        return tail.is_empty()
                            || (statement_assignment
                                && tail.strip_prefix(b"=").is_some_and(|value| {
                                    let value = value.trim_ascii_start();
                                    !value.is_empty()
                                        && !matches!(value.first(), Some(b'=' | b'\''))
                                }));
                    }
                }
                b'\'' => return false,
                _ => {}
            }
        }
        index += 1;
    }
    false
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("vba-binding-capture").expect("built-in VBA diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn statement_label_keys_compare_integer_values_and_reject_invalid_labels() {
        for (text, expected) in [
            ("Finish", "finish"),
            ("CLEAN_UP", "clean_up"),
            ("0", "0"),
            ("0010", "10"),
            ("&HA", "10"),
            ("&o12", "10"),
            ("&12", "10"),
            ("10%", "10"),
            ("&H8000&", "32768"),
            ("65535", "65535"),
            ("&H10000", "65536"),
            ("2147483647&", "2147483647"),
        ] {
            assert_eq!(
                super::statement_label_key(text).as_deref(),
                Some(expected),
                "{text}"
            );
        }
        for text in [
            "",
            "Randomize",
            "randomize",
            "_label",
            "étiquette",
            "Done$",
            "10.0",
            "1E1",
            "-1",
            "&H",
            "&O8",
            "2147483648",
            "4294967296",
            "&HFFFF",
            "&O100000",
            "32768%",
            "&H8000%",
            "10!",
            "10#",
        ] {
            assert_eq!(super::statement_label_key(text), None, "{text}");
        }
    }

    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;

    #[test]
    fn array_application_suffix_requires_closed_indices_and_statement_context() {
        for suffix in [b"(1)".as_slice(), b" (Inner(\"(\")) ", b"(\"x\"\"y\")"] {
            assert!(complete_array_application_suffix(suffix, false));
        }
        for suffix in [
            b"(1) = Len(\"x\")".as_slice(),
            b"(1)(2)",
            b"(\"unterminated)",
            b"(1) ' comment",
        ] {
            assert!(!complete_array_application_suffix(suffix, false));
        }
        assert!(complete_array_application_suffix(b"(1) = Len(\"x\")", true));
        for suffix in [
            b"(1) =".as_slice(),
            b"(1) == value",
            b"(1) = ' comment",
            b"(1)(2) = value",
            b"(1) + value",
        ] {
            assert!(!complete_array_application_suffix(suffix, true));
        }
    }

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
    fn enum_inventory_preserves_capture_order_and_missing_identities() {
        let source = b"Phase Ready Phase.Ready Ready";
        let original = vec![
            fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 29),
            fact(2, Some(1), SyntaxFactKind::Scope, "vba.enum.scope", 0, 11),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                "vba.enum.declaration",
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
            fact(
                5,
                Some(3),
                SyntaxFactKind::Declaration,
                "vba.enum_constant.declaration",
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
                Some(1),
                SyntaxFactKind::Scope,
                "vba.sub_function.scope",
                12,
                29,
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Occurrence,
                "vba.member.reference",
                12,
                23,
            ),
            fact(
                9,
                Some(7),
                SyntaxFactKind::Occurrence,
                "vba.value_identifier.reference",
                24,
                29,
            ),
        ];
        let ty = SymbolId::from_bytes([1; 20]);
        let member = SymbolId::from_bytes([2; 20]);
        for missing in [None, Some(4), Some(6)] {
            let mut symbols = HashMap::from([(4, ty), (6, member)]);
            if let Some(id) = missing {
                symbols.remove(&id);
            }
            for rotation in 0..original.len() {
                let mut facts = original.clone();
                facts.rotate_left(rotation);
                for reverse in [false, true] {
                    if reverse {
                        facts.reverse();
                    }
                    let bindings =
                        VbaBindings::new(&facts, source, &symbols, 256, &Cancellation::new())
                            .unwrap();
                    for (id, text, expected) in [
                        (8, "Phase.Ready", missing.is_none().then_some(member)),
                        (9, "Ready", (missing != Some(6)).then_some(member)),
                    ] {
                        let site = facts.iter().find(|fact| fact.local_id() == id).unwrap();
                        let result = bindings.resolve(site, text, &Cancellation::new()).unwrap();
                        assert_eq!(result.as_ref().map(|binding| binding.symbol), expected);
                        assert!(result.is_none_or(|binding| !binding.is_call));
                    }
                }
            }
        }
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
    fn instance_ownership_requires_class_identity_and_matching_procedure_scope() {
        let symbol = SymbolId::from_bytes([9; 20]);
        for (class_end, parent, present, expected) in [
            (Some(16), 2, true, Some(symbol)),
            (Some(16), 2, false, None),
            (Some(15), 2, true, None),
            (Some(16), 1, true, None),
            (None, 2, true, None),
        ] {
            let mut facts = vec![
                fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 16),
                fact(
                    2,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "vba.sub_function.scope",
                    0,
                    16,
                ),
                fact(
                    4,
                    Some(parent),
                    SyntaxFactKind::Occurrence,
                    "vba.instance.reference",
                    14,
                    16,
                ),
            ];
            if let Some(end) = class_end {
                facts.push(fact(
                    3,
                    None,
                    SyntaxFactKind::Module,
                    "vba.class.module",
                    0,
                    end,
                ));
            }
            for _ in 0..2 {
                let symbols = if present {
                    HashMap::from([(3, symbol)])
                } else {
                    HashMap::new()
                };
                let cancellation = Cancellation::new();
                let plan =
                    VbaBindings::new(&facts, b"              Me", &symbols, 64, &cancellation)
                        .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 4).unwrap();
                assert_eq!(
                    plan.resolve(reference, "Me", &cancellation)
                        .unwrap()
                        .map(|binding| (binding.symbol, binding.is_call)),
                    expected.map(|symbol| (symbol, false))
                );
                assert!(
                    plan.resolve(reference, "[Me]", &cancellation)
                        .unwrap()
                        .is_none()
                );
                facts.reverse();
            }
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
    fn declared_type_inventory_preserves_order_and_missing_evidence() {
        let cancellation = Cancellation::new();
        let ty = SymbolId::from_bytes([1; 20]);
        let field = SymbolId::from_bytes([2; 20]);
        let parameter = SymbolId::from_bytes([3; 20]);
        let source = b"Coord X point Coord point.X";
        let original = vec![
            fact(1, None, SyntaxFactKind::Scope, "vba.file.scope", 0, 27),
            fact(2, Some(1), SyntaxFactKind::Scope, "vba.struct.scope", 0, 7),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                "vba.struct.declaration",
                0,
                7,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                0,
                5,
            ),
            fact(
                5,
                Some(3),
                SyntaxFactKind::Declaration,
                "vba.field.declaration",
                6,
                7,
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                6,
                7,
            ),
            fact(
                7,
                Some(1),
                SyntaxFactKind::Scope,
                "vba.sub_function.scope",
                8,
                27,
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Declaration,
                "vba.parameter.declaration",
                8,
                19,
            ),
            fact(
                9,
                Some(8),
                SyntaxFactKind::Occurrence,
                "vba.identifier.definition",
                8,
                13,
            ),
            fact(
                10,
                Some(8),
                SyntaxFactKind::Occurrence,
                "vba.direct_type_identifier.reference",
                14,
                19,
            ),
            fact(
                11,
                Some(7),
                SyntaxFactKind::Occurrence,
                "vba.member.reference",
                20,
                27,
            ),
        ];
        for missing in [None, Some(4), Some(6), Some(9), Some(10)] {
            let mut symbols = HashMap::from([(4, ty), (6, field), (9, parameter)]);
            let mut facts = original.clone();
            if let Some(missing) = missing {
                symbols.remove(&missing);
                if missing == 10 {
                    facts.retain(|fact| fact.local_id() != 10);
                }
            }
            for _ in 0..2 {
                let plan = VbaBindings::new(&facts, source, &symbols, 64, &cancellation).unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 11).unwrap();
                assert_eq!(
                    plan.resolve(reference, "point.X", &cancellation)
                        .unwrap()
                        .map(|binding| (binding.symbol, binding.is_call)),
                    missing.is_none().then_some((field, false))
                );
                facts.reverse();
            }
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
