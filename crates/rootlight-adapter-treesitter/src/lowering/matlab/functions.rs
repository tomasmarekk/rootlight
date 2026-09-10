//! File-local and nested MATLAB function visibility from native scopes.
//! Imported names and duplicate declarations block unsupported target guesses.

use super::*;

pub(super) struct Functions<'a> {
    parents: HashMap<u64, Option<u64>>,
    import_parents: HashMap<u64, Option<u64>>,
    declarations: BTreeMap<(u64, &'a str), Option<u64>>,
    imports: HashMap<u64, Option<BTreeSet<&'a str>>>,
    targets: BTreeSet<u64>,
}

impl<'a> Functions<'a> {
    pub(super) fn new(
        facts: &[SyntaxFact],
        source: &'a str,
        owners: &HashMap<u64, Option<u64>>,
        workspaces: &HashMap<u64, Workspace>,
        names: &HashMap<SourceSpan, Option<&'a str>>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            parents: HashMap::new(),
            import_parents: HashMap::new(),
            declarations: BTreeMap::new(),
            imports: HashMap::new(),
            targets: BTreeSet::new(),
        };
        let mut function_scopes = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if let Some(workspace) = workspaces.get(&fact.local_id()) {
                let parent = fact
                    .parent()
                    .and_then(|parent| owners.get(&parent).copied().flatten());
                result.parents.insert(fact.local_id(), parent);
                result
                    .import_parents
                    .insert(fact.local_id(), workspace.parent);
                if fact.syntax_kind().as_str() == "matlab.function.scope" {
                    function_scopes.insert(fact.span(), fact.local_id());
                }
            }
        }
        for fact in facts {
            cancellation.check()?;
            match fact.syntax_kind().as_str() {
                "matlab.function.declaration" => {
                    let Some(name) = names.get(&fact.span()).copied().flatten() else {
                        continue;
                    };
                    let Some(parent) = function_scopes
                        .get(&fact.span())
                        .and_then(|scope| result.parents.get(scope))
                        .copied()
                        .flatten()
                    else {
                        continue;
                    };
                    result
                        .declarations
                        .entry((parent, name))
                        .and_modify(|target| *target = None)
                        .or_insert(Some(fact.local_id()));
                    result.targets.insert(fact.local_id());
                }
                "matlab.import.reference" => {
                    let Some(owner) = owners.get(&fact.local_id()).copied().flatten() else {
                        continue;
                    };
                    let text = source_text(source, fact)?
                        .strip_prefix("import")
                        .ok_or_else(invalid)?
                        .trim();
                    let entry = result
                        .imports
                        .entry(owner)
                        .or_insert_with(|| Some(BTreeSet::new()));
                    if text.starts_with('(') || text.is_empty() {
                        *entry = None;
                        continue;
                    }
                    for path in text.split_whitespace() {
                        cancellation.check()?;
                        let Some(names) = entry.as_mut() else { break };
                        let mut components = path.split('.').peekable();
                        let mut valid = true;
                        while let Some(component) = components.next() {
                            cancellation.check()?;
                            let last = components.peek().is_none();
                            if last && component == "*" && path.contains('.') {
                                break;
                            }
                            let mut chars = component.chars();
                            if !chars.next().is_some_and(char::is_alphabetic) {
                                valid = false;
                                break;
                            }
                            for ch in chars {
                                cancellation.check()?;
                                if !ch.is_alphanumeric() && ch != '_' {
                                    valid = false;
                                    break;
                                }
                            }
                            if !valid {
                                break;
                            }
                            if last {
                                names.insert(component);
                            }
                        }
                        if !valid {
                            *entry = None;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        owner: Option<u64>,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut scope = owner;
        // Explicit imports outrank even nearer nested declarations. Wildcard
        // imports do not; local functions precede them in MATLAB name resolution.
        for _ in 0..=self.import_parents.len() {
            cancellation.check()?;
            let Some(current) = scope else { break };
            if self
                .imports
                .get(&current)
                .is_some_and(|imports| imports.as_ref().is_none_or(|names| names.contains(name)))
            {
                return Ok(None);
            }
            scope = self
                .import_parents
                .get(&current)
                .copied()
                .ok_or_else(invalid)?;
        }
        if scope.is_some() {
            return Err(invalid());
        }
        scope = owner;
        for _ in 0..=self.parents.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if let Some(target) = self.declarations.get(&(current, name)) {
                return Ok(*target);
            }
            scope = self.parents.get(&current).copied().ok_or_else(invalid)?;
        }
        Err(invalid())
    }

    pub(super) fn is_function(&self, target: u64) -> bool {
        self.targets.contains(&target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn functions() -> Functions<'static> {
        Functions {
            parents: HashMap::from([(1, None), (2, Some(1))]),
            import_parents: HashMap::from([(1, None), (2, Some(1))]),
            declarations: BTreeMap::from([((1, "helper"), Some(10)), ((2, "helper"), Some(20))]),
            imports: HashMap::new(),
            targets: BTreeSet::from([10, 20]),
        }
    }

    #[test]
    fn explicit_import_barriers_precede_nearer_function_declarations() {
        let mut functions = functions();
        assert_eq!(
            functions
                .resolve(Some(2), "helper", &Cancellation::new())
                .unwrap(),
            Some(20)
        );
        functions
            .imports
            .insert(1, Some(BTreeSet::from(["helper"])));
        assert_eq!(
            functions
                .resolve(Some(2), "helper", &Cancellation::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn duplicate_function_names_block_outer_namesakes() {
        let mut functions = functions();
        functions.declarations.insert((2, "helper"), None);
        assert_eq!(
            functions
                .resolve(Some(2), "helper", &Cancellation::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn function_lookup_rejects_cycles_missing_scopes_and_cancellation() {
        let mut functions = functions();
        assert!(
            functions
                .resolve(Some(3), "missing", &Cancellation::new())
                .is_err()
        );
        functions.parents.insert(1, Some(2));
        assert!(
            functions
                .resolve(Some(2), "missing", &Cancellation::new())
                .is_err()
        );
        functions.import_parents.insert(1, Some(2));
        assert!(
            functions
                .resolve(Some(2), "helper", &Cancellation::new())
                .is_err()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(functions.resolve(Some(2), "helper", &cancellation).is_err());
    }
}
