//! Resolves authored Groovy values, local types and explicit import selections.
//! Value visibility excludes initializers and noncapturing method/type boundaries;
//! type parameters use a separate namespace and respect static owner boundaries.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::GroovyImportedType;

struct Binding {
    visible_from: u64,
    symbol: Option<SymbolId>,
}

struct ImportedType {
    evidence: GroovyImportedType,
    qualified_name: String,
}

#[derive(Clone, Copy)]
struct LocalType {
    declaration: u64,
    symbol: Option<SymbolId>,
}

pub(super) struct GroovyBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    iterable_ends: BTreeMap<u64, u64>,
    bindings: BTreeMap<(u64, &'a str), Binding>,
    type_bindings: BTreeMap<(u64, &'a str), Option<SymbolId>>,
    local_types: BTreeMap<(u64, &'a str), Option<LocalType>>,
    module_packages: BTreeMap<u64, Option<String>>,
    type_names: BTreeMap<u64, (&'a str, Option<SymbolId>)>,
    inherited_scopes: BTreeSet<u64>,
    imports: BTreeMap<String, Option<ImportedType>>,
    unresolved_imports: bool,
    maximum_name_bytes: usize,
}

impl<'a> GroovyBindings<'a> {
    /// Builds a file-bounded visibility plan; absent entity identities still shadow.
    ///
    /// # Errors
    /// Returns cancellation or invalid capture/source relationships.
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            facts: BTreeMap::new(),
            iterable_ends: BTreeMap::new(),
            bindings: BTreeMap::new(),
            type_bindings: BTreeMap::new(),
            local_types: BTreeMap::new(),
            module_packages: BTreeMap::new(),
            type_names: BTreeMap::new(),
            inherited_scopes: BTreeSet::new(),
            imports: BTreeMap::new(),
            unresolved_imports: false,
            maximum_name_bytes,
        };
        cancellation.check()?;
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "groovy.iterable.expression" {
                let mut parent = fact.parent();
                for _ in 0..facts.len() {
                    cancellation.check()?;
                    let Some(id) = parent else {
                        break;
                    };
                    let owner = result.fact(id)?;
                    if owner.syntax_kind().as_str() == "groovy.enhanced_for.scope" {
                        result.iterable_ends.insert(id, fact.span().end_byte());
                        break;
                    }
                    parent = owner.parent();
                }
            }
        }
        for fact in facts {
            cancellation.check()?;
            let is_type = fact.syntax_kind().as_str() == "groovy.type_identifier.definition";
            if !is_type && fact.syntax_kind().as_str() != "groovy.identifier.definition" {
                continue;
            }
            let visibility = if is_type {
                result
                    .type_owner(fact, cancellation)?
                    .map(|owner| (owner, 0))
            } else {
                result.visibility(fact, cancellation)?
            };
            let Some((scope, visible_from)) = visibility else {
                continue;
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let name = source.get(start..end).ok_or_else(invalid_capture)?;
            if name.len() > maximum_name_bytes {
                continue;
            }
            let name = std::str::from_utf8(name).map_err(|_| invalid_capture())?;
            if is_type {
                result
                    .type_bindings
                    .entry((scope, name))
                    .and_modify(|target| *target = None)
                    .or_insert_with(|| symbols.get(&fact.local_id()).copied());
                continue;
            }
            let binding = Binding {
                visible_from,
                symbol: symbols.get(&fact.local_id()).copied(),
            };
            // Duplicate local declarations are not valid Groovy shadowing.
            // Keep the name unavailable instead of selecting by traversal order.
            result
                .bindings
                .entry((scope, name))
                .and_modify(|existing| {
                    existing.symbol = None;
                    existing.visible_from = 0;
                })
                .or_insert(binding);
        }
        result.collect_type_context(source, symbols, maximum_name_bytes, cancellation)?;
        Ok(result)
    }

    fn collect_type_context(
        &mut self,
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut aliases = BTreeMap::new();
        let mut packages = BTreeMap::new();
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "groovy.qualified_identifier.definition"
                    | "groovy.package_ownership_unavailable.scope"
            ) && let Some(module) = self.file_module(fact, cancellation)?
            {
                let name =
                    if fact.syntax_kind().as_str() == "groovy.qualified_identifier.definition" {
                        rootlight_adapter_sdk::structural_captured_name_for_fact(
                            "groovy",
                            fact,
                            self.source_name(fact, source)?,
                            maximum_name_bytes,
                        )
                        .map(std::borrow::Cow::into_owned)
                    } else {
                        None
                    };
                packages
                    .entry(module)
                    .and_modify(|prior| *prior = None)
                    .or_insert(name);
            }
        }
        self.module_packages = packages;
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            match fact.syntax_kind().as_str() {
                "groovy.package_star_import.reference"
                | "groovy.static_import.reference"
                | "groovy.static_star_import.reference" => self.unresolved_imports = true,
                "groovy.import_alias.definition" => {
                    if let Some(owner) = self.import_owner(fact, cancellation)? {
                        let name = self.source_name(fact, source)?;
                        aliases
                            .entry(owner)
                            .and_modify(|prior| *prior = None)
                            .or_insert(Some(name));
                    }
                }
                "groovy.inheritance.expression" => {
                    if let Some(owner) = self.type_context(fact.parent(), cancellation)? {
                        self.inherited_scopes.insert(owner);
                    }
                }
                "groovy.identifier.definition" => {
                    let Some(declaration) = fact.parent().map(|id| self.fact(id)).transpose()?
                    else {
                        continue;
                    };
                    if !is_type_declaration(declaration) {
                        continue;
                    }
                    if let Some(owner) = self.type_context(declaration.parent(), cancellation)? {
                        let name = self.source_name(fact, source)?;
                        if name.len() <= maximum_name_bytes {
                            self.type_names.insert(
                                declaration.local_id(),
                                (name, symbols.get(&fact.local_id()).copied()),
                            );
                            self.local_types
                                .entry((owner, name))
                                .and_modify(|prior| *prior = None)
                                .or_insert_with(|| {
                                    Some(LocalType {
                                        declaration: declaration.local_id(),
                                        symbol: symbols.get(&fact.local_id()).copied(),
                                    })
                                });
                        }
                    }
                }
                _ => {}
            }
        }
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "groovy.type_import.reference" {
                continue;
            }
            let Some(owner) = self.import_owner(fact, cancellation)? else {
                continue;
            };
            let text = self.source_name(fact, source)?;
            let Some(target) = rootlight_adapter_sdk::structural_captured_name_for_fact(
                "groovy",
                fact,
                text,
                maximum_name_bytes,
            ) else {
                continue;
            };
            let name = match aliases.get(&owner) {
                Some(Some(alias)) => *alias,
                Some(None) => continue,
                None => target.rsplit('.').next().ok_or_else(invalid_capture)?,
            };
            if name.len() > maximum_name_bytes {
                continue;
            }
            let binding = GroovyImportedType {
                import: fact.span(),
                target: content_hash(target.as_bytes()),
            };
            self.imports
                .entry(name.to_owned())
                .and_modify(|prior| *prior = None)
                .or_insert_with(|| {
                    Some(ImportedType {
                        evidence: binding,
                        qualified_name: target.into_owned(),
                    })
                });
        }
        Ok(())
    }

    fn source_name(&self, fact: &SyntaxFact, source: &'a [u8]) -> Result<&'a str, AdapterError> {
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        std::str::from_utf8(source.get(start..end).ok_or_else(invalid_capture)?)
            .map_err(|_| invalid_capture())
    }

    fn import_owner(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { return Ok(None) };
            let owner = self.fact(id)?;
            if owner.syntax_kind().as_str() == "groovy.import.scope" {
                return Ok(Some(id));
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    fn type_context(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { return Ok(None) };
            let owner = self.fact(id)?;
            if is_type_declaration(owner) || owner.syntax_kind().as_str() == "groovy.file.module" {
                return Ok(Some(id));
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    /// Returns a native import selection with simple and qualified type precedence.
    ///
    /// # Errors
    /// Returns cancellation or malformed capture ancestry. Unresolved inheritance
    /// blocks simple type selection, but not a qualified import alias.
    pub(super) fn imported_type(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<GroovyImportedType>, AdapterError> {
        cancellation.check()?;
        if !matches!(
            fact.syntax_kind().as_str(),
            "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
        ) {
            return Ok(None);
        }
        let Some(name) = rootlight_adapter_sdk::structural_captured_name_for_fact(
            "groovy",
            fact,
            name,
            self.maximum_name_bytes,
        ) else {
            return Ok(None);
        };
        let (root, suffix) = name
            .split_once('.')
            .map_or((name.as_ref(), None), |(root, suffix)| (root, Some(suffix)));
        let Some(binding) = self.imports.get(root).and_then(Option::as_ref) else {
            return Ok(None);
        };
        // ResolveVisitor checks exact module class names before import aliases.
        // Generic and nested simple-name lookup does not apply to a dotted type.
        let shadowed = if suffix.is_some() {
            self.module_type(fact, &name, cancellation)?.is_some()
        } else {
            self.type_parameter(fact, root, cancellation)?.is_some()
                || self.local_type(fact, root, cancellation)?.is_some()
        };
        if shadowed {
            return Ok(None);
        }
        let mut evidence = binding.evidence;
        if let Some(suffix) = suffix {
            // Only the native qualified-type node admits this substitution. Field
            // access and arbitrary dotted expressions do not prove a type target.
            let Some(target) =
                bounded_type_path(&binding.qualified_name, suffix, self.maximum_name_bytes)?
            else {
                return Ok(None);
            };
            evidence.target = content_hash(target.as_bytes());
        }
        Ok(Some(evidence))
    }

    fn file_module(
        &self,
        reference: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut parent = Some(reference.local_id());
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { return Ok(None) };
            let owner = self.fact(id)?;
            if owner.syntax_kind().as_str() == "groovy.file.module" {
                return Ok(Some(id));
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    fn module_type(
        &self,
        reference: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<Option<SymbolId>>, AdapterError> {
        let Some((package, name)) = name.rsplit_once('.') else {
            return Ok(None);
        };
        let Some(module) = self.file_module(reference, cancellation)? else {
            return Ok(None);
        };
        if self.module_packages.get(&module).and_then(Option::as_deref) != Some(package) {
            return Ok(None);
        }
        Ok(self
            .local_types
            .get(&(module, name))
            .map(|target| target.and_then(|target| target.symbol)))
    }

    fn type_owners(
        &self,
        reference: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<(Vec<u64>, Option<u64>), AdapterError> {
        let mut parent = reference.parent();
        let mut owners = Vec::new();
        let mut module = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok((owners, module));
            };
            let owner = self.fact(id)?;
            if is_type_declaration(owner) {
                owners.push(id);
            }
            if owner.syntax_kind().as_str() == "groovy.file.module" {
                module = Some(id);
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    fn local_type(
        &self,
        reference: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<Option<SymbolId>>, AdapterError> {
        let (owners, module) = self.type_owners(reference, cancellation)?;
        if let Some(&current) = owners.first()
            && let Some(&(current_name, target)) = self.type_names.get(&current)
            && current_name == name
        {
            return Ok(Some(target));
        }
        // Groovy checks the current type's children first, then enclosing
        // types from outermost to innermost (ResolveVisitor.resolveNestedClass).
        for &owner in owners.iter().take(1).chain(owners.iter().skip(1).rev()) {
            cancellation.check()?;
            if let Some(target) = self.local_types.get(&(owner, name)) {
                return Ok(Some(target.and_then(|target| target.symbol)));
            }
            if self.inherited_scopes.contains(&owner) {
                return Ok(Some(None));
            }
        }
        Ok(module.and_then(|module| {
            self.local_types
                .get(&(module, name))
                .map(|target| target.and_then(|target| target.symbol))
        }))
    }

    fn nested_type_path(
        &self,
        mut owner: u64,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<Option<LocalType>>, AdapterError> {
        let mut target = None;
        for component in name.split('.') {
            cancellation.check()?;
            let Some(child) = self.local_types.get(&(owner, component)) else {
                return Ok(self.inherited_scopes.contains(&owner).then_some(None));
            };
            let Some(child) = child.filter(|child| child.symbol.is_some()) else {
                return Ok(Some(None));
            };
            owner = child.declaration;
            target = Some(child);
        }
        Ok(Some(target))
    }

    fn qualified_local_type(
        &self,
        reference: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        if let Some(target) = self.module_type(reference, name, cancellation)? {
            return Ok(target);
        }
        let Some((root, _)) = name.split_once('.') else {
            return Ok(None);
        };
        // Import selection precedes the compiler's dotted-to-nested lookup.
        // Unimplemented static/star selection cannot justify a local fallback.
        if self.imports.contains_key(root) || self.unresolved_imports {
            return Ok(None);
        }
        let (owners, module) = self.type_owners(reference, cancellation)?;
        if let Some(module) = module {
            match self.module_packages.get(&module) {
                Some(Some(package)) => {
                    if let Some(path) = name
                        .strip_prefix(package)
                        .and_then(|path| path.strip_prefix('.'))
                        && let Some(target) = self.nested_type_path(module, path, cancellation)?
                    {
                        return Ok(target.and_then(|target| target.symbol));
                    }
                }
                Some(None) => return Ok(None),
                None => {}
            }
            if let Some(Some(target)) = self.nested_type_path(module, name, cancellation)?
                && owners.first() == Some(&target.declaration)
            {
                return Ok(target.symbol);
            }
        }
        // Match the entire child path before moving to the next enclosing scope:
        // a same-named prefix with no matching child does not shadow a full path.
        for &owner in owners.iter().take(1).chain(owners.iter().skip(1).rev()) {
            if let Some(target) = self.nested_type_path(owner, name, cancellation)? {
                return Ok(target.and_then(|target| target.symbol));
            }
        }
        let Some(module) = module else {
            return Ok(None);
        };
        Ok(self
            .nested_type_path(module, name, cancellation)?
            .flatten()
            .and_then(|target| target.symbol))
    }

    /// Returns only an exact authored lexical target, never a runtime dispatch guess.
    ///
    /// # Errors
    /// Returns cancellation or malformed cyclic/missing capture ancestry.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "groovy.qualified_type.reference" {
            let Some(name) = rootlight_adapter_sdk::structural_captured_name_for_fact(
                "groovy",
                fact,
                name,
                self.maximum_name_bytes,
            ) else {
                return Ok(None);
            };
            return self.qualified_local_type(fact, &name, cancellation);
        }
        if fact.syntax_kind().as_str() == "groovy.type_identifier.reference" {
            return self.resolve_type(fact, name, cancellation);
        }
        if fact.syntax_kind().as_str() != "groovy.identifier.reference" {
            return Ok(None);
        }
        let mut scope = self.nearest_scope(fact.parent(), cancellation)?;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if let Some(binding) = self.bindings.get(&(current, name))
                && fact.span().start_byte() >= binding.visible_from
            {
                return Ok(binding.symbol);
            }
            let owner = self.fact(current)?;
            if matches!(
                owner.syntax_kind().as_str(),
                "groovy.function.scope"
                    | "groovy.static_function.scope"
                    | "groovy.constructor.scope"
                    | "groovy.type_body.scope"
                    | "groovy.file.scope"
            ) || (name == "it"
                && owner.syntax_kind().as_str() == "groovy.implicit_closure.scope")
            {
                return Ok(None);
            }
            scope = self.nearest_scope(owner.parent(), cancellation)?;
        }
        Err(invalid_capture())
    }

    fn type_owner(
        &self,
        definition: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut parent = definition.parent();
        let mut parameter_seen = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Declaration {
                if !parameter_seen {
                    if fact.syntax_kind().as_str() != "groovy.type_parameter.declaration" {
                        return Ok(None);
                    }
                    parameter_seen = true;
                } else if matches!(
                    fact.syntax_kind().as_str(),
                    "groovy.class.declaration"
                        | "groovy.interface.declaration"
                        | "groovy.trait.declaration"
                        | "groovy.record.declaration"
                        | "groovy.function.declaration"
                        | "groovy.constructor.declaration"
                ) {
                    return Ok(Some(id));
                } else {
                    return Ok(None);
                }
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn resolve_type(
        &self,
        reference: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        if let Some(target) = self.type_parameter(reference, name, cancellation)? {
            return Ok(target);
        }
        Ok(self.local_type(reference, name, cancellation)?.flatten())
    }

    fn type_parameter(
        &self,
        reference: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<Option<SymbolId>>, AdapterError> {
        let mut parent = reference.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let owner = self.fact(id)?;
            if let Some(target) = self.type_bindings.get(&(id, name)) {
                return Ok(Some(*target));
            }
            // Static owners can declare their own generic parameters, but cannot
            // capture those of an enclosing class. Their declaration precedes this scope.
            if matches!(
                owner.syntax_kind().as_str(),
                "groovy.static_function.scope"
                    | "groovy.static_context.scope"
                    | "groovy.file.scope"
            ) {
                return Ok(None);
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    fn visibility(
        &self,
        definition: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, u64)>, AdapterError> {
        let mut parent = definition.parent();
        let mut declaration = None;
        let mut destructuring_end = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Declaration && declaration.is_none() {
                if !matches!(
                    fact.syntax_kind().as_str(),
                    "groovy.variable.declaration" | "groovy.parameter.declaration"
                ) {
                    return Ok(None);
                }
                declaration = Some(fact);
            }
            if fact.syntax_kind().as_str() == "groovy.destructuring.scope" {
                destructuring_end = Some(fact.span().end_byte());
            }
            if is_scope(fact) {
                let Some(declaration) = declaration else {
                    return Ok(None);
                };
                if fact.syntax_kind().as_str() == "groovy.type_body.scope" {
                    return Ok(None);
                }
                let visible_from = if fact.syntax_kind().as_str() == "groovy.enhanced_for.scope" {
                    let iterable_end = self
                        .iterable_ends
                        .get(&id)
                        .copied()
                        .unwrap_or(fact.span().end_byte());
                    // An unbraced loop body can itself declare a local. Only the
                    // header binding becomes visible after evaluating the iterable.
                    if declaration.span().end_byte() <= iterable_end {
                        iterable_end
                    } else {
                        destructuring_end.unwrap_or(declaration.span().end_byte())
                    }
                } else {
                    destructuring_end.unwrap_or(declaration.span().end_byte())
                };
                return Ok(Some((id, visible_from)));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn nearest_scope(
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
            if is_scope(fact) {
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

fn is_type_declaration(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "groovy.class.declaration"
            | "groovy.interface.declaration"
            | "groovy.trait.declaration"
            | "groovy.record.declaration"
            | "groovy.enum.declaration"
    )
}

fn is_scope(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Scope
        && matches!(
            fact.syntax_kind().as_str(),
            "groovy.file.scope"
                | "groovy.function.scope"
                | "groovy.static_function.scope"
                | "groovy.constructor.scope"
                | "groovy.block.scope"
                | "groovy.type_body.scope"
                | "groovy.for.scope"
                | "groovy.enhanced_for.scope"
                | "groovy.catch.scope"
                | "groovy.try.scope"
                | "groovy.closure.scope"
                | "groovy.implicit_closure.scope"
        )
}

fn bounded_type_path(
    prefix: &str,
    suffix: &str,
    maximum: usize,
) -> Result<Option<String>, AdapterError> {
    let Some(length) = prefix
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(suffix.len()))
    else {
        return Ok(None);
    };
    if length > maximum {
        return Ok(None);
    }
    let mut name = String::new();
    name.try_reserve_exact(length)
        .map_err(|_| invalid_capture())?;
    name.push_str(prefix);
    name.push('.');
    name.push_str(suffix);
    Ok(Some(name))
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("groovy-binding-capture")
            .expect("built-in Groovy capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

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
            fact(1, None, SyntaxFactKind::Scope, "groovy.file.scope", 0, 8),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.variable.declaration",
                0,
                3,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.reference",
                4,
                5,
            ),
        ]
    }

    #[test]
    fn qualified_module_names_preserve_missing_and_duplicate_identities() {
        let source = b"sample Local sample.Local";
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Module, "groovy.file.module", 0, 25),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                "groovy.qualified_identifier.definition",
                0,
                6,
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.class.declaration",
                7,
                12,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                7,
                12,
            ),
            fact(
                5,
                Some(1),
                SyntaxFactKind::Occurrence,
                "groovy.qualified_type.reference",
                13,
                25,
            ),
        ];
        let cancellation = Cancellation::new();
        let symbol = SymbolId::from_bytes([8; 20]);
        for missing in [false, true] {
            let symbols = if missing {
                HashMap::new()
            } else {
                HashMap::from([(4, symbol)])
            };
            let plan = GroovyBindings::new(&facts, source, &symbols, 100, &cancellation).unwrap();
            assert_eq!(
                plan.module_type(&facts[4], "sample.Local", &cancellation)
                    .unwrap(),
                Some((!missing).then_some(symbol))
            );
        }
        facts.push(fact(
            6,
            Some(1),
            SyntaxFactKind::Declaration,
            "groovy.class.declaration",
            7,
            12,
        ));
        facts.push(fact(
            7,
            Some(6),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.definition",
            7,
            12,
        ));
        let symbols = HashMap::from([(4, symbol), (7, SymbolId::from_bytes([9; 20]))]);
        let plan = GroovyBindings::new(&facts, source, &symbols, 100, &cancellation).unwrap();
        assert_eq!(
            plan.module_type(&facts[4], "sample.Local", &cancellation)
                .unwrap(),
            Some(None)
        );
        assert_eq!(
            plan.resolve(&facts[4], "sample.Local", &cancellation)
                .unwrap(),
            None
        );
    }

    #[test]
    fn nested_type_paths_require_complete_unique_identities() {
        let source = b"Outer Inner Outer.Inner";
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Module, "groovy.file.module", 0, 23),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.class.declaration",
                0,
                11,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                5,
            ),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Declaration,
                "groovy.class.declaration",
                6,
                11,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                6,
                11,
            ),
            fact(
                6,
                Some(1),
                SyntaxFactKind::Occurrence,
                "groovy.qualified_type.reference",
                12,
                23,
            ),
        ];
        let outer = SymbolId::from_bytes([1; 20]);
        let inner = SymbolId::from_bytes([2; 20]);
        let cancellation = Cancellation::new();
        for _ in 0..2 {
            for (symbols, maximum, expected) in [
                (HashMap::from([(3, outer), (5, inner)]), 11, Some(inner)),
                (HashMap::from([(3, outer), (5, inner)]), 10, None),
                (HashMap::from([(3, outer)]), 11, None),
                (HashMap::from([(5, inner)]), 11, None),
            ] {
                let plan =
                    GroovyBindings::new(&facts, source, &symbols, maximum, &cancellation).unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 6).unwrap();
                assert_eq!(
                    plan.resolve(reference, "Outer.Inner", &cancellation)
                        .unwrap(),
                    expected
                );
            }
            facts.reverse();
        }
        facts.push(fact(
            7,
            Some(1),
            SyntaxFactKind::Declaration,
            "groovy.class.declaration",
            0,
            5,
        ));
        facts.push(fact(
            8,
            Some(7),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.definition",
            0,
            5,
        ));
        let symbols = HashMap::from([(3, outer), (5, inner), (8, SymbolId::from_bytes([3; 20]))]);
        let plan = GroovyBindings::new(&facts, source, &symbols, 11, &cancellation).unwrap();
        let reference = facts.iter().find(|fact| fact.local_id() == 6).unwrap();
        assert_eq!(
            plan.resolve(reference, "Outer.Inner", &cancellation)
                .unwrap(),
            None
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            plan.resolve(reference, "Outer.Inner", &cancellation)
                .is_err()
        );
    }

    #[test]
    fn qualified_type_composition_obeys_exact_byte_limits() {
        assert_eq!(
            bounded_type_path("sample", "Local", 12).unwrap().as_deref(),
            Some("sample.Local")
        );
        assert_eq!(bounded_type_path("sample", "Local", 11).unwrap(), None);
    }

    #[test]
    fn type_bindings_preserve_shadowing_without_identities_or_capture_order() {
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "groovy.file.scope", 0, 8),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.class.declaration",
                0,
                8,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                "groovy.type_parameter.declaration",
                0,
                1,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(2),
                SyntaxFactKind::Declaration,
                "groovy.function.declaration",
                2,
                8,
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Declaration,
                "groovy.type_parameter.declaration",
                2,
                3,
            ),
            fact(
                7,
                Some(6),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.definition",
                2,
                3,
            ),
            fact(
                8,
                Some(5),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.reference",
                4,
                5,
            ),
            fact(
                9,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.reference",
                6,
                7,
            ),
        ];
        let outer = SymbolId::from_bytes([8; 20]);
        let inner = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        for symbols in [
            HashMap::from([(4, outer), (7, inner)]),
            HashMap::from([(4, outer)]),
        ] {
            for _ in 0..2 {
                let plan =
                    GroovyBindings::new(&facts, b"T T T T ", &symbols, 64, &cancellation).unwrap();
                for (id, expected) in [(8, symbols.get(&7).copied()), (9, Some(outer))] {
                    let read = facts.iter().find(|fact| fact.local_id() == id).unwrap();
                    assert_eq!(plan.resolve(read, "T", &cancellation).unwrap(), expected);
                }
                facts.reverse();
            }
        }
        facts.push(fact(
            10,
            Some(5),
            SyntaxFactKind::Declaration,
            "groovy.type_parameter.declaration",
            7,
            8,
        ));
        facts.push(fact(
            11,
            Some(10),
            SyntaxFactKind::Occurrence,
            "groovy.type_identifier.definition",
            7,
            8,
        ));
        let plan = GroovyBindings::new(
            &facts,
            b"T T T TT",
            &HashMap::from([(4, outer), (7, inner)]),
            64,
            &cancellation,
        )
        .unwrap();
        let read = facts.iter().find(|fact| fact.local_id() == 8).unwrap();
        assert_eq!(plan.resolve(read, "T", &cancellation).unwrap(), None);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(plan.resolve(read, "T", &cancellation).is_err());
        assert!(
            GroovyBindings::new(&facts, b"T T T TT", &HashMap::new(), 64, &cancellation).is_err()
        );
    }

    #[test]
    fn malformed_type_capture_ancestry_and_source_return_errors() {
        let cancellation = Cancellation::new();
        for parent in [Some(99), Some(2)] {
            let facts = vec![
                fact(1, None, SyntaxFactKind::Scope, "groovy.file.scope", 0, 8),
                fact(2, parent, SyntaxFactKind::Scope, "groovy.block.scope", 0, 8),
                fact(
                    3,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    "groovy.type_identifier.definition",
                    0,
                    1,
                ),
            ];
            assert!(
                GroovyBindings::new(&facts, b"T       ", &HashMap::new(), 64, &cancellation)
                    .is_err()
            );
            let plan =
                GroovyBindings::new(&facts[..2], b"T       ", &HashMap::new(), 64, &cancellation)
                    .unwrap();
            let read = fact(
                4,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.reference",
                2,
                3,
            );
            assert!(plan.resolve(&read, "T", &cancellation).is_err());
        }
        let facts = vec![
            fact(
                1,
                None,
                SyntaxFactKind::Declaration,
                "groovy.class.declaration",
                0,
                8,
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.type_parameter.declaration",
                0,
                8,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.type_identifier.definition",
                7,
                8,
            ),
        ];
        for source in [b"T".as_slice(), b"       \xff".as_slice()] {
            assert!(
                GroovyBindings::new(&facts, source, &HashMap::new(), 64, &cancellation).is_err()
            );
        }
    }

    #[test]
    fn capture_order_does_not_change_targets_and_missing_symbols_remain_unresolved() {
        let mut facts = facts();
        let symbol = SymbolId::from_bytes([9; 20]);
        let symbols = HashMap::from([(3, symbol)]);
        let cancellation = Cancellation::new();
        for _ in 0..2 {
            let plan =
                GroovyBindings::new(&facts, b"x   x   ", &symbols, 64, &cancellation).unwrap();
            let read = facts.iter().find(|fact| fact.local_id() == 4).unwrap();
            assert_eq!(
                plan.resolve(read, "x", &cancellation).unwrap(),
                Some(symbol)
            );
            let plan = GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation)
                .unwrap();
            assert_eq!(plan.resolve(read, "x", &cancellation).unwrap(), None);
            facts.reverse();
        }
    }

    #[test]
    fn duplicate_bindings_do_not_select_an_arbitrary_target() {
        let mut facts = facts();
        facts.push(fact(
            5,
            Some(1),
            SyntaxFactKind::Declaration,
            "groovy.variable.declaration",
            6,
            7,
        ));
        facts.push(fact(
            6,
            Some(5),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.definition",
            6,
            7,
        ));
        let symbol = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        let plan = GroovyBindings::new(
            &facts,
            b"x   x x ",
            &HashMap::from([(3, symbol)]),
            64,
            &cancellation,
        )
        .unwrap();
        assert_eq!(plan.resolve(&facts[3], "x", &cancellation).unwrap(), None);
    }

    #[test]
    fn malformed_ancestry_and_source_ranges_return_typed_errors() {
        let cancellation = Cancellation::new();
        for bad in [
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(99),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(5),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                20,
                21,
            ),
        ] {
            let mut facts = facts();
            facts.push(bad);
            assert!(
                GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation)
                    .is_err()
            );
        }
        let mut facts = facts();
        facts.push(fact(
            5,
            Some(5),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.reference",
            6,
            7,
        ));
        let plan =
            GroovyBindings::new(&facts, b"x   x x ", &HashMap::new(), 64, &cancellation).unwrap();
        assert!(plan.resolve(&facts[4], "x", &cancellation).is_err());
    }

    #[test]
    fn both_plan_construction_and_resolution_honor_cancellation() {
        let facts = facts();
        let cancellation = Cancellation::new();
        let plan =
            GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation).unwrap();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(plan.resolve(&facts[3], "x", &cancellation).is_err());
        assert!(
            GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation).is_err()
        );
    }
}
