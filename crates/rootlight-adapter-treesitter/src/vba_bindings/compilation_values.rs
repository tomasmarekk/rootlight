//! Closed Boolean branch decisions retain every authored alternative.
//! Host/project values and unsupported constant initializers remain barriers.

use rootlight_ir::{MAX_VBA_COMPILATION_BRANCHES, SourceSpan, VbaCompilationBranch};
use rootlight_vba::{CompilationConditionLimits, CompilationConditionValue};

use super::*;

struct CompilerConstant {
    value: bool,
    declared_at: u64,
}

impl VbaBindings<'_> {
    pub(crate) fn compilations(&self) -> impl Iterator<Item = &VbaCompilation> {
        self.compilations.iter()
    }

    pub(crate) fn compilation_evaluation_proven(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if self.proven_compilation_facts.contains(&fact.local_id()) {
            return Ok(true);
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "vba.conditional_sub_function.declaration"
                | "vba.conditional_function.declaration"
                | "vba.conditional_property.declaration"
        ) {
            // A split procedure header needs its own grammar/identity proof;
            // proving a surrounding branch is not sufficient to retire its gap.
            return Ok(false);
        }
        Ok(false)
    }

    pub(super) fn compilation_activity(&self, fact: &SyntaxFact) -> VbaCompilationActivity {
        self.compilations
            .iter()
            .find(|model| {
                model.scope.file() == fact.span().file()
                    && model.scope.start_byte() <= fact.span().start_byte()
                    && model.scope.end_byte() >= fact.span().end_byte()
            })
            .map_or(VbaCompilationActivity::Unproven, |model| {
                model.activity_at(fact.span())
            })
    }

    pub(super) fn collect_compilation_values(
        &mut self,
        facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut headers = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        let mut branches = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        let mut constants = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        let mut modules = Vec::new();
        for fact in facts {
            cancellation.check()?;
            match fact.syntax_kind().as_str() {
                "vba.file.scope" => modules.push(fact),
                "vba.compilation_if.signature"
                | "vba.compilation_elseif.signature"
                | "vba.compilation_else.signature"
                | "vba.compilation_unknown.signature" => {
                    if let Some(group) = self.preprocessor_scope(fact.parent(), cancellation)? {
                        headers.entry(group).or_default().push(fact);
                    }
                }
                "vba.preprocessor_branch.scope" => {
                    if let Some(group) = self.preprocessor_scope(fact.parent(), cancellation)? {
                        branches.entry(group).or_default().push(fact);
                    }
                }
                "vba.compilation_constant.declaration" => {
                    if let Some(module) = self.compilation_scope(fact.parent(), cancellation)? {
                        constants.entry(module).or_default().push(fact);
                    }
                }
                _ => {}
            }
        }
        for values in headers.values_mut().chain(branches.values_mut()) {
            values.sort_unstable_by_key(|fact| fact.span().start_byte());
        }
        for module in modules {
            cancellation.check()?;
            if module.span().start_byte() == module.span().end_byte() {
                // No nonempty declaration or use can belong to an empty scope.
                continue;
            }
            let (values, valid_constants) = self.closed_compiler_constants(
                constants.get(&module.local_id()).map_or(&[], Vec::as_slice),
                cancellation,
            )?;
            let mut model = VbaCompilation {
                scope: module.span(),
                complete: true,
                branches: Vec::new(),
            };
            for declaration in facts.iter().filter(|fact| {
                matches!(
                    fact.syntax_kind().as_str(),
                    "vba.conditional_sub_function.declaration"
                        | "vba.conditional_function.declaration"
                        | "vba.conditional_property.declaration"
                )
            }) {
                cancellation.check()?;
                if self.compilation_scope(declaration.parent(), cancellation)?
                    == Some(module.local_id())
                {
                    // Split procedure headers carry alternative parameter lists
                    // without ordinary preprocessor scopes. Until inventoried,
                    // their absence cannot prove an unguarded module.
                    model.complete = false;
                }
            }
            for group in facts
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.preprocessor.scope")
            {
                cancellation.check()?;
                if self.compilation_scope(group.parent(), cancellation)? != Some(module.local_id())
                {
                    continue;
                }
                let group_headers: &[&SyntaxFact] =
                    headers.get(&group.local_id()).map_or(&[], Vec::as_slice);
                let group_branches: &[&SyntaxFact] =
                    branches.get(&group.local_id()).map_or(&[], Vec::as_slice);
                if group_headers.is_empty()
                    || group_headers.len() > MAX_VBA_COMPILATION_BRANCHES - model.branches.len()
                {
                    model.complete = false;
                    continue;
                }
                let mut alternatives = vec![None; group_headers.len()];
                for branch in group_branches {
                    cancellation.check()?;
                    let count = group_headers.partition_point(|header| {
                        header.span().start_byte() < branch.span().start_byte()
                    });
                    if count == 0 || alternatives[count - 1].replace(*branch).is_some() {
                        model.complete = false;
                    }
                }
                let decisions =
                    self.closed_group_decisions(group_headers, &values, cancellation)?;
                let known = valid_constants && model.complete && decisions.is_some();
                if known {
                    self.proven_compilation_facts.insert(group.local_id());
                }
                for (index, (header, branch)) in group_headers.iter().zip(alternatives).enumerate()
                {
                    cancellation.check()?;
                    let activity = if known {
                        if decisions.as_ref().and_then(|values| values.get(index)) == Some(&true) {
                            VbaCompilationActivity::Active
                        } else {
                            VbaCompilationActivity::Inactive
                        }
                    } else {
                        VbaCompilationActivity::Unproven
                    };
                    let region = if let Some(branch) = branch {
                        self.compilation_activities
                            .insert(branch.local_id(), activity);
                        branch.span()
                    } else {
                        // Native optional bodies have no scope node. Preserve
                        // their exact empty boundary after the directive line;
                        // this range cannot contain any declaration/use token.
                        self.empty_compilation_region(header, group, cancellation)?
                    };
                    model.branches.push(VbaCompilationBranch {
                        group: group.span(),
                        region,
                        activity,
                    });
                }
            }
            if !model.complete {
                // A truncated inventory cannot prove that a missing branch is
                // unguarded, nor support absolute adapter-side branch selection.
                for group in facts.iter().filter(|fact| {
                    fact.syntax_kind().as_str() == "vba.preprocessor.scope"
                        && fact.span().file() == model.scope.file()
                        && fact.span().start_byte() >= model.scope.start_byte()
                        && fact.span().end_byte() <= model.scope.end_byte()
                }) {
                    cancellation.check()?;
                    self.proven_compilation_facts.remove(&group.local_id());
                    for branch in branches.get(&group.local_id()).into_iter().flatten() {
                        self.compilation_activities
                            .insert(branch.local_id(), VbaCompilationActivity::Unproven);
                    }
                }
                model.branches.clear();
            }
            model.branches.sort_by_key(|branch| {
                (
                    branch.group.start_byte(),
                    branch.group.end_byte(),
                    branch.region.start_byte(),
                    branch.region.end_byte(),
                )
            });
            self.compilations.push(model);
        }
        Ok(())
    }

    fn closed_compiler_constants(
        &mut self,
        declarations: &[&SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<(BTreeMap<String, CompilerConstant>, bool), AdapterError> {
        let mut values = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut valid = true;
        for declaration in declarations {
            cancellation.check()?;
            let text = self.compilation_source(declaration)?;
            let Some(header) = rootlight_vba::compilation_constant_header(text, &mut || {
                cancellation.check().map_err(AdapterError::from)
            })?
            else {
                valid = false;
                continue;
            };
            let Some(name) = std::str::from_utf8(header.name())
                .ok()
                .filter(|name| name.is_ascii() && name.len() <= self.maximum_name_bytes)
            else {
                valid = false;
                continue;
            };
            let name = name.to_ascii_lowercase();
            if !names.insert(name.clone())
                || matches!(
                    name.as_str(),
                    "win16" | "win32" | "win64" | "mac" | "vba6" | "vba7"
                )
            {
                valid = false;
                continue;
            }
            let initializer = text
                .get(header.initializer_start()..)
                .ok_or_else(invalid_capture)?;
            // This gate supports closed Boolean initializers only. Forward
            // references, numeric/Variant coercion and project/host inputs need
            // separate evidence, not an implicit default value.
            let expression = initializer
                .split(|byte| *byte == b'\'')
                .next()
                .unwrap_or_default();
            let expression = expression.trim_ascii_end();
            let result = rootlight_vba::compilation_condition_value(
                expression,
                self.compilation_limits(),
                &mut |_| Ok::<_, AdapterError>(None),
                &mut || cancellation.check().map_err(AdapterError::from),
            )?;
            if let CompilationConditionValue::Proven(value) = result {
                values.insert(
                    name,
                    CompilerConstant {
                        value,
                        declared_at: declaration.span().start_byte(),
                    },
                );
                self.proven_compilation_facts.insert(declaration.local_id());
            } else {
                valid = false;
            }
        }
        if !valid {
            for declaration in declarations {
                self.proven_compilation_facts
                    .remove(&declaration.local_id());
            }
        }
        Ok((values, valid))
    }

    fn closed_group_decisions(
        &self,
        headers: &[&SyntaxFact],
        constants: &BTreeMap<String, CompilerConstant>,
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<bool>>, AdapterError> {
        let mut selected = false;
        let mut saw_else = false;
        let mut valid = !headers.is_empty();
        let mut decisions = Vec::new();
        for (index, header) in headers.iter().enumerate() {
            cancellation.check()?;
            let kind = header.syntax_kind().as_str();
            let value = if kind == "vba.compilation_else.signature" {
                valid &= index != 0 && !saw_else;
                saw_else = true;
                Some(true)
            } else {
                valid &= !saw_else
                    && if index == 0 {
                        kind == "vba.compilation_if.signature"
                    } else {
                        kind == "vba.compilation_elseif.signature"
                    };
                let value = rootlight_vba::compilation_condition_value(
                    self.compilation_source(header)?,
                    self.compilation_limits(),
                    &mut |name| {
                        let key = std::str::from_utf8(name)
                            .map_err(|_| invalid_capture())?
                            .to_ascii_lowercase();
                        Ok::<_, AdapterError>(
                            constants
                                .get(&key)
                                .filter(|constant| {
                                    constant.declared_at < header.span().start_byte()
                                })
                                .map(|constant| constant.value),
                        )
                    },
                    &mut || cancellation.check().map_err(AdapterError::from),
                )?;
                match value {
                    CompilationConditionValue::Proven(value) => Some(value),
                    CompilationConditionValue::Unproven | CompilationConditionValue::Malformed => {
                        None
                    }
                }
            };
            valid &= value.is_some();
            let included = !selected && value == Some(true);
            selected |= included;
            decisions.push(included);
        }
        // Every ElseIf must be evaluated, even after a selected earlier arm.
        Ok(valid.then_some(decisions))
    }

    fn compilation_limits(&self) -> CompilationConditionLimits {
        CompilationConditionLimits {
            maximum_name_bytes: self.maximum_name_bytes,
            // A live token stack cannot legitimately exceed the already bounded
            // source bytes; no new global ceiling or source-sized allocation.
            maximum_stack_entries: self.source.len(),
        }
    }

    fn compilation_source(&self, fact: &SyntaxFact) -> Result<&[u8], AdapterError> {
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        self.source.get(start..end).ok_or_else(invalid_capture)
    }

    fn empty_compilation_region(
        &self,
        header: &SyntaxFact,
        group: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<SourceSpan, AdapterError> {
        let start = if header.syntax_kind().as_str() == "vba.compilation_else.signature" {
            header.span().start_byte()
        } else {
            header.span().end_byte()
        };
        let begin = usize::try_from(start).map_err(|_| invalid_capture())?;
        let end = usize::try_from(group.span().end_byte()).map_err(|_| invalid_capture())?;
        let suffix = self.source.get(begin..end).ok_or_else(invalid_capture)?;
        let mut cursor = 0;
        let mut comment = false;
        while let Some(byte) = suffix.get(cursor) {
            cancellation.check()?;
            if *byte == b'\n' {
                let offset = u64::try_from(begin + cursor + 1).map_err(|_| invalid_capture())?;
                return SourceSpan::new(group.span().file(), offset, offset)
                    .map_err(|_| invalid_capture());
            }
            comment |= *byte == b'\''
                || suffix
                    .get(cursor..cursor + 3)
                    .is_some_and(|word| word.eq_ignore_ascii_case(b"Rem"))
                    && suffix
                        .get(cursor + 3)
                        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
            if !comment && matches!(byte, b' ' | b'\t') {
                let trivia = rootlight_vba::logical_whitespace_len(&suffix[cursor..], &mut || {
                    cancellation.check().map_err(AdapterError::from)
                })?;
                cursor += trivia;
            } else {
                cursor += 1;
            }
        }
        Err(invalid_capture())
    }
}

#[cfg(test)]
mod tests;
