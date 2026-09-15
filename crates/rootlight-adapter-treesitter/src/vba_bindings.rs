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

pub(super) struct VbaBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    bindings: BTreeMap<u64, BTreeMap<String, Option<Binding>>>,
    foreign_scopes: BTreeSet<u64>,
    constant_symbols: BTreeSet<SymbolId>,
    class_module: Option<&'a SyntaxFact>,
    class_symbol: Option<SymbolId>,
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
            class_symbol: None,
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
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "vba.class.module"
                && result.class_module.replace(fact).is_some()
            {
                return Err(invalid_capture());
            }
        }
        result.class_symbol = result
            .class_module
            .and_then(|fact| symbols.get(&fact.local_id()).copied());
        let mut required_parameter_scopes = BTreeSet::new();
        let mut enum_module_bindings = Vec::new();
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
            let enum_member = declaration.syntax_kind().as_str() == "vba.enum_constant.declaration";
            if enum_member {
                // Qualified lookup uses the enum's members, while simple names
                // participate in the enclosing module tier (MS-VBAL 5.6.10).
                result.insert(owner, name.as_deref(), symbol);
            }
            if owns_scope(declaration) || enum_member {
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
            if fact.syntax_kind().as_str() != "vba.direct_type_identifier.reference" {
                continue;
            }
            let Some(declaration) = self.declaration(fact.parent(), cancellation)? else {
                continue;
            };
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
            "vba.member.reference" | "vba.invoked_member.reference"
        ) {
            return self.field_reference(fact, text, cancellation);
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.type_identifier.reference" | "vba.direct_type_identifier.reference"
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
            if let Some(binding) = self.bindings.get(&owner).and_then(|names| names.get(&key)) {
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
