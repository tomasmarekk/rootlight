//! Closed first-party procedural modules enter the existing syntax normalizer.
//! Unsupported source returns to the native whole-module path; resource failures
//! never trigger a second parser, and successful scans retain no native tree.

use std::ops::Range;

use rootlight_adapter_sdk::NativeVbaModuleKind;
use rootlight_vba::{
    ModuleBodyStatement, ModuleConstantVisibility, ModuleError, ModuleInventory, ModuleLimits,
    ModuleLocalKind, ModuleScalarValueKind, ModuleVariableVisibility, ProcedureKind,
};

use super::*;
use crate::VBA_PROCEDURAL_FRONTEND_VERSION;

pub(super) struct PreparedModule {
    inventory: ModuleInventory,
    family: GrammarFamily,
    native_family: GrammarFamily,
    processed_nodes: usize,
}

pub(super) fn prepare(
    provider: &TreeSitterProvider,
    request: &ParseRequest<'_>,
    previous: Option<&PreviousParse>,
    edits: &[SourceEdit],
    settings: ParserSettings,
    cancellation: &Cancellation,
) -> Result<Option<PreparedModule>, AdapterError> {
    cancellation.check()?;
    if !matches!(request.language().as_str(), "vba" | "visual-basic-classic")
        || request.encoding().as_str() != "utf-8"
        || request.native_vba_module() == Some(NativeVbaModuleKind::Object)
        || !request.included_ranges().is_empty()
    {
        return Ok(None);
    }
    let (family, native_family) =
        provider.admit_parse_input(request, previous, edits, settings, cancellation)?;
    let maximum_nodes = request.limits().max_syntax_nodes();
    let maximum_source = request
        .limits()
        .max_source_bytes()
        .min(provider.config.max_source_bytes());
    let limits = ModuleLimits::new(maximum_source, maximum_nodes, maximum_nodes)
        .with_max_parameters(maximum_nodes)
        .with_max_arguments(maximum_nodes)
        .with_max_expression_nodes(maximum_nodes);
    let inventory =
        match rootlight_vba::module_inventory(request.source().bytes(), limits, &mut || {
            cancellation.check()
        }) {
            Ok(inventory) => inventory,
            Err(ModuleError::Unsupported { .. }) => return Ok(None),
            Err(ModuleError::SourceLimit) => {
                return Err(RequestError::ProviderLimit {
                    resource: ResourceKind::SourceBytes,
                    observed: request.source().bytes().len(),
                    limit: maximum_source,
                }
                .into());
            }
            Err(
                ModuleError::StatementLimit
                | ModuleError::ProcedureLimit
                | ModuleError::ParameterLimit
                | ModuleError::ArgumentLimit
                | ModuleError::ExpressionLimit,
            ) => {
                return Err(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: maximum_nodes
                        .checked_add(1)
                        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?,
                    limit: maximum_nodes,
                }
                .into());
            }
            Err(ModuleError::AllocationFailed) => return Err(SinkError::AllocationFailed.into()),
            Err(ModuleError::Cancelled(cancelled)) => return Err(cancelled.into()),
        };
    let mut processed_nodes = inventory
        .statement_count()
        .checked_add(inventory.procedures().len())
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
    let mut previous_variable_statement = None;
    for variable in inventory.variables() {
        cancellation.check()?;
        let statement = variable.statement();
        // A comma group is one logical statement, but every sibling needs work.
        if previous_variable_statement.as_ref() == Some(&statement) {
            processed_nodes = processed_nodes
                .checked_add(1)
                .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
        }
        previous_variable_statement = Some(statement);
        processed_nodes = processed_nodes
            .checked_add(variable.array_work_nodes())
            .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
    }
    for procedure in inventory.procedures() {
        cancellation.check()?;
        processed_nodes = processed_nodes
            .checked_add(procedure.parameters().len())
            .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
        for parameter in procedure.parameters() {
            cancellation.check()?;
            if parameter.default_value().is_some() {
                processed_nodes = processed_nodes
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
            }
        }
        for statement in procedure.statements() {
            cancellation.check()?;
            let extra_nodes = match statement {
                ModuleBodyStatement::Variables(declaration) => {
                    // The logical segment already paid for its first declarator;
                    // siblings and authored array designators still require work.
                    let mut work = declaration
                        .variables()
                        .len()
                        .checked_sub(1)
                        .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
                    for variable in declaration.variables() {
                        cancellation.check()?;
                        work = work
                            .checked_add(variable.array_work_nodes())
                            .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
                    }
                    work
                }
                ModuleBodyStatement::Call(call) => call.argument_values().len(),
                // The assignment is already counted; its RHS application and
                // receiver require their own work in addition to argument atoms.
                ModuleBodyStatement::ApplicationAssignment(assignment) => assignment
                    .argument_values()
                    .len()
                    .checked_add(2)
                    .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?,
                ModuleBodyStatement::ExpressionAssignment(assignment) => {
                    assignment.inventory().work_nodes()
                }
                ModuleBodyStatement::ExpressionCall(call) => call.inventory().work_nodes(),
                ModuleBodyStatement::Declaration(_) | ModuleBodyStatement::Assignment(_) => 0,
            };
            processed_nodes = processed_nodes
                .checked_add(extra_nodes)
                .ok_or_else(|| provider_failure("vba-procedural-node-accounting"))?;
        }
    }
    require_provider_limit(ResourceKind::SyntaxNodes, processed_nodes, maximum_nodes)?;
    cancellation.check()?;
    Ok(Some(PreparedModule {
        inventory,
        family,
        native_family,
        processed_nodes,
    }))
}

pub(super) fn required_facts(
    prepared: PreparedModule,
    request: &ParseRequest<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let candidates = candidates(
        &prepared.inventory,
        request.source().bytes(),
        false,
        request.limits().max_syntax_nodes(),
        cancellation,
    )?;
    let normalized = normalize_query_candidates(candidates, request, usize::MAX, cancellation)?;
    validate_depth(&normalized, request, cancellation)?;
    Ok(normalized.required)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit(
    prepared: PreparedModule,
    request: &ParseRequest<'_>,
    previous: Option<&PreviousParse>,
    edits: &[SourceEdit],
    settings: ParserSettings,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
) -> Result<RawParseWithPrevious, AdapterError> {
    let candidates = candidates(
        &prepared.inventory,
        request.source().bytes(),
        true,
        request.limits().max_syntax_nodes(),
        cancellation,
    )?;
    let normalized = normalize_query_candidates(
        candidates,
        request,
        pre_extraction_fact_limit(sink.remaining_budget())?,
        cancellation,
    )?;
    let maximum_depth = validate_depth(&normalized, request, cancellation)?;
    if normalized.limited {
        emit_extraction_limit_diagnostic(request, sink, cancellation)?;
    }
    let plan = plan_fact_batches(&normalized.facts, sink.remaining_budget(), cancellation)?;
    // The closed route knows every retained fact before publication. Exact
    // byte/batch clipping must not discard a later declaration's identity.
    if plan.emitted < normalized.facts.len() {
        return Err(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed: normalized.facts.len(),
            limit: plan.emitted,
        }
        .into());
    }
    let limited = normalized.limited;
    emit_fact_plan(normalized.facts, plan, sink, cancellation)?;
    let source_len = request.source().bytes().len();
    let covered = if request.included_ranges().is_empty() {
        source_len
    } else {
        included_range_source_bytes(request.included_ranges(), cancellation)?
    };
    let omitted = covered < source_len;
    let usage = sink.staged_usage();
    let coverage = CoverageReport::new(
        AnalysisTier::TierD,
        if limited || omitted {
            CoverageStatus::Bounded
        } else {
            CoverageStatus::Complete
        },
        source_len,
        covered,
        usize::from(limited)
            .checked_add(usize::from(omitted))
            .ok_or_else(|| provider_failure("coverage-accounting"))?,
        Vec::new(),
    )
    .map_err(AdapterError::InvalidReport)?;
    let report = WorkReport::new(
        coverage,
        ResourceUsage::new(
            source_len,
            usage.records(),
            prepared.processed_nodes,
            maximum_depth,
            None,
            usage,
        ),
        StreamEnd::new(sink.next_sequence(), usage),
    )
    .map_err(AdapterError::InvalidReport)?;
    cancellation.check()?;
    Ok(RawParseWithPrevious {
        report,
        pending: None,
        reuse_status: if previous.is_some() {
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        } else {
            ReuseStatus::Fresh
        },
        reuse_key: ParseReuseKey {
            previous_content_hash: None,
            current_content_hash: request.source().source_ref().content_hash(),
            family: prepared.family,
            native_family: prepared.native_family,
            grammar_version: VBA_PROCEDURAL_FRONTEND_VERSION,
            encoding: request.encoding().as_str().to_owned(),
            included_ranges: request.shared_included_ranges(),
            settings,
            edits: source_edit_identities(edits, cancellation)?,
        },
    })
}

fn validate_depth(
    normalized: &NormalizedFacts,
    request: &ParseRequest<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut maximum = 0;
    for (index, fact) in normalized.facts.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        maximum = maximum.max(fact.depth());
    }
    require_provider_limit(
        ResourceKind::SyntaxDepth,
        maximum,
        request.limits().max_syntax_depth(),
    )?;
    cancellation.check()?;
    Ok(maximum)
}

fn candidates(
    inventory: &ModuleInventory,
    source: &[u8],
    include_optional: bool,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<QueryCandidate>, AdapterError> {
    if inventory.source_len() != source.len() {
        return Err(provider_failure("vba-procedural-source-identity"));
    }
    let mut count = inventory
        .procedures()
        .len()
        .checked_mul(4)
        .and_then(|count| count.checked_add(3 + usize::from(inventory.module_name().is_some())))
        .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
    count = inventory
        .constants()
        .len()
        .checked_mul(2)
        .and_then(|constants| count.checked_add(constants))
        .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
    count = inventory
        .variables()
        .len()
        .checked_mul(2)
        .and_then(|variables| count.checked_add(variables))
        .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
    for procedure in inventory.procedures() {
        cancellation.check()?;
        count = procedure
            .parameters()
            .len()
            .checked_mul(2)
            .and_then(|parameters| count.checked_add(parameters))
            .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
        for statement in procedure.statements() {
            cancellation.check()?;
            let declarations = match statement {
                ModuleBodyStatement::Declaration(_) => 1,
                ModuleBodyStatement::Variables(declaration) => declaration.variables().len(),
                ModuleBodyStatement::Assignment(_)
                | ModuleBodyStatement::ApplicationAssignment(_)
                | ModuleBodyStatement::ExpressionAssignment(_)
                | ModuleBodyStatement::Call(_)
                | ModuleBodyStatement::ExpressionCall(_) => 0,
            };
            count = declarations
                .checked_mul(2)
                .and_then(|declarations| count.checked_add(declarations))
                .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?;
        }
    }
    require_provider_limit(ResourceKind::SyntaxNodes, count, maximum)?;
    let mut output = Vec::new();
    try_reserve_exact_cancellable(
        &mut output,
        count,
        cancellation,
        "vba-procedural-fact-allocation",
    )?;
    for role in [
        StructuralRole::Root,
        StructuralRole::Module,
        StructuralRole::Scope,
    ] {
        output.push(candidate(
            source_root_start(source, cancellation)?..source.len(),
            role,
            "vba.file",
            role != StructuralRole::Scope,
        ));
    }
    if let Some(name) = inventory.module_name() {
        let start = name
            .start
            .checked_sub(1)
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        let end = name
            .end
            .checked_add(1)
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        let text = source
            .get(start..end)
            .and_then(|text| std::str::from_utf8(text).ok())
            .ok_or_else(|| provider_failure("vba-procedural-module-name"))?;
        if rootlight_adapter_sdk::vba_module_name(text).is_none() {
            return Err(provider_failure("vba-procedural-module-name"));
        }
        output.push(candidate(
            start..end,
            StructuralRole::Definition,
            "vba.module_name",
            true,
        ));
    }
    for constant in inventory.constants() {
        cancellation.check()?;
        push_body_candidate(
            &mut output,
            maximum,
            cancellation,
            constant.declarator(),
            StructuralRole::Declaration,
            "vba.constant",
            true,
        )?;
        push_body_candidate(
            &mut output,
            maximum,
            cancellation,
            constant.name(),
            StructuralRole::Definition,
            "vba.identifier",
            true,
        )?;
        if include_optional {
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                constant.declarator(),
                StructuralRole::Expression,
                match constant.visibility() {
                    Some(ModuleConstantVisibility::Public) => "vba.public",
                    Some(ModuleConstantVisibility::Private) => "vba.private",
                    None => "vba.visibility_unknown",
                },
                false,
            )?;
        }
    }
    for variable in inventory.variables() {
        cancellation.check()?;
        push_body_candidate(
            &mut output,
            maximum,
            cancellation,
            variable.declarator(),
            StructuralRole::Declaration,
            "vba.variable",
            true,
        )?;
        push_body_candidate(
            &mut output,
            maximum,
            cancellation,
            variable.name(),
            StructuralRole::Definition,
            "vba.identifier",
            true,
        )?;
        if include_optional {
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                variable.declarator(),
                StructuralRole::Expression,
                match variable.visibility() {
                    Some(ModuleVariableVisibility::Public) => "vba.public",
                    Some(ModuleVariableVisibility::Private) => "vba.private",
                    None => "vba.visibility_unknown",
                },
                false,
            )?;
        }
    }
    for procedure in inventory.procedures() {
        cancellation.check()?;
        let syntax = match procedure.kind() {
            ProcedureKind::Sub => "vba.sub_function",
            ProcedureKind::Function => "vba.function",
            _ => return Err(provider_failure("vba-procedural-kind")),
        };
        let header = native_header(
            source,
            procedure.header(),
            procedure.body(),
            procedure.declaration(),
            procedure
                .statements()
                .first()
                .map(|statement| statement.statement().start),
            cancellation,
        )?;
        output.push(candidate(
            procedure.declaration(),
            StructuralRole::Declaration,
            syntax,
            true,
        ));
        output.push(candidate(
            procedure.declaration(),
            StructuralRole::Scope,
            syntax,
            false,
        ));
        output.push(candidate(
            header.clone(),
            StructuralRole::Signature,
            syntax,
            true,
        ));
        output.push(candidate(
            procedure.name(),
            StructuralRole::Definition,
            "vba.identifier",
            true,
        ));
        for parameter in procedure.parameters() {
            cancellation.check()?;
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                parameter.declaration(),
                StructuralRole::Declaration,
                if parameter.is_optional() {
                    "vba.omittable_parameter"
                } else {
                    "vba.parameter"
                },
                true,
            )?;
            push_body_candidate(
                &mut output,
                maximum,
                cancellation,
                parameter.name(),
                StructuralRole::Definition,
                "vba.identifier",
                true,
            )?;
            if include_optional
                && let Some(value) = parameter.default_value()
                && value.kind() == ModuleScalarValueKind::Name
            {
                push_body_candidate(
                    &mut output,
                    maximum,
                    cancellation,
                    value.span(),
                    StructuralRole::Reference,
                    "vba.default_constant_identifier",
                    false,
                )?;
            }
        }
        if include_optional {
            let text = source
                .get(procedure.header())
                .ok_or_else(|| provider_failure("vba-procedural-header"))?;
            let first = text
                .split(|byte| byte.is_ascii_whitespace())
                .next()
                .unwrap_or_default();
            let visibility = if first.eq_ignore_ascii_case(b"Public") {
                "vba.public"
            } else if first.eq_ignore_ascii_case(b"Private") {
                "vba.private"
            } else if first.eq_ignore_ascii_case(b"Friend") {
                "vba.project_restricted"
            } else {
                "vba.visibility_unknown"
            };
            require_provider_limit(
                ResourceKind::SyntaxNodes,
                output
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
                maximum,
            )?;
            try_reserve_cancellable(
                &mut output,
                1,
                cancellation,
                "vba-procedural-fact-allocation",
            )?;
            output.push(candidate(
                header,
                StructuralRole::Expression,
                visibility,
                false,
            ));
        }
        for statement in procedure.statements() {
            body_candidates(
                statement,
                &mut output,
                include_optional,
                maximum,
                cancellation,
            )?;
        }
    }
    if include_optional {
        lexical_trivia(source, &mut output, maximum, cancellation)?;
    }
    cancellation.check()?;
    Ok(output)
}

fn body_candidates(
    statement: &ModuleBodyStatement,
    output: &mut Vec<QueryCandidate>,
    include_optional: bool,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    match statement {
        ModuleBodyStatement::Variables(declaration) => {
            for variable in declaration.variables() {
                cancellation.check()?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    variable.declarator(),
                    StructuralRole::Declaration,
                    "vba.variable",
                    true,
                )?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    variable.name(),
                    StructuralRole::Definition,
                    "vba.identifier",
                    true,
                )?;
                if include_optional {
                    push_body_candidate(
                        output,
                        maximum,
                        cancellation,
                        variable.declarator(),
                        StructuralRole::Expression,
                        "vba.visibility_unknown",
                        false,
                    )?;
                }
            }
        }
        ModuleBodyStatement::Declaration(declaration) => {
            let syntax = match declaration.kind() {
                ModuleLocalKind::Variable => "vba.variable",
                ModuleLocalKind::Constant => "vba.constant",
            };
            push_body_candidate(
                output,
                maximum,
                cancellation,
                declaration.declarator(),
                StructuralRole::Declaration,
                syntax,
                true,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                declaration.name(),
                StructuralRole::Definition,
                "vba.identifier",
                true,
            )?;
            if include_optional {
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    declaration.declarator(),
                    StructuralRole::Expression,
                    "vba.visibility_unknown",
                    false,
                )?;
            }
        }
        ModuleBodyStatement::Assignment(assignment) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.destination(),
                StructuralRole::Reference,
                "vba.let_identifier",
                false,
            )?;
            match assignment.value().kind() {
                ModuleScalarValueKind::Name => push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    assignment.value().span(),
                    StructuralRole::Reference,
                    "vba.value_identifier",
                    false,
                )?,
                // Numeric and Boolean nodes have no captures in the current query
                // pack; quoted strings use the shared lexical-trivia scan below.
                ModuleScalarValueKind::UnsignedDecimal
                | ModuleScalarValueKind::UnsignedHexadecimal
                | ModuleScalarValueKind::UnsignedOctal
                | ModuleScalarValueKind::UnsignedFloat
                | ModuleScalarValueKind::String
                | ModuleScalarValueKind::Boolean => {}
            }
        }
        ModuleBodyStatement::ApplicationAssignment(assignment) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.destination(),
                StructuralRole::Reference,
                "vba.let_identifier",
                false,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.application(),
                StructuralRole::Reference,
                "vba.application",
                false,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.callee(),
                StructuralRole::Reference,
                "vba.invoked_get_identifier",
                false,
            )?;
            for argument in assignment.argument_values() {
                cancellation.check()?;
                match argument.kind() {
                    ModuleScalarValueKind::Name => push_body_candidate(
                        output,
                        maximum,
                        cancellation,
                        argument.span(),
                        StructuralRole::Reference,
                        "vba.value_identifier",
                        false,
                    )?,
                    ModuleScalarValueKind::UnsignedDecimal
                    | ModuleScalarValueKind::UnsignedHexadecimal
                    | ModuleScalarValueKind::UnsignedOctal
                    | ModuleScalarValueKind::UnsignedFloat
                    | ModuleScalarValueKind::String
                    | ModuleScalarValueKind::Boolean => {}
                }
            }
        }
        ModuleBodyStatement::ExpressionAssignment(assignment) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                assignment.destination(),
                StructuralRole::Reference,
                "vba.let_identifier",
                false,
            )?;
            for application in assignment.inventory().applications() {
                cancellation.check()?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    application.application(),
                    StructuralRole::Reference,
                    "vba.application",
                    false,
                )?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    application.callee(),
                    StructuralRole::Reference,
                    "vba.invoked_get_identifier",
                    false,
                )?;
            }
            for value in assignment.inventory().values() {
                cancellation.check()?;
                if value.kind() == ModuleScalarValueKind::Name {
                    push_body_candidate(
                        output,
                        maximum,
                        cancellation,
                        value.span(),
                        StructuralRole::Reference,
                        "vba.value_identifier",
                        false,
                    )?;
                }
            }
        }
        ModuleBodyStatement::ExpressionCall(call) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.statement(),
                StructuralRole::Reference,
                "vba.application",
                false,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.callee(),
                StructuralRole::Reference,
                "vba.invoked_identifier",
                false,
            )?;
            // The leaf retains its proven root application for argument evidence;
            // explicit Call already emitted that root with its complete statement.
            for application in call.inventory().applications().iter().skip(1) {
                cancellation.check()?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    application.application(),
                    StructuralRole::Reference,
                    "vba.application",
                    false,
                )?;
                push_body_candidate(
                    output,
                    maximum,
                    cancellation,
                    application.callee(),
                    StructuralRole::Reference,
                    "vba.invoked_get_identifier",
                    false,
                )?;
            }
            for value in call.inventory().values() {
                cancellation.check()?;
                if value.kind() == ModuleScalarValueKind::Name {
                    push_body_candidate(
                        output,
                        maximum,
                        cancellation,
                        value.span(),
                        StructuralRole::Reference,
                        "vba.value_identifier",
                        false,
                    )?;
                }
            }
        }
        ModuleBodyStatement::Call(call) if include_optional => {
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.statement(),
                StructuralRole::Reference,
                "vba.application",
                false,
            )?;
            push_body_candidate(
                output,
                maximum,
                cancellation,
                call.callee(),
                StructuralRole::Reference,
                "vba.invoked_identifier",
                false,
            )?;
            for argument in call.argument_values() {
                cancellation.check()?;
                match argument.kind() {
                    ModuleScalarValueKind::Name => push_body_candidate(
                        output,
                        maximum,
                        cancellation,
                        argument.span(),
                        StructuralRole::Reference,
                        "vba.value_identifier",
                        false,
                    )?,
                    ModuleScalarValueKind::UnsignedDecimal
                    | ModuleScalarValueKind::UnsignedHexadecimal
                    | ModuleScalarValueKind::UnsignedOctal
                    | ModuleScalarValueKind::UnsignedFloat
                    | ModuleScalarValueKind::String
                    | ModuleScalarValueKind::Boolean => {}
                }
            }
        }
        ModuleBodyStatement::Assignment(_)
        | ModuleBodyStatement::ApplicationAssignment(_)
        | ModuleBodyStatement::ExpressionAssignment(_)
        | ModuleBodyStatement::ExpressionCall(_)
        | ModuleBodyStatement::Call(_) => {}
    }
    Ok(())
}

fn push_body_candidate(
    output: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
    range: Range<usize>,
    role: StructuralRole,
    syntax: &'static str,
    required: bool,
) -> Result<(), AdapterError> {
    require_provider_limit(
        ResourceKind::SyntaxNodes,
        output
            .len()
            .checked_add(1)
            .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
        maximum,
    )?;
    try_reserve_cancellable(output, 1, cancellation, "vba-procedural-fact-allocation")?;
    output.push(candidate(range, role, syntax, required));
    Ok(())
}

fn candidate(
    range: Range<usize>,
    role: StructuralRole,
    syntax: &'static str,
    required: bool,
) -> QueryCandidate {
    QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required,
        native_depth: 0,
    }
}

fn source_root_start(source: &[u8], cancellation: &Cancellation) -> Result<usize, AdapterError> {
    // Native extras before the first source-file item do not extend its span.
    // A newline is a real item and must not be trimmed as leading whitespace.
    for (offset, byte) in source.iter().enumerate() {
        if offset.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if !matches!(byte, b' ' | b'\t' | b'\x0c') {
            return Ok(offset);
        }
    }
    cancellation.check()?;
    Ok(source.len())
}

fn native_header(
    source: &[u8],
    header: Range<usize>,
    body: Range<usize>,
    declaration: Range<usize>,
    first_statement: Option<usize>,
    cancellation: &Cancellation,
) -> Result<Range<usize>, AdapterError> {
    let suffix = source
        .get(header.end..body.start)
        .ok_or_else(|| provider_failure("vba-procedural-header"))?;
    let mut end = declaration.end;
    for (offset, byte) in suffix.iter().enumerate() {
        cancellation.check()?;
        if *byte == b'\n' {
            end = header.end + offset;
            break;
        }
    }
    if end == declaration.end {
        let boundary = first_statement.unwrap_or(body.end);
        if first_statement.is_some() {
            end = boundary;
        }
        let body_bytes = source
            .get(body.start..boundary)
            .ok_or_else(|| provider_failure("vba-procedural-header"))?;
        let mut comment = false;
        for (offset, byte) in body_bytes.iter().enumerate() {
            cancellation.check()?;
            if *byte == b'\n' || (*byte == b':' && !comment) {
                end = body.start + offset;
                break;
            }
            comment |= *byte == b'\'';
        }
    }
    while end > declaration.start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
        cancellation.check()?;
        end -= 1;
    }
    Ok(declaration.start..end)
}

fn lexical_trivia(
    source: &[u8],
    output: &mut Vec<QueryCandidate>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut cursor = 0;
    while let Some(byte) = source.get(cursor) {
        cancellation.check()?;
        let start = cursor;
        let quoted = *byte == b'"';
        if quoted {
            cursor += 1;
            loop {
                cancellation.check()?;
                let Some(next) = source.get(cursor) else {
                    return Err(provider_failure("vba-procedural-string"));
                };
                cursor += 1;
                if *next == b'"' {
                    if source.get(cursor) != Some(&b'"') {
                        break;
                    }
                    cursor += 1;
                }
            }
        } else if *byte == b'[' {
            cursor += 1;
            while source.get(cursor).is_some_and(|byte| *byte != b']') {
                cancellation.check()?;
                cursor += 1;
            }
            if source.get(cursor) != Some(&b']') {
                return Err(provider_failure("vba-procedural-bracket"));
            }
            cursor += 1;
            continue;
        } else if *byte == b'\'' {
            while source.get(cursor).is_some_and(|byte| *byte != b'\n') {
                cancellation.check()?;
                cursor += 1;
            }
        } else if byte.is_ascii_alphabetic() || *byte == b'_' {
            while source
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                cancellation.check()?;
                cursor += 1;
            }
            if source
                .get(cursor)
                .is_some_and(|byte| b"%&^!#@$".contains(byte))
            {
                cursor += 1;
                continue;
            }
            if !source
                .get(start..cursor)
                .is_some_and(|word| word.eq_ignore_ascii_case(b"Rem"))
            {
                continue;
            }
            // Native Rem is a bare three-byte extra unless a space/tab starts
            // its line tail. Header delimiters and form feeds are not that tail.
            if matches!(source.get(cursor), Some(b' ' | b'\t')) {
                while source.get(cursor).is_some_and(|byte| *byte != b'\n') {
                    cancellation.check()?;
                    cursor += 1;
                }
            }
        } else if byte.is_ascii_digit() {
            while source.get(cursor).is_some_and(u8::is_ascii_digit) {
                cancellation.check()?;
                cursor += 1;
            }
            continue;
        } else {
            cursor += 1;
            continue;
        }
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            output
                .len()
                .checked_add(1)
                .ok_or_else(|| provider_failure("vba-procedural-fact-accounting"))?,
            maximum,
        )?;
        try_reserve_cancellable(output, 1, cancellation, "vba-procedural-fact-allocation")?;
        output.push(candidate(
            start..cursor,
            if quoted {
                StructuralRole::StringLiteral
            } else {
                StructuralRole::Comment
            },
            if quoted { "vba.string" } else { "vba.comment" },
            false,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{
        AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, StreamLimits,
        SyntaxFactKind,
    };
    use rootlight_ir::{BuildContextIdentity, IrLimits};
    use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};

    struct Fixture {
        _directory: tempfile::TempDir,
        snapshot: SourceSnapshot,
        source: SourceRef,
        limits: AnalysisLimits,
        provider: TreeSitterProvider,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
            std::fs::write(directory.path().join("module.bas"), text).unwrap();
            let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
            let repository = RepositoryRoot::open(repository_id, directory.path()).unwrap();
            let relative = RelativePath::parse(std::path::Path::new("module.bas")).unwrap();
            let snapshot = repository.snapshot(&relative, 1024 * 1024).unwrap();
            let source = SourceRef::new(
                repository_id,
                "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                    .parse()
                    .unwrap(),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
            let stream =
                StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch)
                    .unwrap();
            let limits = AnalysisLimits::new(
                1024 * 1024,
                100_000,
                64,
                16,
                8 * 1024 * 1024,
                stream.clone(),
                stream,
                IrLimits::default(),
            )
            .unwrap();
            let config = RuntimeConfig::new(
                1024 * 1024,
                100_000,
                64,
                16,
                16,
                1,
                2 * 1024 * 1024,
                ParserSettings::new(1024).unwrap(),
            )
            .unwrap();
            Self {
                _directory: directory,
                snapshot,
                source,
                limits,
                provider: TreeSitterProvider::new(config).unwrap(),
            }
        }

        fn request(&self, language: &str, ranges: &[(u64, u64)]) -> ParseRequest<'_> {
            let ranges = ranges
                .iter()
                .map(|&(start, end)| {
                    IncludedRange::new(
                        SourceSpan::new(self.snapshot.file(), start, end).unwrap(),
                        LanguageId::new(language).unwrap(),
                    )
                })
                .collect();
            ParseRequest::new(
                GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
                LanguageId::new(language).unwrap(),
                EncodingId::utf8(),
                ranges,
                &self.limits,
            )
            .unwrap()
        }
    }

    fn compare_native(fixture: &Fixture, language: &str, ranges: &[(u64, u64)]) {
        let cancellation = Cancellation::new();
        let request = fixture.request(language, ranges);
        let prepared = prepare(
            &fixture.provider,
            &request,
            None,
            &[],
            fixture.provider.config.default_settings(),
            &cancellation,
        )
        .unwrap()
        .unwrap_or_else(|| {
            let source = request.source().bytes();
            let prefix = source.get(..source.len().min(1024)).unwrap();
            panic!(
                "the authored complete procedural module is supported: language={language:?}, source_bytes={}, source_prefix={:?}",
                source.len(),
                String::from_utf8_lossy(prefix),
            );
        });
        let identity = normalize_query_candidates(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                false,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        let actual = normalize_query_candidates(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        assert_eq!(fixture.provider.stats().cache.entries, 0);
        // This private legacy path bypasses routing without a public test flag.
        let native = fixture
            .provider
            .prepare_tree(
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap();
        assert!(native.traversal.fully_traversed);
        assert_eq!(
            native.traversal.coverage,
            if ranges.is_empty() {
                CoverageStatus::Complete
            } else {
                CoverageStatus::Bounded
            },
            "the closed authored module has no native recovery: {:?}",
            request.source().bytes()
        );
        let pack = fixture
            .provider
            .query_packs
            .get_for_source(native.family, request.source().path().as_str())
            .unwrap();
        let legacy = pack
            .extract(
                native.family,
                &native.tree,
                request.source().bytes(),
                request.limits().max_syntax_nodes(),
                usize::MAX,
                &cancellation,
            )
            .unwrap();
        assert!(legacy.limit.is_none());
        let expected =
            normalize_query_candidates(legacy.candidates, &request, usize::MAX, &cancellation)
                .unwrap();
        let expected_identity = normalize_query_candidates(
            pack.extract_identity(
                native.family,
                &native.tree,
                request.source().bytes(),
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap(),
            &request,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(
            identity.required,
            expected_identity.required,
            "identity demand for {language}, {:?}; native tree: {}; native identity: {:?}",
            String::from_utf8_lossy(request.source().bytes()),
            native.tree.root_node().to_sexp(),
            expected_identity.facts,
        );
        assert_eq!(
            actual.required, expected.required,
            "full demand for {language}"
        );
        assert_eq!(
            actual.facts,
            expected.facts,
            "exact source facts for {language}, {:?}",
            request.source().bytes()
        );
    }

    #[test]
    fn first_party_vba_module_facts_match_the_private_native_route() {
        let cases = [
            "",
            " ",
            " \t\x0c",
            " \t\x0cSub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub",
            "Sub Run()\nEnd Sub \t\x0c",
            "Sub Run()\nEnd Sub\n \t\x0c",
            "\n' leading\nRem ordinary comment\n\n",
            "' ordinary comment:",
            "Rem ordinary comment:",
            "Sub Run()\n' body:\nEnd Sub\n' trailing:",
            "Attribute VB_Name = \"ModuleA\"\nOption Explicit\nPublic Sub Run()\nEnd Sub\n",
            "  ' leading\r\nAttribute VB_Name = \"ModuleA\"\r\nOption Compare Text\r\nPrivate Static Sub [Run]() ' header\r\n' body\r\nEnd Sub\r\n",
            "Function Result$()\nEnd Function\nPublic Function Count() As Long\nEnd Function\n",
            "Sub First(): End Sub\nSub Second(): : End Sub\n",
            "Sub Same()\nEnd Sub\nSub Same()\nEnd Sub\n",
            "Public _\r\nSub Run( _\r\n)\r\nEnd Sub\r\n",
            "Sub Run() ' header\r\nEnd Sub ' terminator\r\n",
            "Sub Run(): ' body\r\nEnd Sub ' terminator\r\n",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for keyword in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let source = format!("Function Result() As {keyword}\nEnd Function\n");
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_bodies_match_the_private_native_route() {
        let cases = [
            "Sub Run()\nDim value As Long\nConst seed As Long = 65\nvalue = seed\nLet value = 7\nEnd Sub\n",
            "Sub Run()\nDim [value] As String\nConst [text] As String = \"a\"\"b\"\n[value] = [text]\nLet [value] = \"replacement\"\nEnd Sub\n",
            "Sub Run()\nDim flag As Boolean\nConst enabled As Boolean = True\nflag = enabled\nLet flag = False\nEnd Sub\n",
            "Sub Run()\nDim [Me] As Boolean\nConst [True] As Boolean = False\n[Me] = [True]\nDim [Rem] As Long\nConst [Sub] As Long = 1\n[Rem] = [Sub]\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\"\r\nOption Explicit\r\nPrivate Static Function Result() As Long\r\nDim count As Long ' declaration\r\nConst seed As Long = 7 ' constant\r\nLet count = seed ' assignment\r\nResult = count\r\nEnd Function ' terminator\r\n",
            "Sub Run():Dim value As Long:Const seed As Long = 7:value = seed:Let value = 8:End Sub\n",
            "Sub Run(): : Dim value As Long\n' body:\nLet value = 1\nEnd Sub\n",
            "Sub Run(): ' header\r\nDim value As Long: ' declaration\r\nLet value = 1\r\nEnd Sub\r\n",
            "Sub Run()\nDim _\r\n [value] As _\n Long\nConst [seed] As _\r\n Long = _\n 7\nLet _\r\n [value] = _\n [seed]\nEnd Sub\n",
            "Function Result() As String\nDim value As String\nConst seed As String = \"\"\nvalue = \"text\"\nResult = value\nEnd Function\n",
            "Sub Run()\nDim value As Long\nConst seed As Long = 001\nvalue = 000\nEnd Sub",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for keyword in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let source = format!(
                "Sub Run()\nDim value As {keyword}\nConst seed As {keyword} = 1\nvalue = seed\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_intrinsic_parameters_match_the_private_native_route() {
        for passing in ["", "ByVal ", "ByRef "] {
            for intrinsic in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                for (kind, body, result) in [
                    ("Sub", "value = value", ""),
                    ("Function", "Result = value", " As Long"),
                ] {
                    let source = format!(
                        "{kind} Result({passing}value As {intrinsic}){result}\n{body}\nEnd {kind}\n"
                    );
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for source in [
            "Sub Run(ByVal [Me] As Long):[Me] = [Me]:End Sub\n",
            "Sub Run(ByRef [Rem] As Long) ' header\r\n[Rem] = [Rem] ' body\r\nEnd Sub ' terminator\r\n",
            "Public _\r\nFunction Result( _\n bYvAl _\r\n [value] _\n As _\r\n Long _\n ) As Long\r\nResult = [value]\r\nEnd Function\r\n",
            "Sub First(ByVal value As Long)\nvalue = value\nEnd Sub\nSub Second(ByRef value As Long)\nvalue = value\nEnd Sub\n",
            "Sub Accept(ByVal value As Long)\nEnd Sub\nSub Run()\nCall Accept()\nEnd Sub\n",
            "Sub Run(ByVal first As Long, ByRef second As String)\nfirst = first\nsecond = second\nEnd Sub\n",
            "Sub Run(value As Long, value As Long)\nvalue = value\nEnd Sub\n",
            "Function Result(first As Long, ByVal second As String, ByRef third As Boolean) As Long\nResult = first\nEnd Function\n",
            "Sub Run( _\r\n ByVal first As Long, _\n ByRef [second] As String, _\r\n third As Object _\n )\nfirst = first\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_omitted_types_match_the_private_native_route() {
        for passing in ["", "ByVal ", "ByRef "] {
            for parameters in [
                format!("{passing}value"),
                format!("Optional {passing}value"),
                format!("Optional {passing}value = 007"),
                format!("Optional {passing}[value] = \"a,\"\"b\"\":()\""),
                format!("Optional {passing}value = False"),
                format!(
                    "{passing}value, typed As Long, Optional unset, Optional text As String = \"x\""
                ),
            ] {
                for category in ["Sub", "Function"] {
                    let source = format!("{category} Run({parameters})\nEnd {category}\n");
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for source in [
            "Sub Run(first, ByVal second, ByRef third, Optional count = 1, Optional flag As Boolean = True)\nEnd Sub\n",
            "Sub Run(value, value, Optional value, Optional value = 1)\nEnd Sub\n",
            "Sub Run([Optional], ByVal [ByRef], Optional [As] = True, Optional [Rem])\nEnd Sub\n",
            "Sub Run(Optional first = 1, second, Optional third)\nEnd Sub\n",
            "Public Sub Run(ByVal first, _\r\nByRef second As Long, Optional [text] = \"x\", Optional unset) ' header\r\nDim value As Long\r\nvalue = first\r\nCall Target(first, second, [text])\r\nEnd Sub ' done\r\n",
            "Sub Run(first, Optional second = 1):first = second:End Sub\n",
            "Function Echo(ByRef value, Optional count = 1) As Long\nEcho = value\nEnd Function\n",
            "Sub Before(value)\nEnd Sub\nSub After(Optional value = \"x\")\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        let source = "Sub Run(ByVal untyped, typed As Long, Optional unset, Optional text = \"x\")\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &Cancellation::new(),
            )
            .unwrap()
            .unwrap();
            let parameters = prepared.inventory.procedures()[0].parameters();
            assert_eq!(parameters.len(), 4);
            for (parameter, ty) in parameters.iter().zip([None, Some("Long"), None, None]) {
                assert_eq!(
                    parameter
                        .type_range()
                        .map(|range| &source.as_bytes()[range]),
                    ty.map(str::as_bytes)
                );
            }
            let all = candidates(
                &prepared.inventory,
                source.as_bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &Cancellation::new(),
            )
            .unwrap();
            assert!(
                all.iter()
                    .all(|candidate| candidate.role != StructuralRole::Reference)
            );
            assert!(
                all.iter()
                    .all(|candidate| candidate.syntax != "vba.variant")
            );
        }
    }

    #[test]
    fn first_party_vba_omitted_type_name_positions_keep_native_admission() {
        let mut unexpected = Vec::new();
        let mut missing = Vec::new();
        let mut complete = 0;
        let mut unknown = 0;
        for name in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
            "Optional",
            "ByVal",
            "ByRef",
            "ParamArray",
            "As",
            "Rem",
        ] {
            for passing in ["", "ByVal ", "ByRef "] {
                for clause in [
                    format!("{passing}{name}"),
                    format!("{passing}[{name}]"),
                    format!("Optional {passing}{name}"),
                    format!("Optional {passing}[{name}]"),
                    format!("Optional {passing}{name} = 1"),
                    format!("Optional {passing}[{name}] = \"x\""),
                ] {
                    let source = format!("Sub Run({clause})\nEnd Sub\n");
                    for language in ["vba", "visual-basic-classic"] {
                        let fixture = Fixture::new(&source);
                        let request = fixture.request(language, &[]);
                        let cancellation = Cancellation::new();
                        let native = fixture
                            .provider
                            .prepare_tree(
                                &request,
                                None,
                                &[],
                                fixture.provider.config.default_settings(),
                                &cancellation,
                            )
                            .unwrap();
                        assert!(native.traversal.fully_traversed);
                        let admitted = prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap()
                        .is_some();
                        if native.traversal.coverage == CoverageStatus::Unknown {
                            unknown += 1;
                            if admitted {
                                unexpected.push(format!("{language}:{clause}"));
                            }
                        } else {
                            assert_eq!(native.traversal.coverage, CoverageStatus::Complete);
                            complete += 1;
                            if admitted {
                                compare_native(&Fixture::new(&source), language, &[]);
                            } else {
                                missing.push(format!("{language}:{clause}"));
                            }
                        }
                    }
                }
            }
        }
        assert_eq!((complete, unknown), (996, 48));
        assert!(
            unexpected.is_empty(),
            "native-recovered omitted-type names keep whole-module fallback: {unexpected:?}"
        );
        assert!(
            missing.is_empty(),
            "native-complete omitted-type names belong to the written slice: {missing:?}"
        );
    }

    #[test]
    fn first_party_vba_rem_parameter_boundaries_keep_the_native_contract() {
        let mut complete = 0;
        let mut unknown = 0;
        let mut empty = 0;
        for spelling in ["Rem", "rEm", "[Rem]"] {
            for prefix in [
                "",
                "ByVal ",
                "ByRef ",
                "Optional ",
                "Optional ByVal ",
                "Optional ByRef ",
            ] {
                for tail in [
                    "", ", value", "=1", " = 1", " ", "\t", "\x0c", " _\r\n ", " As Long",
                ] {
                    let clause = format!("{prefix}{spelling}{tail}");
                    let source = format!("Sub Run({clause})\nEnd Sub\n");
                    for language in ["vba", "visual-basic-classic"] {
                        let fixture = Fixture::new(&source);
                        let request = fixture.request(language, &[]);
                        let cancellation = Cancellation::new();
                        let native = fixture
                            .provider
                            .prepare_tree(
                                &request,
                                None,
                                &[],
                                fixture.provider.config.default_settings(),
                                &cancellation,
                            )
                            .unwrap();
                        assert!(native.traversal.fully_traversed);
                        assert!(matches!(
                            native.traversal.coverage,
                            CoverageStatus::Complete | CoverageStatus::Unknown
                        ));
                        let pack = fixture
                            .provider
                            .query_packs
                            .get_for_source(native.family, request.source().path().as_str())
                            .unwrap();
                        let extracted = pack
                            .extract(
                                native.family,
                                &native.tree,
                                request.source().bytes(),
                                request.limits().max_syntax_nodes(),
                                usize::MAX,
                                &cancellation,
                            )
                            .unwrap();
                        assert!(extracted.limit.is_none());
                        let normalized = normalize_query_candidates(
                            extracted.candidates,
                            &request,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        assert!(!normalized.limited);
                        let mut nodes = vec![native.tree.root_node()];
                        let mut parameters = Vec::new();
                        while let Some(node) = nodes.pop() {
                            if node.kind() == "parameter" {
                                parameters.push(node);
                            }
                            let mut cursor = node.walk();
                            nodes.extend(node.named_children(&mut cursor));
                        }
                        let admitted = prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                        if native.traversal.coverage == CoverageStatus::Unknown {
                            unknown += 1;
                            assert!(
                                admitted.is_none(),
                                "native recovery retains whole-module fallback: {language}:{clause:?}"
                            );
                            continue;
                        }
                        complete += 1;
                        let required_default = parameters.iter().any(|node| {
                            node.child_by_field_name("default_value").is_some()
                                && node.child_by_field_name("optional_modifier").is_none()
                        });
                        if required_default {
                            assert!(
                                admitted.is_none(),
                                "required defaults remain outside the literal-default slice: {language}:{clause:?}"
                            );
                            continue;
                        }
                        parameters.sort_by_key(|node| node.start_byte());
                        let prepared = admitted.unwrap_or_else(|| {
                            panic!(
                                "native-complete header trivia is supported: {language}:{clause:?}"
                            )
                        });
                        let actual = prepared.inventory.procedures()[0].parameters();
                        assert_eq!(actual.len(), parameters.len());
                        assert_eq!(normalized.required, 7 + parameters.len() * 2);
                        for (actual, expected) in actual.iter().zip(&parameters) {
                            let name = expected.child_by_field_name("name").unwrap();
                            assert_eq!(actual.name(), name.start_byte()..name.end_byte());
                            assert_eq!(
                                actual.is_optional(),
                                expected.child_by_field_name("optional_modifier").is_some()
                            );
                            assert_eq!(
                                actual.default_value().is_some(),
                                expected.child_by_field_name("default_value").is_some()
                            );
                        }
                        if actual.is_empty() {
                            empty += 1;
                            assert_eq!(
                                normalized
                                    .facts
                                    .iter()
                                    .filter(
                                        |fact| fact.syntax_kind().as_str() == "vba.comment.comment"
                                    )
                                    .count(),
                                1
                            );
                            assert!(normalized.facts.iter().all(|fact| {
                                !matches!(
                                    fact.syntax_kind().as_str(),
                                    "vba.parameter.declaration"
                                        | "vba.omittable_parameter.declaration"
                                )
                            }));
                        }
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        assert_eq!((complete, unknown, empty), (116, 208, 8));
    }

    #[test]
    fn first_party_vba_parameter_default_word_boundaries_use_native_captures() {
        for (expression, kind, default_text, value_text, end) in [
            ("TrueRem", "identifier", "= TrueRem", "TrueRem", 54),
            ("True\x0cRem", "boolean_literal", "= True", "True", 51),
            ("False\x0cRem", "boolean_literal", "= False", "False", 52),
        ] {
            let source =
                format!("Sub Run(ByVal Rem[value], Optional Rem[flag] = {expression})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Complete,
                    "{language}:{expression:?}",
                );
                let pack = fixture
                    .provider
                    .query_packs
                    .get_for_source(native.family, request.source().path().as_str())
                    .unwrap();
                let extracted = pack
                    .extract(
                        native.family,
                        &native.tree,
                        request.source().bytes(),
                        request.limits().max_syntax_nodes(),
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                assert!(extracted.limit.is_none());
                let normalized = normalize_query_candidates(
                    extracted.candidates,
                    &request,
                    usize::MAX,
                    &cancellation,
                )
                .unwrap();
                assert!(!normalized.limited);
                let mut nodes = vec![native.tree.root_node()];
                let mut defaults = Vec::new();
                while let Some(node) = nodes.pop() {
                    if node.kind() == "parameter"
                        && let Some(default) = node.child_by_field_name("default_value")
                    {
                        defaults.push(default);
                    }
                    let mut cursor = node.walk();
                    nodes.extend(node.named_children(&mut cursor));
                }
                assert_eq!(defaults.len(), 1, "{language}:{expression:?}");
                let default = defaults[0];
                let value = default.child_by_field_name("value").unwrap();
                assert_eq!(default.byte_range(), 45..end);
                assert_eq!(source.get(default.byte_range()).unwrap(), default_text);
                assert_eq!(value.kind(), kind);
                assert_eq!(value.byte_range(), 47..end);
                assert_eq!(source.get(value.byte_range()).unwrap(), value_text);
                let default_facts = normalized
                    .facts
                    .iter()
                    .filter(|fact| {
                        fact.span().start_byte() >= u64::try_from(default.start_byte()).unwrap()
                            && fact.span().end_byte() <= u64::try_from(default.end_byte()).unwrap()
                    })
                    .collect::<Vec<_>>();
                match value.kind() {
                    "identifier" => {
                        assert_eq!(default_facts.len(), 1);
                        let fact = default_facts[0];
                        assert_eq!(
                            fact.syntax_kind().as_str(),
                            "vba.default_constant_identifier.reference",
                        );
                        assert_eq!(fact.span().start_byte(), 47);
                        assert_eq!(fact.span().end_byte(), 54);
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                    "boolean_literal" => {
                        assert!(default_facts.is_empty());
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                    kind => panic!(
                        "the narrow default-word control has an unexpected captured value: {language}:{expression:?}:{kind:?}:{value_text:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn first_party_vba_header_parameter_trivia_matches_native_facts() {
        for source in [
            "Sub Run(Rem)\nEnd Sub\n",
            "Sub Run(rEm\x0c)\r\nEnd Sub\r\n",
            "Sub Run(Rem[value], Optional other = 7)\nEnd Sub\n",
            "Sub Run(value Rem)\nEnd Sub\n",
            "Sub Run(ByVal Rem[value])\nEnd Sub\n",
            "Sub Run(Optional Rem[value] = 7)\nEnd Sub\n",
            "Sub Run(value As Rem\x0cLong)\nEnd Sub\n",
            "Sub Run(Optional value = Rem\x0c1)\nEnd Sub\n",
            "Sub Run(Optional value = 1Rem)\nEnd Sub\n",
            "Sub Run(Optional value = \"x\"Rem)\nEnd Sub\n",
            "Sub Run(ByVal Rem[value], Optional Rem[flag] = True\x0cRem)\nEnd Sub\n",
            "Sub Run(Remember, Rem1, [Rem], Optional text = \"Rem\")\nEnd Sub\n",
            "Function Rem$()\nEnd Function\n",
            "Rem\r\nSub Run()\r\nEnd Sub\r\n",
            "Rem\n",
            "Rem",
            "Sub Run(Rem\x0cByVal\x0c[Rem] Rem, _\n Rem\x0cOptional Rem\x0cByRef\x0c[label] Rem\x0cAs Rem\x0cString Rem\x0c= Rem\x0c\"Rem,\"\"As\"\":)\" Rem,[last] Rem)\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_header_trivia_nodes_and_candidate_ceilings_are_exact() {
        for (source, nodes, required) in [
            ("Sub Run(Rem)\nEnd Sub\n", 4, 7),
            ("Sub Run(Rem[value], Optional other = 7)\nEnd Sub\n", 7, 11),
        ] {
            for language in ["vba", "visual-basic-classic"] {
                let mut fixture = Fixture::new(source);
                let cancellation = Cancellation::new();
                let request = fixture.request(language, &[]);
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                assert_eq!(prepared.processed_nodes, nodes);
                for optional in [false, true] {
                    let all = candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        optional,
                        request.limits().max_syntax_nodes(),
                        &cancellation,
                    )
                    .unwrap();
                    let exact = all.len();
                    assert_eq!(
                        all.iter()
                            .filter(|candidate| candidate.role == StructuralRole::Comment)
                            .count(),
                        usize::from(optional)
                    );
                    assert_eq!(
                        normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                            .unwrap()
                            .required,
                        required
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            exact,
                            &cancellation
                        )
                        .unwrap()
                        .len(),
                        exact
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            exact - 1,
                            &cancellation
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: exact,
                            limit: exact - 1
                        })
                    );
                }
                fixture.limits = with_maximum_nodes(&fixture.limits, nodes);
                let request = fixture.request(language, &[]);
                assert_eq!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .unwrap()
                    .processed_nodes,
                    nodes
                );
                fixture.limits = with_maximum_nodes(&fixture.limits, nodes - 1);
                let request = fixture.request(language, &[]);
                assert!(
                    matches!(prepare(&fixture.provider, &request, None, &[], fixture.provider.config.default_settings(), &cancellation),
                    Err(AdapterError::RejectedRequest(RequestError::ProviderLimit { resource: ResourceKind::SyntaxNodes, observed, limit })) if observed == nodes && limit == nodes - 1)
                );
                cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    ),
                    Err(AdapterError::Cancelled {
                        reason: rootlight_cancel::CancellationReason::ClientRequest
                    })
                ));
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_omitted_type_residual_tails_keep_whole_native_fallback() {
        for parameters in [
            "value As",
            "Optional value As",
            "value As UserType",
            "value As [Long]",
            "value As New Object",
            "value As Long extra",
            "value As Long()",
            "Optional value As = 1",
            "Optional value As UserType = 1",
            "value = 1",
            "Optional value = seed.Member",
            "Optional value = 1 + 2",
            "Optional value = -1",
            "Optional value = 1.5%",
            "Optional value = 1%%",
            "Optional value = True And False",
            "Optional value = \"unterminated",
            "Optional value = \"x\" & \"y\"",
            "Optional = 1",
            "ByVal",
            "ByRef",
            "value$",
            "value()",
            "ParamArray values()",
            "value,",
            ", value",
            "value,,other",
            "value, Optional other =",
            "value, valid As Long, invalid As",
            "value unexpected",
        ] {
            let source =
                format!("Sub Accepted(value)\nEnd Sub\nSub Rejected({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{parameters}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_omitted_type_nodes_and_candidate_ceilings_are_exact() {
        for (parameters, parameter_count, defaults, required) in [
            ("value", 1, 0, 9),
            ("ByVal first, ByRef second", 2, 0, 11),
            ("required, Optional value = 1", 2, 1, 11),
            (
                "Optional count = 7, Optional text = \"x\", Optional flag = True, Optional unset",
                4,
                3,
                15,
            ),
            ("typed As Long, untyped, Optional value = 007", 3, 1, 13),
        ] {
            let source = format!("Sub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let mut fixture = Fixture::new(&source);
                let cancellation = Cancellation::new();
                let request = fixture.request(language, &[]);
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                assert_eq!(prepared.processed_nodes, 4 + parameter_count + defaults);
                assert_eq!(
                    prepared.inventory.procedures()[0].parameters().len(),
                    parameter_count
                );
                for optional in [false, true] {
                    let all = candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        optional,
                        request.limits().max_syntax_nodes(),
                        &cancellation,
                    )
                    .unwrap();
                    assert!(
                        all.iter()
                            .all(|candidate| candidate.role != StructuralRole::Reference)
                    );
                    let exact = all.len();
                    assert_eq!(
                        normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                            .unwrap()
                            .required,
                        required
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            exact,
                            &cancellation
                        )
                        .unwrap()
                        .len(),
                        exact
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            exact - 1,
                            &cancellation
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: exact,
                            limit: exact - 1
                        })
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            0,
                            &cancellation
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: required,
                            limit: 0
                        })
                    );
                }
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes);
                let request = fixture.request(language, &[]);
                assert_eq!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .unwrap()
                    .processed_nodes,
                    prepared.processed_nodes
                );
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes - 1);
                let request = fixture.request(language, &[]);
                assert!(
                    matches!(prepare(&fixture.provider, &request, None, &[], fixture.provider.config.default_settings(), &cancellation),
                    Err(AdapterError::RejectedRequest(RequestError::ProviderLimit { resource: ResourceKind::SyntaxNodes, observed, limit }))
                        if observed == prepared.processed_nodes && limit == prepared.processed_nodes - 1)
                );
                cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    ),
                    Err(AdapterError::Cancelled {
                        reason: rootlight_cancel::CancellationReason::ClientRequest
                    })
                ));
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_optional_parameters_match_the_private_native_route() {
        for passing in ["", "ByVal ", "ByRef "] {
            for intrinsic in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                for default in ["", " = 001"] {
                    let source = format!(
                        "Sub Run(required As Long, Optional {passing}value As {intrinsic}{default})\nvalue = value\nEnd Sub\n"
                    );
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for source in [
            "Sub Run(Optional value As Long = 1)\nEnd Sub\n",
            "Sub Run(ByVal first As Long, Optional second As Long = 1)\nEnd Sub\n",
            "Sub Run(Optional first As Long, second As Long)\nEnd Sub\n",
            "Sub Run(Optional value As Long, Optional value As Long = 7)\nEnd Sub\n",
            "Sub Run(Optional [Me] As String = \"a,\"\"b\"\":()\", Optional [Rem] As Boolean = False):[Me] = [Me]:End Sub\n",
            "Private Function Result(Optional ByRef value As Long = 7, Optional ByVal flag As Boolean = True) As Long\nResult = value\nEnd Function\n",
            "Sub Run(Optional value As String = \"\") ' header\r\nvalue = value ' body\r\nEnd Sub ' terminator\r\n",
            "Public _\r\nSub Run( _\n first As Long, _\r\n Optional _\n ByVal _\r\n [value] _\n As _\r\n Long _\n = _\r\n 001, _\n Optional text As String = \"a,\"\"b\"\":)\" _\r\n )\r\n[value] = first\r\nEnd Sub\r\n",
            "Sub Accept(required As Long, Optional count As Long = 7, Optional text As String = \"x\")\nEnd Sub\nSub Run()\nDim local As Long\nCall Accept(1)\nCall Accept(1, local, \"replacement\")\nEnd Sub\n",
            "Function First(Optional value As Long) As Long\nFirst = value\nEnd Function\nSub Second(Optional value As Boolean = fAlSe)\nvalue = value\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_optional_name_positions_never_gain_false_complete_admission() {
        let mut unexpected = Vec::new();
        let mut missing = Vec::new();
        for name in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
            "Optional",
            "ByVal",
            "ByRef",
            "ParamArray",
            "As",
            "Rem",
        ] {
            for passing in ["", "ByVal ", "ByRef "] {
                for clause in [
                    format!("Optional {passing}{name} As Long"),
                    format!("Optional {passing}{name} As Long = 1"),
                    format!("Optional {passing}[{name}] As String = \"x\""),
                ] {
                    let source = format!("Sub Run({clause})\nEnd Sub\n");
                    for language in ["vba", "visual-basic-classic"] {
                        let fixture = Fixture::new(&source);
                        let request = fixture.request(language, &[]);
                        let cancellation = Cancellation::new();
                        let native = fixture
                            .provider
                            .prepare_tree(
                                &request,
                                None,
                                &[],
                                fixture.provider.config.default_settings(),
                                &cancellation,
                            )
                            .unwrap();
                        assert!(native.traversal.fully_traversed);
                        let admitted = prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap()
                        .is_some();
                        if native.traversal.coverage == CoverageStatus::Unknown {
                            if admitted {
                                unexpected.push(format!("{language}:{clause}"));
                            }
                        } else {
                            assert_eq!(
                                native.traversal.coverage,
                                CoverageStatus::Complete,
                                "{language}:{clause}"
                            );
                            if admitted {
                                compare_native(&Fixture::new(&source), language, &[]);
                            } else {
                                missing.push(format!("{language}:{clause}"));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            unexpected.is_empty(),
            "native-recovered Optional names keep whole-module fallback: {unexpected:?}"
        );
        assert!(
            missing.is_empty(),
            "native-complete Optional parameter spellings belong to the written slice: {missing:?}"
        );
    }

    #[test]
    fn first_party_vba_optional_default_positions_preserve_atomic_admission() {
        for default in [
            "True",
            "False",
            "tRuE",
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
            "Optional",
            "ByVal",
            "ByRef",
            "ParamArray",
            "As",
            "Rem",
            "Nothing",
            "Null",
            "Empty",
            "Me",
            "[True]",
            "[Name]",
        ] {
            let source = format!("Sub Run(Optional value As Variant = {default})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert!(matches!(
                    native.traversal.coverage,
                    CoverageStatus::Complete | CoverageStatus::Unknown
                ));
                let admitted = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .is_some();
                if matches!(
                    default,
                    "True"
                        | "False"
                        | "tRuE"
                        | "Name"
                        | "Line"
                        | "Load"
                        | "Unload"
                        | "Input"
                        | "Print"
                        | "Close"
                        | "Reset"
                        | "Beep"
                        | "DefBool"
                        | "DefByte"
                        | "DefInt"
                        | "DefLng"
                        | "DefLngLng"
                        | "DefLngPtr"
                        | "DefCur"
                        | "DefSng"
                        | "DefDbl"
                        | "DefDate"
                        | "DefStr"
                        | "DefObj"
                        | "DefVar"
                        | "DefDec"
                        | "[True]"
                        | "[Name]"
                ) {
                    assert_eq!(native.traversal.coverage, CoverageStatus::Complete);
                    assert!(admitted);
                    compare_native(&Fixture::new(&source), language, &[]);
                } else {
                    assert!(
                        !admitted,
                        "a reserved expression is outside the authored atomic default slice: {language}:{default}"
                    );
                }
            }
        }
    }

    #[test]
    fn first_party_vba_optional_residual_tails_keep_whole_native_fallback() {
        for parameters in [
            "Optional value As Long =",
            "Optional value As Long = 1 + 2",
            "Optional value As Long = Target()",
            "Optional value As Long = seed.Member",
            "Optional value As Long = -1",
            "Optional value As Double = 1.5%",
            "Optional value As Long = &H10%%",
            "Optional value As Date = #1/1/2000#",
            "Optional value As String = \"unterminated",
            "Optional value As String = \"x\" & \"y\"",
            "ByVal value As Long = 1",
            "value As Long = 1",
            "ByVal Optional value As Long = 1",
            "Optional Optional value As Long = 1",
            "Optional ParamArray value() As Variant",
            "Optional values() As Long",
            "Optional value As UserType = 1",
            "Optional value As [Types].[Result] = 1",
            "Optional value As String * 4 = \"x\"",
            "Optional value As Long = 1,",
            ", Optional value As Long = 1",
            "Optional first As Long = 1,, Optional second As Long = 2",
            "first As Long, Optional second As Long = 1, third As UserType",
        ] {
            let source = format!("Sub Empty()\nEnd Sub\nSub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{parameters}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_module_scalar_variables_match_native_facts() {
        for source in [
            "Dim stored",
            "Public stored As Long\n",
            "Private [stored] As String ' tail\r\n",
            "Dim stored\nDim stored\n",
            "' leading\nAttribute VB_Name = \"Authored\"\nOption Explicit\nPublic stored As Long\nConst Limit = 7\nSub Run()\nstored = Limit\nEnd Sub\n",
            "Private _\r\n[stored] _\r\nAs _\r\nLong\r\nSub Run()\n[stored] = [stored]\nEnd Sub\n",
            "Sub First()\nEnd Sub\nDim stored\nSub Second(ByVal stored As Long)\nstored = stored\nEnd Sub\nPrivate Last As Boolean\n",
            "Public stored As Long\nSub Run(Optional value = stored)\nDim stored As Long\nstored = stored\nEnd Sub\n",
            "Dim [True]\nPublic Name\nPrivate Line As Long\nDim Load As Long\n",
            "Dim stored\nConst Limit = 7\nPublic Last As Long\nSub Run()\nDim stored As Long\nCall Target(stored, Limit)\nEnd Sub\n",
            "Dim stored As Long\nConst stored = 7\nSub Run(Optional value = stored)\nEnd Sub\n",
            "Const stored = 7\nDim stored As Long\nSub Run(Optional value = stored)\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for ty in [
            "",
            " As String",
            " As Boolean",
            " As Byte",
            " As Integer",
            " As Long",
            " As LongLong",
            " As LongPtr",
            " As Single",
            " As Double",
            " As Currency",
            " As Date",
            " As Variant",
            " As Object",
        ] {
            for prefix in ["Dim", "Public", "Private"] {
                for name in ["stored", "[Item]", "Name"] {
                    for newline in ["\n", "\r\n"] {
                        let source = format!(
                            "{prefix} {name}{ty}{newline}Sub Run(){newline}End Sub{newline}"
                        );
                        for language in ["vba", "visual-basic-classic"] {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn first_party_vba_module_variable_names_keep_exact_native_admission() {
        for prefix in ["Dim", "Public", "Private"] {
            for name in [
                "Name",
                "Line",
                "Load",
                "Unload",
                "Input",
                "Print",
                "Close",
                "Reset",
                "Beep",
                "DefBool",
                "DefByte",
                "DefInt",
                "DefLng",
                "DefLngLng",
                "DefLngPtr",
                "DefCur",
                "DefSng",
                "DefDbl",
                "DefDate",
                "DefStr",
                "DefObj",
                "DefVar",
                "DefDec",
                "[True]",
                "[Me]",
                "[Rem]",
                "[Dim]",
                "[ReDim]",
            ] {
                for ty in ["", " As Long"] {
                    let source = format!("{prefix} {name}{ty}\nSub Run()\nEnd Sub\n");
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for name in [
            "Dim", "ReDim", "Rem", "True", "False", "Sub", "Function", "Public", "Private",
        ] {
            let source = format!("Dim {name} As Long\nSub Run()\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                let expected_coverage = if matches!(name, "Dim" | "ReDim" | "Rem") {
                    CoverageStatus::Unknown
                } else {
                    CoverageStatus::Complete
                };
                assert_eq!(
                    native.traversal.coverage, expected_coverage,
                    "exact native admission for a conservatively excluded spelling: {language}:{name}"
                );
                if expected_coverage == CoverageStatus::Complete {
                    let pack = fixture
                        .provider
                        .query_packs
                        .get_for_source(native.family, request.source().path().as_str())
                        .unwrap();
                    let legacy = pack
                        .extract(
                            native.family,
                            &native.tree,
                            request.source().bytes(),
                            request.limits().max_syntax_nodes(),
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                    assert!(legacy.limit.is_none());
                    let normalized = normalize_query_candidates(
                        legacy.candidates,
                        &request,
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                    assert_eq!(
                        normalized
                            .facts
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "vba.identifier.definition"
                                    && fact.span().start_byte() == 4
                                    && fact.span().end_byte()
                                        == u64::try_from(4 + name.len()).unwrap()
                            })
                            .count(),
                        1,
                        "the native route retains the exact written definition: {language}:{name}"
                    );
                    assert_eq!(
                        normalized
                            .facts
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "vba.variable.declaration"
                                    && fact.span().start_byte() == 4
                                    && fact.span().end_byte()
                                        == u64::try_from(4 + name.len() + " As Long".len()).unwrap()
                            })
                            .count(),
                        1,
                        "the contextual name belongs to its full authored declaration: {language}:{name}"
                    );
                    assert!(normalized.facts.iter().all(|fact| {
                        !fact.syntax_kind().as_str().ends_with(".reference")
                            || fact.span().start_byte() != 4
                            || fact.span().end_byte() != u64::try_from(4 + name.len()).unwrap()
                    }));
                }
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{name}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_module_variables_obey_exact_node_and_candidate_ceilings() {
        let source = "Public stored As Long\nSub Run()\nstored = stored\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.inventory.variables().len(), 1);
            assert_eq!(prepared.processed_nodes, 6);
            for (optional, expected) in [(false, 9), (true, 13)] {
                let exact = candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    optional,
                    expected,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(exact.len(), expected);
                assert_eq!(
                    exact
                        .iter()
                        .filter(|fact| fact.syntax == "vba.variable")
                        .count(),
                    1
                );
                assert_eq!(
                    exact
                        .iter()
                        .filter(|fact| fact.syntax == "vba.public")
                        .count(),
                    usize::from(optional)
                );
                assert_eq!(
                    exact
                        .iter()
                        .filter(|fact| fact.syntax == "vba.value_identifier")
                        .count(),
                    usize::from(optional)
                );
                assert!(
                    matches!(candidates(&prepared.inventory, source.as_bytes(), optional, expected - 1, &cancellation), Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes, observed, limit,
                })) if observed == expected && limit == expected - 1)
                );
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, 6);
            let request = fixture.request(language, &[]);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                6
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, 5);
            let request = fixture.request(language, &[]);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 6,
                    limit: 5,
                }))
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    true,
                    13,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest,
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_module_array_work_obeys_exact_node_and_candidate_ceilings() {
        let source = "Public items(-2 To +3, 004) As Long\nSub Run()\nitems = items\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.inventory.variables()[0].array_work_nodes(), 8);
            assert_eq!(prepared.processed_nodes, 14);
            for (optional, required) in [(false, 9), (true, 13)] {
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        optional,
                        required,
                        &cancellation
                    )
                    .unwrap()
                    .len(),
                    required
                );
                assert!(
                    matches!(candidates(&prepared.inventory, source.as_bytes(), optional, required - 1, &cancellation), Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes, observed, limit,
                })) if observed == required && limit == required - 1)
                );
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, 14);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                14
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, 13);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 14,
                    limit: 13,
                }))
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    true,
                    13,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
        let bounds = std::iter::repeat_n("2", 64).collect::<Vec<_>>().join(",");
        let source = format!("Dim items({bounds})\n");
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(&source);
            fixture.limits = with_maximum_nodes(&fixture.limits, 130);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new()
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 131,
                    limit: 130,
                }))
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_module_numeric_arrays_match_exact_native_facts() {
        for ty in [
            "",
            " As String",
            " As Boolean",
            " As Byte",
            " As Integer",
            " As Long",
            " As LongLong",
            " As LongPtr",
            " As Single",
            " As Double",
            " As Currency",
            " As Date",
            " As Variant",
            " As Object",
        ] {
            for prefix in ["Dim", "Public", "Private"] {
                for bounds in [
                    "",
                    "2",
                    "+2",
                    "-2",
                    "-2 To +3",
                    "0 To 2, 1 To 3",
                    "2,3,4",
                    "+ 2",
                    "- _\n 2",
                ] {
                    for newline in ["\n", "\r\n"] {
                        let bounds = bounds.replace('\n', newline);
                        let source = format!(
                            "{prefix} [items]({bounds}){ty}{newline}Sub Run(){newline}End Sub{newline}"
                        );
                        for language in ["vba", "visual-basic-classic"] {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
        for source in [
            "Dim items()",
            "Public items(0 To 2, -1 To +3) As Long\n",
            "Private _\r\n[items] _\r\n( _\r\n- _\r\n1 _\r\nTo _\r\n+ _\r\n2 _\r\n, _\r\n003 _\r\n) _\r\nAs _\r\nVariant ' tail\r\n",
            "Dim items(2)\nDim items(3)\n",
            "Attribute VB_Name = \"Arrays\"\nOption Explicit\nDim items()\nConst Limit=7\nSub Run(Optional initial=items)\nCall items()\nEnd Sub\n",
            "Sub First()\nEnd Sub\nPublic items(-2 To 3, 004) As Long\nSub Second()\nEnd Sub\nDim last()\n",
            "Dim items(2)\nConst items=7\nSub Run(Optional initial=items)\nEnd Sub\n",
            "Const items=7\nDim items(-1 To 3)\nSub Run(Optional initial=items)\nEnd Sub\n",
            "Dim items(1)As Long\n",
            "Dim items(1To -2)\n",
            "Dim items(1 To-2)\n",
            "Dim items(1To+2)\n",
            "Dim items(1To2)\n",
            "Dim items(1 _\r\nTo _\r\n-2)\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        let magnitude = "9".repeat(2048);
        let source = format!("Dim items(-{magnitude} To +{magnitude}, {magnitude}) As Long\n");
        for language in ["vba", "visual-basic-classic"] {
            compare_native(&Fixture::new(&source), language, &[]);
        }
    }

    #[test]
    fn first_party_vba_module_array_bounds_keep_whole_fallback_for_other_expressions() {
        for declaration in [
            "Dim items(Limit)",
            "Dim items(-Limit)",
            "Dim items([Limit])",
            "Dim items(1 + 2)",
            "Dim items(Target())",
            "Dim items(1.5%)",
            "Dim items(&HFF%%)",
            "Dim items(&O7%%)",
            "Dim items(2&&)",
            "Dim items(\"2\")",
            "Dim items(True)",
            "Dim items(,2)",
            "Dim items(2,)",
            "Dim items(2,,3)",
            "Dim items(1 To)",
            "Dim items(To 2)",
            "Dim items(1 To 2 To 3)",
            "Dim items((2))",
            "Dim items(2",
            "Dim items(2))",
            "Dim items(+)",
            "Dim items(--2)",
            "Dim items(2) = 1",
            "Dim items(2), last(Limit)",
            "Dim items As Long()",
            "Dim items() As New Object",
            "Dim items() As Product",
            "Dim items(): Dim last",
            "Dim items()\nOption Explicit",
        ] {
            let source =
                format!("Dim previous\nSub First()\nEnd Sub\n{declaration}\nSub Run()\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                assert!(
                    prepare(
                        &fixture.provider,
                        &fixture.request(language, &[]),
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{declaration}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_grouped_module_variables_match_exact_native_facts() {
        for ty in [
            "",
            " As String",
            " As Boolean",
            " As Byte",
            " As Integer",
            " As Long",
            " As LongLong",
            " As LongPtr",
            " As Single",
            " As Double",
            " As Currency",
            " As Date",
            " As Variant",
            " As Object",
        ] {
            for prefix in ["Dim", "Public", "Private"] {
                for bounds in ["", "2", "-2 To +3", "0 To 2, 1 To 3", "2,3,4", "- _\n 2"] {
                    for newline in ["\n", "\r\n"] {
                        let bounds = bounds.replace('\n', newline);
                        let source = format!(
                            "{prefix} [first], second{ty}, grid({bounds}) As Long, pending(){newline}Sub Run(Optional seed=first){newline}Dim value As Long{newline}value=second{newline}Call grid(){newline}End Sub{newline}"
                        );
                        for language in ["vba", "visual-basic-classic"] {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
        for source in [
            "Dim first, second As Long",
            "Public first As Long, second\n",
            "Private first(),second(1To2)As Long\n",
            "Dim first, _\r\n second(2,3)As Long\n",
            "Dim first _\r\n,second()\n",
            "Public _\r\n first _\n, _\r\n[second] _\n( _\r\n-1 _\nTo +2,3) _\r\nAs _\nVariant, last\n",
            "Private first, [Public] As Long, [Rem](), [Dim](2)\n",
            "Dim first, Rem2, Remember, [Rem]\n",
            "Public stored As Long, [STORED](2)\nSub Run(Optional value=stored)\nEnd Sub\n",
            "Attribute VB_Name = \"Grouped\"\nOption Explicit\nDim first, second(2)\nConst Seed=7\nSub Run(Optional value=Seed)\nCall second()\nEnd Sub\n",
            "Dim before, early(2)\nSub First()\nEnd Sub\nPublic middle As Long, grid(-1 To 3, 004)\nSub Second()\nEnd Sub\nPrivate after(), last As String\n",
            "Dim first, items(2)\nConst items=7\nSub Run(Optional value=items)\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for name in [
            "Name", "Line", "Remember", "Rem2", "[True]", "[Public]", "[As]", "[To]", "[Dim]",
            "[ReDim]",
        ] {
            for prefix in ["Dim", "Public", "Private"] {
                let source = format!("{prefix} first, {name}(2) As Long, last\n");
                for language in ["vba", "visual-basic-classic"] {
                    compare_native(&Fixture::new(&source), language, &[]);
                }
            }
        }
        let declarations = (0..128)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Private {declarations}\n");
        for language in ["vba", "visual-basic-classic"] {
            compare_native(&Fixture::new(&source), language, &[]);
        }
    }

    #[test]
    fn first_party_vba_grouped_module_keyword_names_keep_exact_closed_admission() {
        for name in [
            "Dim", "ReDim", "Rem", "True", "False", "Sub", "Function", "Public", "Private", "As",
            "To", "Empty",
        ] {
            let source = format!("Dim first, {name} As Long\nSub Run()\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .is_none()
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                let expected = if matches!(name, "Dim" | "ReDim" | "Rem") {
                    CoverageStatus::Unknown
                } else {
                    CoverageStatus::Complete
                };
                assert_eq!(native.traversal.coverage, expected, "{language}: {name}");
                if expected == CoverageStatus::Complete {
                    let pack = fixture
                        .provider
                        .query_packs
                        .get_for_source(native.family, request.source().path().as_str())
                        .unwrap();
                    let extracted = pack
                        .extract(
                            native.family,
                            &native.tree,
                            request.source().bytes(),
                            request.limits().max_syntax_nodes(),
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                    assert!(extracted.limit.is_none());
                    let normalized = normalize_query_candidates(
                        extracted.candidates,
                        &request,
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                    let start = u64::try_from("Dim first, ".len()).unwrap();
                    for (syntax, length) in [
                        ("vba.identifier.definition", name.len()),
                        ("vba.variable.declaration", name.len() + " As Long".len()),
                    ] {
                        assert_eq!(
                            normalized
                                .facts
                                .iter()
                                .filter(|fact| fact.syntax_kind().as_str() == syntax
                                    && fact.span().start_byte() == start
                                    && fact.span().end_byte()
                                        == start + u64::try_from(length).unwrap())
                                .count(),
                            1,
                            "exact native after-comma declaration: {language}: {name}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn first_party_vba_grouped_module_work_and_candidates_obey_exact_ceilings() {
        let source = "Public alpha, beta As Long, grid(-1 To +2, 3) As Variant, pending()\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let prepared = prepare(
                &fixture.provider,
                &fixture.request(language, &[]),
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.inventory.statement_count(), 1);
            assert_eq!(prepared.inventory.variables().len(), 4);
            assert_eq!(prepared.processed_nodes, 14);
            for (optional, required) in [(false, 11), (true, 15)] {
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        optional,
                        required,
                        &cancellation
                    )
                    .unwrap()
                    .len(),
                    required
                );
                assert!(
                    matches!(candidates(&prepared.inventory, source.as_bytes(), optional, required - 1, &cancellation), Err(AdapterError::RejectedRequest(RequestError::ProviderLimit { resource: ResourceKind::SyntaxNodes, observed, limit })) if observed == required && limit == required - 1)
                );
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, 14);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                14
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, 13);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 14,
                    limit: 13
                }))
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    true,
                    15,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
        let declarations = (0..128)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Dim {declarations}\n");
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(&source);
            fixture.limits = with_maximum_nodes(&fixture.limits, 129);
            let cancellation = Cancellation::new();
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                129
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, 128);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &fixture.request(language, &[]),
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 129,
                    limit: 128
                }))
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_grouped_module_variables_keep_whole_fallback_for_late_tails() {
        for declaration in [
            "Dim first,",
            "Dim first,,second",
            "Dim first, As Long",
            "Dim first, second As",
            "Dim first, second As Product",
            "Dim first, second As New Object",
            "Dim first, second=1",
            "Dim first, second(Limit)",
            "Dim first, second(1+2)",
            "Dim first, second(2,)",
            "Dim first, second As Long()",
            "Dim first, second$",
            "Dim first, [two words]",
            "Dim first, [a,b]",
            "Dim first,\nsecond",
            "Dim first, Public second",
            "Dim first, second: Dim last",
            "Dim first, second\nOption Explicit",
            "Dim first, second\nSub Run()\nDim local, other As Product\nEnd Sub",
        ] {
            let source = format!("Dim accepted, earlier(2)\nSub First()\nEnd Sub\n{declaration}\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                assert!(
                    prepare(
                        &fixture.provider,
                        &fixture.request(language, &[]),
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none(),
                    "{language}: {declaration}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_richer_module_variables_keep_whole_native_fallback() {
        for declaration in [
            "Dim",
            "Public",
            "Private",
            "Dim stored As",
            "Public stored As UserType",
            "Private stored As New Object",
            "Dim stored As String * 3",
            "Dim stored$",
            "Dim stored = 1",
            "Dim stored As Long = 1",
            "Dim first, second As Product",
            "Dim stored(Limit) As Long",
            "Public stored(1 + 3) As Long",
            "Global stored As Long",
            "Friend stored As Long",
            "Static stored As Long",
            "Public Dim stored As Long",
            "Private WithEvents stored As Object",
            "Dim stored: Dim last",
            "Dim stored\nOption Explicit",
            "Dim stored\nAttribute VB_Name = \"Late\"",
        ] {
            let source = format!("Sub First()\nEnd Sub\n{declaration}\nSub Run()\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{declaration}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_module_constants_and_named_defaults_match_native_facts() {
        for source in [
            "Const Limit=7",
            "Public Const [Limit] = 007\n",
            "Private Const Label As String = \"a,\"\"b\"\":()\" ' tail\r\n",
            "Const Flag As Boolean = False\nConst Flag As Boolean = True\n",
            "' leading\nAttribute VB_Name = \"Authored\"\nOption Explicit\nPrivate Const Fallback As Long = 3\nSub Run(Optional value = [FALLBACK])\nConst Fallback As Long = 8\nvalue = Fallback\nEnd Sub\n",
            "Public _\r\nConst _\r\n[Limit] _\r\nAs _\r\nLong _\r\n= _\r\n7\r\nSub Run(Optional value As Long = _\r\n[Limit])\r\nEnd Sub\r\n",
            "Sub First(Optional value = Limit)\nEnd Sub\nPrivate Const Limit = 3\nSub Second(Optional value As Long = Limit)\nEnd Sub\nPublic Const Last As Boolean = True\n",
            "Sub Run(Optional value = TrueRem)\nEnd Sub\nConst TrueRem = False\n",
            "Const [True] = 3\nSub Run(Optional value = [True])\nEnd Sub\n",
            "Sub Run(Optional value = Missing)\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for ty in [
            "",
            " As String",
            " As Boolean",
            " As Byte",
            " As Integer",
            " As Long",
            " As LongLong",
            " As LongPtr",
            " As Single",
            " As Double",
            " As Currency",
            " As Date",
            " As Variant",
            " As Object",
        ] {
            for visibility in ["", "Public ", "Private "] {
                for (name, value) in [("Limit", "7"), ("[Flag]", "True"), ("Label", "\"text\"")] {
                    for newline in ["\n", "\r\n"] {
                        let source = format!(
                            "{visibility}Const {name}{ty} = {value}{newline}Sub Run(Optional value{ty} = {name}){newline}End Sub{newline}"
                        );
                        for language in ["vba", "visual-basic-classic"] {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn first_party_vba_module_constants_obey_exact_node_and_candidate_ceilings() {
        let source = "Const Limit = 7\nSub Run(Optional value = Limit)\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.inventory.constants().len(), 1);
            assert_eq!(prepared.processed_nodes, 7);
            for (optional, expected) in [(false, 11), (true, 14)] {
                let exact = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    expected,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(exact.len(), expected);
                assert_eq!(
                    exact
                        .iter()
                        .filter(|fact| fact.syntax == "vba.constant")
                        .count(),
                    1
                );
                assert_eq!(
                    exact
                        .iter()
                        .filter(|fact| fact.syntax == "vba.default_constant_identifier")
                        .count(),
                    usize::from(optional)
                );
                assert!(matches!(candidates(
                    &prepared.inventory, request.source().bytes(), optional, expected - 1, &cancellation,
                ), Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes, observed, limit,
                })) if observed == expected && limit == expected - 1));
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, 7);
            let request = fixture.request(language, &[]);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                7
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, 6);
            let request = fixture.request(language, &[]);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 7,
                    limit: 6,
                }))
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    true,
                    14,
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest,
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_module_constant_exclusions_keep_whole_native_fallback() {
        for declaration in [
            "Const Missing",
            "Const Value =",
            "Const Value = Other",
            "Const Value = Other.Member",
            "Const Value = Target()",
            "Const Value = 1 + 2",
            "Const Value = -1",
            "Const Value = &H10%%",
            "Const Value = #1/1/2000#",
            "Const Value = 1.5%",
            "Const Value = 1, Other = 2",
            "Const Value As = 1",
            "Const Value As UserType = 1",
            "Const Value As New Object = 1",
            "Const Value As String * 3 = \"abc\"",
            "Const Value$ = \"abc\"",
            "Const Values() = 1",
            "Friend Const Value = 1",
            "Global Const Value = 1",
            "Const Value = \"unterminated",
            "#Const Value = 1",
            "Const Value = 1: Const Other = 2",
            "Const Value = 1\nOption Explicit",
            "Const Value = 1\nAttribute VB_Name = \"Late\"",
        ] {
            let source = format!("Sub First()\nEnd Sub\n{declaration}\nSub Run()\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "{language}:{declaration}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_optional_default_nodes_and_candidate_ceilings_are_exact() {
        let source = "Sub Run(required As Long, Optional count As Long = 7, Optional text As String = \"x\", Optional flag As Boolean = True, Optional unset As Long)\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.processed_nodes, 12);
            assert_eq!(
                prepared.inventory.procedures()[0]
                    .parameters()
                    .iter()
                    .filter(|parameter| parameter.default_value().is_some())
                    .count(),
                3
            );
            for optional in [false, true] {
                let all = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap();
                let exact = all.len();
                assert_eq!(
                    all.iter()
                        .filter(|candidate| candidate.syntax == "vba.omittable_parameter")
                        .count(),
                    4
                );
                assert_eq!(
                    all.iter()
                        .filter(|candidate| candidate.syntax == "vba.string")
                        .count(),
                    usize::from(optional)
                );
                assert!(
                    all.iter()
                        .all(|candidate| candidate.role != StructuralRole::Reference)
                );
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    17
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact,
                        &cancellation
                    )
                    .unwrap()
                    .len(),
                    exact
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact - 1,
                        &cancellation
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: exact,
                        limit: exact - 1
                    })
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        0,
                        &cancellation
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: 17,
                        limit: 0
                    })
                );
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes);
            let request = fixture.request(language, &[]);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                12
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes - 1);
            let request = fixture.request(language, &[]);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 12,
                    limit: 11
                }))
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_parameter_positions_never_gain_false_complete_admission() {
        let mut unexpected_admissions = Vec::new();
        for name in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for passing in ["", "ByVal ", "ByRef "] {
                let source = format!("Sub Run({passing}{name} As Long)\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(native.traversal.fully_traversed);
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_some();
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        if admitted {
                            unexpected_admissions.push(format!("{language}:{passing}{name}"));
                        }
                    } else {
                        assert_eq!(
                            native.traversal.coverage,
                            CoverageStatus::Complete,
                            "the parameter-position oracle must not be bounded: {language}, {passing}{name}"
                        );
                        assert!(
                            admitted,
                            "native-complete parameter names belong to the closed slice: {language}, {passing}{name}"
                        );
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        assert!(
            unexpected_admissions.is_empty(),
            "native-recovered parameter names retain whole-module fallback: {unexpected_admissions:?}"
        );
    }

    #[test]
    fn first_party_vba_required_keyword_parameter_positions_keep_the_native_contract() {
        let mut complete = 0;
        let mut unknown = 0;
        for name in ["Optional", "ByVal", "ByRef", "As", "ParamArray", "Rem"] {
            for passing in ["", "ByVal ", "ByRef "] {
                let source = format!("Sub Run({passing}{name} As Long)\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(native.traversal.fully_traversed);
                    assert!(matches!(
                        native.traversal.coverage,
                        CoverageStatus::Complete | CoverageStatus::Unknown
                    ));
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        unknown += 1;
                        assert!(
                            admitted.is_none(),
                            "native-recovered required names keep whole-module fallback: {language}:{passing}{name}"
                        );
                        continue;
                    }
                    complete += 1;
                    let prepared = admitted.unwrap_or_else(|| {
                        panic!(
                            "native-complete required names belong to the written slice: {language}:{passing}{name}"
                        )
                    });
                    let parameters = prepared.inventory.procedures()[0].parameters();
                    assert_eq!(parameters.len(), 1);
                    let parameter = &parameters[0];
                    assert!(!parameter.is_optional());
                    assert!(parameter.default_value().is_none());
                    let name_start = "Sub Run(".len() + passing.len();
                    assert_eq!(parameter.name(), name_start..name_start + name.len());
                    let pack = fixture
                        .provider
                        .query_packs
                        .get_for_source(native.family, request.source().path().as_str())
                        .unwrap();
                    let extracted = pack
                        .extract(
                            native.family,
                            &native.tree,
                            request.source().bytes(),
                            request.limits().max_syntax_nodes(),
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                    assert!(extracted.limit.is_none());
                    let normalized = normalize_query_candidates(
                        extracted.candidates,
                        &request,
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                    assert!(!normalized.limited);
                    assert_eq!(normalized.required, 9);
                    assert_eq!(
                        normalized
                            .facts
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "vba.parameter.declaration"
                            })
                            .count(),
                        1
                    );
                    assert!(normalized.facts.iter().all(|fact| {
                        fact.syntax_kind().as_str() != "vba.omittable_parameter.declaration"
                    }));
                    assert_eq!(
                        normalized
                            .facts
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == "vba.identifier.definition"
                                    && fact.span().start_byte()
                                        == u64::try_from(name_start).unwrap()
                                    && fact.span().end_byte()
                                        == u64::try_from(name_start + name.len()).unwrap()
                            })
                            .count(),
                        1
                    );
                    compare_native(&Fixture::new(&source), language, &[]);
                }
            }
        }
        assert_eq!((complete, unknown), (18, 18));
    }

    #[test]
    fn first_party_vba_unsupported_parameters_keep_whole_native_fallback() {
        for parameters in [
            "Optional value As Long = seed.Member",
            "ByVal value As Long = 1",
            "ParamArray values() As Variant",
            "ByRef values() As Long",
            "ByVal value As UserType",
            "ByVal value As [Types].[Result]",
            "ByVal value As New Object",
            "ByVal value As String * 4",
            "ByVal value unexpected",
            "ByVal ByRef value As Long",
            "ByVal value$ As String",
            "ByVal first As Long,",
            ", ByVal value As Long",
            "ByVal first As Long,, ByRef second As Long",
            "ByVal first As Long, Optional second As Long = 1 + 2",
            "ByVal first As Long, ByRef second As UserType",
        ] {
            let source = format!("Sub Empty()\nEnd Sub\nSub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported parameter never publishes a supported prefix: {language}, {parameters}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    fn with_maximum_nodes(limits: &AnalysisLimits, maximum: usize) -> AnalysisLimits {
        AnalysisLimits::new(
            limits.max_source_bytes(),
            maximum,
            limits.max_syntax_depth(),
            limits.max_embedded_ranges(),
            limits.max_reported_memory_bytes(),
            limits.syntax_stream().clone(),
            limits.ir_stream().clone(),
            limits.ir().clone(),
        )
        .unwrap()
    }

    #[test]
    fn first_party_vba_parameter_nodes_and_candidate_ceilings_are_exact() {
        for (parameters, parameter_count, required) in [
            ("ByVal value As Long", 1, 9),
            ("ByVal first As Long, ByRef second As String", 2, 11),
        ] {
            let source = format!("Sub Run({parameters})\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let mut fixture = Fixture::new(&source);
                let cancellation = Cancellation::new();
                let request = fixture.request(language, &[]);
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    prepared.inventory.procedures()[0].parameters().len(),
                    parameter_count
                );
                assert_eq!(prepared.processed_nodes, 4 + parameter_count);
                for optional in [false, true] {
                    let all = candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        request.limits().max_syntax_nodes(),
                        &cancellation,
                    )
                    .unwrap();
                    let exact = all.len();
                    assert_eq!(
                        normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                            .unwrap()
                            .required,
                        required
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            exact,
                            &cancellation
                        )
                        .unwrap()
                        .len(),
                        exact
                    );
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            exact - 1,
                            &cancellation
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: exact,
                            limit: exact - 1,
                        })
                    );
                }
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes);
                let request = fixture.request(language, &[]);
                assert_eq!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    )
                    .unwrap()
                    .unwrap()
                    .processed_nodes,
                    prepared.processed_nodes
                );
                fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes - 1);
                let request = fixture.request(language, &[]);
                assert!(matches!(
                    prepare(&fixture.provider, &request, None, &[], fixture.provider.config.default_settings(), &cancellation),
                    Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed,
                        limit,
                    })) if observed == prepared.processed_nodes && limit == prepared.processed_nodes - 1
                ));
                assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
                assert!(matches!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation
                    ),
                    Err(AdapterError::Cancelled {
                        reason: rootlight_cancel::CancellationReason::ClientRequest,
                    })
                ));
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_contextual_names_match_the_private_native_route() {
        for body in [
            "Name = Name",
            "Let Name = Name",
            "Dim Name As Long\nName = Name",
            "Line = Line",
            "Let Line = Line",
            "Dim Line As Long\nLine = Line",
            "[Load] = 1",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    fn compare_native_procedural_body(body: &str) {
        let source = format!("Sub Run()\n{body}\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            compare_native(&Fixture::new(&source), language, &[]);
        }
    }

    #[test]
    fn first_party_vba_contextual_load_let_destination_matches_the_private_native_route() {
        compare_native_procedural_body("Let Load = 1");
    }

    #[test]
    fn first_party_vba_contextual_load_variable_declaration_matches_the_private_native_route() {
        compare_native_procedural_body("Dim Load As Long");
    }

    #[test]
    fn first_party_vba_contextual_load_constant_declaration_matches_the_private_native_route() {
        compare_native_procedural_body("Const Load As Long = 1");
    }

    #[test]
    fn first_party_vba_contextual_load_value_matches_the_private_native_route() {
        compare_native_procedural_body("value = Load");
    }

    #[test]
    fn first_party_vba_contextual_load_call_matches_the_private_native_route() {
        compare_native_procedural_body("Call Load()");
    }

    #[test]
    fn first_party_vba_contextual_unload_let_destination_matches_the_private_native_route() {
        compare_native_procedural_body("Let Unload = 1");
    }

    #[test]
    fn first_party_vba_contextual_unload_bracketed_assignment_matches_the_private_native_route() {
        compare_native_procedural_body("[Unload] = 1");
    }

    #[test]
    fn first_party_vba_contextual_bare_load_and_unload_keep_recovered_native_route() {
        for body in ["Load = 1", "lOaD = 1", "Unload = 1", "uNlOaD = 1"] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the native contextual destination requires recovery: {language}, {body}"
                );
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "a recovered contextual destination must retain whole-module fallback: {language}, {body}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_statement_heads_never_gain_false_complete_admission() {
        let mut unexpected_admissions = Vec::new();
        for word in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for (shape, body) in [
                ("assignment", format!("{word}=1")),
                ("let_assignment", format!("Let {word}=1")),
                ("variable_declaration", format!("Dim {word} As Long")),
                ("constant_declaration", format!("Const {word} As Long=1")),
                ("value", format!("value={word}")),
                ("call", format!("Call {word}()")),
            ] {
                let source = format!("Sub Run()\n{body}\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(
                        native.traversal.fully_traversed,
                        "the finite head audit must finish traversal: {language}, {shape}, {word}"
                    );
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_some();
                    if matches!(word, "Name" | "Line") && shape != "call" {
                        assert!(
                            admitted,
                            "a supported contextual name stays admitted: {language}, {shape}, {word}"
                        );
                    }
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        if admitted {
                            unexpected_admissions.push(format!("{language}:{shape}:{word}"));
                        }
                    } else {
                        assert_eq!(
                            native.traversal.coverage,
                            CoverageStatus::Complete,
                            "the finite head oracle must not be resource-bounded: {language}, {shape}, {word}"
                        );
                        if admitted {
                            compare_native(&Fixture::new(&source), language, &[]);
                        }
                    }
                }
            }
        }
        assert!(
            unexpected_admissions.is_empty(),
            "native-recovered statement heads must retain whole-module fallback: {unexpected_admissions:?}"
        );
    }

    #[test]
    fn first_party_vba_zero_argument_calls_match_the_private_native_route() {
        let cases = [
            "Sub Run()\nCall Target\nEnd Sub\n",
            "Sub Run()\nCall Target()\nEnd Sub\n",
            "Sub Run()\nCall Target ()\nEnd Sub\n",
            "Sub Run()\ncAlL\tTarget ( \t )\nEnd Sub\n",
            "Sub Run()\nCall [Rem]\nCall [Me]()\nCall [Sub] ()\nCall [True]( )\nEnd Sub\n",
            "Sub Run()\nCall [Name]\nCall [Name]()\nCall [nAmE] ( )\nEnd Sub\n",
            "Sub Run()\nCall Target ' call:\nCall Target() ' parenthesized call\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\"\r\nOption Explicit\r\nPrivate Sub Run() ' header\r\nCall Target() ' call\r\nCall [Rem] ' bare call\r\nEnd Sub ' terminator\r\n",
            "Sub Run():Call Target():Call [Rem]:End Sub\n",
            "Sub Run(): : Call Target(): ' call:\nCall Other\nEnd Sub\n",
            "Sub Run()\nCall _\r\n Target( _\n )\nCall Target _\n ( _\r\n )\nEnd Sub\n",
            "Sub Run()\nDim value As Long\nConst seed As Long = 7\nvalue = seed\nCall Target()\nLet value = 8\nCall [Name]\nEnd Sub\n",
            "Function Result() As Long\nCall Target\nResult = 7\nCall Target()\nEnd Function\n",
        ];
        for source in cases {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_arguments_match_the_private_native_route() {
        for source in [
            "Sub Run()\nCall Target()\nEnd Sub\n",
            "Sub Run()\nCall Target(1)\nEnd Sub\n",
            "Sub Run()\nCall Target(007, False)\nEnd Sub\n",
            "Sub Run()\nCall Target(value, 7, \"a,\"\"b\"\":()\", True, [value])\nEnd Sub\n",
            "Sub Run()\nCall Target(value, value, [value])\nEnd Sub\n",
            "Sub Run()\nCall [Name]([Rem], [Me], [True], [Name], [Sub])\nEnd Sub\n",
            "Sub Run()\ncAlL\t[Target] ( \t[value], fAlSe, \"\" )\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\"\r\nOption Explicit\r\nPrivate Sub Run(ByVal value As Long) ' header\r\nCall Target(value, \"x\") ' call:\r\nEnd Sub ' terminator\r\n",
            "Sub Run():Call Target(1, \"x:y\", True):Call Other(value):End Sub\n",
            "Sub Run()\nCall _\r\n Target( _\n value, _\r\n \"x\", _\n False _\r\n )\nEnd Sub\n",
            "Sub Accept(ByVal first As Long, ByRef second As String)\nfirst = first\nsecond = second\nEnd Sub\nSub Run()\nDim value As Long\nCall Accept(value, \"x\")\nEnd Sub\n",
            "Function Result(item As Long) As Long\nCall Target(item, 1)\nResult = item\nEnd Function\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_argument_positions_never_gain_false_complete_admission() {
        let mut unexpected_admissions = Vec::new();
        for name in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for arguments in [name.to_owned(), format!("1, {name}"), format!("[{name}]")] {
                let source = format!("Sub Run()\nCall Target({arguments})\nEnd Sub\n");
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(native.traversal.fully_traversed);
                    let admitted = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_some();
                    if native.traversal.coverage == CoverageStatus::Unknown {
                        if admitted {
                            unexpected_admissions.push(format!("{language}:{arguments}"));
                        }
                    } else {
                        assert_eq!(
                            native.traversal.coverage,
                            CoverageStatus::Complete,
                            "the argument-position oracle must not be bounded: {language}, {arguments}"
                        );
                        assert!(
                            admitted,
                            "native-complete scalar argument names belong to the closed slice: {language}, {arguments}"
                        );
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        assert!(
            unexpected_admissions.is_empty(),
            "native-recovered argument positions retain whole-module fallback: {unexpected_admissions:?}"
        );
    }

    #[test]
    fn first_party_vba_unsupported_argument_lists_keep_whole_native_fallback() {
        for call in [
            "Call Target 1",
            "Call Target value, 1",
            "Call Target(1 + &H2%%)",
            "Call Target(Nested(1).member)",
            "Call Target(holder.Value)",
            "Call Target(value:=1)",
            "Call Target(1, extra:=2)",
            "Call Target(ByVal value)",
            "Call Target(,1)",
            "Call Target(1,)",
            "Call Target(1,,2)",
            "Call Target(1;2)",
            "Call Target((1)",
            "Call Target(-1.5%)",
            "Call Target(1.5%)",
            "Call Target(#1/2/2000#)",
            "Call Target(1) extra",
            "Call Target(1)(2)",
            "Call Target(1))",
            "Call Target(1",
            "Call Target(\"unclosed)",
            "Call Target(1,\n2)",
        ] {
            let source =
                format!("Sub Valid()\nEnd Sub\nSub Run()\nCall Other(1)\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported argument list cannot publish a supported prefix: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_value_applications_match_the_private_native_route() {
        for source in [
            "Sub Run()\nsink = Buffer()\nEnd Sub\n",
            "Sub Run()\nsink = Buffer(0)\nEnd Sub\n",
            "Sub Run()\nLet sink = Grid(0, index)\nEnd Sub\n",
            "Sub Run()\nsink = Fetch(value, 7, \"x\", value, True)\nEnd Sub\n",
            "Sub Run()\nLet [sink] = [Buffer]([index], \"a\"\"b\")\nEnd Sub\n",
            "Sub Run()\nsink = Buffer ( value )\nEnd Sub\n",
            "Sub Run()\nsink = Fetch(value, True, False, \"a,\"\"b\"\":()\", [value])\nEnd Sub\n",
            "Sub Run()\r\n sink = Buffer( _\r\n 0, index) ' read\r\nEnd Sub\r\n",
            "Sub Run(): sink = Buffer(0): sink = Fetch(): End Sub\n",
            "Dim Buffer(0 To 2) As Long\nFunction Fetch(ByVal value As Long) As Long\nFetch = Buffer(value)\nEnd Function\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        // Statement-head keywords can be ordinary RHS receivers; the exact
        // oracle checks that the separate Call-head exclusion does not leak here.
        for callee in [
            "Name",
            "Line",
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for written in [callee.to_owned(), format!("[{callee}]")] {
                if callee == "Line" && written == callee {
                    continue;
                }
                for arguments in ["", "0", "value"] {
                    let source = format!("Sub Run()\nsink = {written}({arguments})\nEnd Sub\n");
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
    }

    #[test]
    fn first_party_vba_value_applications_keep_whole_native_fallback() {
        for assignment in [
            "sink = Line()",
            "sink = line(0)",
            "Let sink = lInE(value)",
            "sink = Buffer(holder.Value)",
            "sink = holder.Buffer(0)",
            "holder.sink = Buffer(0)",
            "sink(0) = Buffer(0)",
            "Set sink = Buffer(0)",
            "sink = Buffer(index:=0)",
            "sink = Buffer(ByVal index)",
            "sink = Buffer(,0)",
            "sink = Buffer(0,)",
            "sink = Buffer(0,,1)",
            "sink = Buffer(1.5%)",
            "sink = Buffer(0)(1)",
            "sink = Buffer(0) extra",
            "sink = Buffer(0))",
            "sink = Buffer(0",
            "sink = Buffer(\"unclosed)",
        ] {
            let source = format!(
                "Sub Valid()\nEnd Sub\nSub Run()\nsink = Buffer(0)\n{assignment}\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "unsupported RHS syntax cannot publish a complete prefix: {language}, {assignment}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_expression_calls_match_the_private_native_route() {
        for body in [
            "Call Target(value + 1, Inner(other) * -2)",
            "Call Target(1 + 2)",
            "Call Target(Nested(1))",
            "Call Target((1))",
            "Call Target(-1)",
            "Dim value As Long\nCall Run(Nested(1))",
            "Call [Name]([value] + 1)",
            "Call Line(value + 1)",
            "Call Target(Not Not value, --other)",
            "Call Target(Inner(), Inner(value), (value = 1))",
            "Call Target(\"a,\"\"b\"\":()\" & \"x\")",
            "Call Target(-(value + 1), [Line](other) + [Name](2))",
        ] {
            compare_native_procedural_body(body);
        }
        for operator in [
            "+", "-", "*", "/", "\\", "^", "Mod", "&", "=", "<>", "<", "<=", ">", ">=", "Is",
            "Like", "And", "Or", "Xor", "Eqv", "Imp",
        ] {
            for expression in [
                format!("left {operator} right"),
                format!("(left {operator} right)"),
                format!("LeftValue(left) {operator} RightValue(right)"),
            ] {
                compare_native_procedural_body(&format!("Call Target({expression})"));
            }
        }
        for first in ["+", "-", "*", "/", "\\", "^", "Mod", "&"] {
            for second in ["+", "-", "*", "/", "\\", "^", "Mod", "&"] {
                compare_native_procedural_body(&format!(
                    "Call Target(left {first} middle {second} right)"
                ));
            }
        }
        for first in ["And", "Or", "Xor", "Eqv", "Imp"] {
            for second in ["And", "Or", "Xor", "Eqv", "Imp"] {
                compare_native_procedural_body(&format!(
                    "Call Target(left {first} middle {second} right)"
                ));
            }
        }
        for source in [
            "Sub Run()\r\n Call Target( _\r\n value + 007, Inner( _\r\n other) * -2) ' note\r\nEnd Sub\r\n",
            "Sub Run(): Call Target(Inner(value + 1), 2): Call Target(Not False): End Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_expression_call_root_and_tails_keep_whole_native_fallback() {
        for call in [
            "Call Target(1)+2",
            "Call Target(Inner(1))+2",
            "Call (Target(1))",
            "Call Target(1)(2)",
            "Call Target(1) extra",
            "Call Target(1 +)",
            "Call Target(Inner(1).member)",
            "Call Target(Inner(1), extra:=2)",
            "Call Target(Inner(1), ByVal extra)",
            "Call Target(Inner(1),)",
            "Call Target(, Inner(1))",
            "Call Target(1 + &H2%%)",
            "Call Target(1 + 2.0%)",
            "Call Target(2^3)",
            "Call Target(value&other)",
            "Call Target(Line(1) + 1)",
            "Call Name(1 + 2)",
            "Call Target 1 + 2",
            "Call Module.Target(1 + 2)",
            "Call Target(())",
        ] {
            let source =
                format!("Sub Valid()\nEnd Sub\nSub Run()\nCall Other(1 + 2)\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "unsupported root/tail cannot publish an own prefix: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_expression_call_work_and_candidate_limits_are_exact() {
        let source = "Sub Run()\nCall Target(value + 1, Inner(other) * -2)\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.processed_nodes, 16);
            let ModuleBodyStatement::ExpressionCall(call) =
                &prepared.inventory.procedures()[0].statements()[0]
            else {
                panic!("the complete explicit Call retains its expression inventory");
            };
            assert_eq!(call.inventory().work_nodes(), 11);
            assert_eq!(call.inventory().argument_count(), 3);
            assert_eq!(call.argument_expressions().len(), 2);
            assert_eq!(call.inventory().applications().len(), 2);
            for (optional, exact) in [(false, 7), (true, 14)] {
                let all = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(all.len(), exact);
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    7
                );
                for (limit, observed) in [(exact - 1, exact), (0, 7)] {
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            limit,
                            &cancellation,
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed,
                            limit,
                        })
                    );
                }
            }
            for limit in [16, 15, 10] {
                fixture.limits = with_maximum_nodes(&fixture.limits, limit);
                let request = fixture.request(language, &[]);
                let result = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                );
                if limit == 16 {
                    assert_eq!(result.unwrap().unwrap().processed_nodes, 16);
                } else {
                    assert_eq!(
                        result.err().unwrap(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: limit + 1,
                            limit,
                        })
                    );
                }
            }
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            let request = fixture.request(language, &[]);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    100,
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_expressions_match_the_private_native_route() {
        for body in [
            "sink = Buffer(0) + 1",
            "sink = Buffer(Nested(0))",
            "sink = Buffer((0))",
            "sink = Buffer(-1)",
            "Dim value As Long\nvalue = 1 + 2",
            "Dim value As Long\nvalue = -1",
            "sink = -value ^ 2 ^ 3",
            "sink = 2 ^ -value ^ 3",
            "sink = --value + +-2",
            "sink = Not Not value",
            "sink = Not value = 1 And True",
            "sink = Not (value = 1) And True Or False Xor True Eqv False Imp True",
            "sink = (value = 1 And other < 2) Or (value >= 3 And other <> 4)",
            "sink = (value = 1) And (other < 2) Or (value >= 3)",
            "sink = value < 1 Or other > 2 And third < 3",
            "sink = \"a,\"\"b\"\":()\" & \"x\"",
            "sink = Outer(Inner(value + 1), items(-(value - 2))) + value",
            "sink = Outer(Inner(), Inner(value), (value = 1))",
            "sink = [Line](Not value, +2) + [Name](value - 1)",
            "sink = Line + Name * 2",
            "sink=([left]+[right])*+2 ^ 3",
            "sink=\"a\"&\"b\"",
            "sink = [Not] + Notable * Modulus - AndAlso",
        ] {
            compare_native_procedural_body(body);
        }
        for operator in [
            "+", "-", "*", "/", "\\", "^", "Mod", "&", "=", "<>", "<", "<=", ">", ">=", "Is",
            "Like", "And", "Or", "Xor", "Eqv", "Imp",
        ] {
            for expression in [
                format!("left {operator} right"),
                format!("(left {operator} right)"),
                format!("LeftValue(left) {operator} RightValue(right)"),
                format!("LeftValue(left {operator} right) + 1"),
            ] {
                compare_native_procedural_body(&format!("sink = {expression}"));
            }
        }
        // Operator pairs exercise precedence and associativity while the oracle
        // compares every source-bound fact, not just the admitted syntax status.
        for first in ["+", "-", "*", "/", "\\", "^", "Mod", "&"] {
            for second in ["+", "-", "*", "/", "\\", "^", "Mod", "&"] {
                compare_native_procedural_body(&format!(
                    "sink = left {first} middle {second} right"
                ));
            }
        }
        for first in ["And", "Or", "Xor", "Eqv", "Imp"] {
            for second in ["And", "Or", "Xor", "Eqv", "Imp"] {
                compare_native_procedural_body(&format!(
                    "sink = left {first} middle {second} right"
                ));
            }
        }
        for operator in ["And", "Or", "Xor", "Eqv", "Imp"] {
            compare_native_procedural_body(&format!("sink = (left = 1) {operator} (right <> 2)"));
        }
        for operator in ["+", "-", "*", "/", "\\", "=", "<", ">"] {
            compare_native_procedural_body(&format!("sink=Read{operator}(1)"));
        }
        for operator in ["^", "&"] {
            compare_native_procedural_body(&format!("sink=Read {operator}(1)"));
        }
        compare_native_procedural_body("sink = value(1) = 2");
        for source in [
            "Sub Run()\r\n sink = -(value + _\r\n 007) * +2 ^ 3 ^ 4 ' note\r\nEnd Sub\r\n",
            "Sub Run(): sink = Outer(Inner(value + 1), 2): sink = Not False: End Sub\n",
            "Sub Run()\nsink = -(value + 1)\nRem expression comment\nsink = Not False\nEnd Sub\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_comparison_logical_precedence_matches_native_syntax() {
        for body in [
            "sink = value < 1 Or other > 2 And third < 3",
            "sink = (value < 1 Or other > 2 And third < 3)",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Complete,
                    "the native expression is complete: {language}, {body}, {}",
                    native.tree.root_node().to_sexp()
                );
                // A fresh provider ensures the first-party admission assertion
                // cannot inherit pool/cache work from the independent oracle.
                compare_native(&Fixture::new(&source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_expression_tails_keep_whole_native_fallback() {
        for assignment in [
            "sink = 1 + &H2%%",
            "sink = -(value.Member + 1)",
            "sink = Outer(Inner(value), extra:=2)",
            "sink = Outer(Inner(value), ByVal extra)",
            "sink = Outer(Inner(value),)",
            "sink = Outer(, Inner(value))",
            "sink = Outer(Inner(value), #1/2/2000#)",
            "sink = 1 + Line()",
            "sink = 1 + Line(0)",
            "sink = Outer(Line(0))",
            "sink = -(value + 1) extra",
            "sink = -(value + 1",
            "sink = value +",
            "sink = value And Or other",
            "sink = 1 + New Widget",
            "sink = 1 + AddressOf Target",
            "sink = Outer(value + 1)(other)",
            "sink = 2^3",
            "sink = 2&3",
            "sink = value^other",
            "sink = value&other",
        ] {
            let source = format!(
                "Sub Valid()\nEnd Sub\nSub Run()\nsink = -(value + 1)\n{assignment}\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported expression tail cannot publish a supported prefix: {language}, {assignment}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_spec_expression_records_preserve_complete_source_facts() {
        let mut expressions = Vec::new();
        for operator in ["And", "Or", "Xor", "Eqv", "Imp"] {
            expressions.push((
                format!("left < 1 {operator} right > 2"),
                7,
                vec!["left", "right"],
            ));
        }
        expressions.extend([
            ("left < 1 < 2".to_owned(), 5, vec!["left"]),
            ("left = right <> 1".to_owned(), 5, vec!["left", "right"]),
            ("(left = right = 1)".to_owned(), 6, vec!["left", "right"]),
            (
                "left ^ middle ^ right".to_owned(),
                5,
                vec!["left", "middle", "right"],
            ),
        ]);
        for (expression, work_nodes, names) in expressions {
            let source = format!("Sub Run()\nsink = {expression}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .expect("the independently specified operator composition is admitted");
                assert_eq!(prepared.processed_nodes, work_nodes + 5);
                let ModuleBodyStatement::ExpressionAssignment(assignment) =
                    &prepared.inventory.procedures()[0].statements()[0]
                else {
                    panic!("the complete RHS retains its structured expression inventory");
                };
                assert_eq!(&source[assignment.expression()], expression);
                assert_eq!(assignment.inventory().work_nodes(), work_nodes);
                assert_eq!(assignment.inventory().argument_count(), 0);
                assert!(assignment.inventory().applications().is_empty());
                let actual = normalize_query_candidates(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        true,
                        request.limits().max_syntax_nodes(),
                        &cancellation,
                    )
                    .unwrap(),
                    &request,
                    usize::MAX,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(actual.required, 7);
                let declaration = actual
                    .facts
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
                    .unwrap();
                let references = actual
                    .facts
                    .iter()
                    .filter(|fact| fact.kind() == SyntaxFactKind::Occurrence)
                    .collect::<Vec<_>>();
                // The procedure name contributes an occurrence alongside the RHS references.
                assert_eq!(references.len(), names.len() + 2);
                let definitions = references
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == "vba.identifier.definition")
                    .collect::<Vec<_>>();
                assert_eq!(definitions.len(), 1);
                assert_eq!(definitions[0].span().start_byte(), 4);
                assert_eq!(definitions[0].span().end_byte(), 7);
                assert_eq!(definitions[0].parent(), Some(declaration.local_id()));
                assert_eq!(definitions[0].depth(), 5);
                assert_eq!(
                    references
                        .iter()
                        .filter(|fact| {
                            matches!(
                                fact.syntax_kind().as_str(),
                                "vba.let_identifier.reference" | "vba.value_identifier.reference"
                            )
                        })
                        .count(),
                    names.len() + 1,
                );
                for (syntax, name) in std::iter::once(("vba.let_identifier.reference", "sink"))
                    .chain(
                        names
                            .iter()
                            .copied()
                            .map(|name| ("vba.value_identifier.reference", name)),
                    )
                {
                    let start = source.find(name).unwrap();
                    let matches = references
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == syntax
                                && fact.span().start_byte() == u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(start + name.len()).unwrap()
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(matches.len(), 1, "{language}: {expression}: {name}");
                    assert_eq!(matches[0].parent(), Some(declaration.local_id()));
                    assert_eq!(matches[0].depth(), 5);
                    assert_eq!(matches[0].span().file(), fixture.source.span().file());
                }
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_expression_work_and_candidate_limits_are_exact() {
        let source = "Sub Run()\nsink = Outer(value + 1, Inner(other)) * -2\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.processed_nodes, 16);
            let ModuleBodyStatement::ExpressionAssignment(assignment) =
                &prepared.inventory.procedures()[0].statements()[0]
            else {
                panic!("the complete richer RHS uses its expression inventory");
            };
            assert_eq!(assignment.inventory().work_nodes(), 11);
            assert_eq!(assignment.inventory().argument_count(), 3);
            for (optional, exact) in [(false, 7), (true, 15)] {
                let all = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(all.len(), exact);
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    7
                );
                for (limit, observed) in [(exact - 1, exact), (0, 7)] {
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            limit,
                            &cancellation,
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed,
                            limit,
                        })
                    );
                }
            }
            for limit in [16, 15, 10] {
                fixture.limits = with_maximum_nodes(&fixture.limits, limit);
                let request = fixture.request(language, &[]);
                let result = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                );
                if limit == 16 {
                    assert_eq!(result.unwrap().unwrap().processed_nodes, 16);
                } else {
                    assert_eq!(
                        result.err().unwrap(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: limit + 1,
                            limit,
                        })
                    );
                }
            }
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            let request = fixture.request(language, &[]);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    100,
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_value_application_work_and_limits_are_exact() {
        let source = "Sub Run()\nsink = Fetch(value, 7, \"x\", value, True)\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.processed_nodes, 12);
            for (optional, exact) in [(false, 7), (true, 14)] {
                let all = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(all.len(), exact);
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    7
                );
                for (limit, observed) in [(exact - 1, exact), (0, 7)] {
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            request.source().bytes(),
                            optional,
                            limit,
                            &cancellation,
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed,
                            limit,
                        })
                    );
                }
            }
            for limit in [12, 11] {
                fixture.limits = with_maximum_nodes(&fixture.limits, limit);
                let request = fixture.request(language, &[]);
                let result = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                );
                if limit == 12 {
                    assert_eq!(result.unwrap().unwrap().processed_nodes, 12);
                } else {
                    assert!(matches!(
                        result,
                        Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 12,
                            limit: 11,
                        }))
                    ));
                }
            }
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            let request = fixture.request(language, &[]);
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    100,
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_argument_nodes_and_candidate_ceilings_are_exact() {
        let source = "Sub Run()\nCall Target(value, 7, \"x\", value, True)\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.processed_nodes, 10);
            let ModuleBodyStatement::Call(call) =
                &prepared.inventory.procedures()[0].statements()[0]
            else {
                panic!("the authored body contains a Call");
            };
            assert_eq!(call.argument_values().len(), 5);
            for optional in [false, true] {
                let all = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap();
                if optional {
                    assert_eq!(
                        all.iter()
                            .filter(|candidate| candidate.syntax == "vba.string")
                            .count(),
                        1
                    );
                    assert_eq!(
                        all.iter()
                            .filter(|candidate| candidate.syntax == "vba.value_identifier")
                            .count(),
                        2
                    );
                } else {
                    assert!(
                        all.iter()
                            .all(|candidate| candidate.role != StructuralRole::Reference)
                    );
                }
                let exact = all.len();
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    7
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact,
                        &cancellation
                    )
                    .unwrap()
                    .len(),
                    exact
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact - 1,
                        &cancellation
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: exact,
                        limit: exact - 1,
                    })
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        0,
                        &cancellation
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: 7,
                        limit: 0,
                    })
                );
            }
            fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes);
            let request = fixture.request(language, &[]);
            assert_eq!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .unwrap()
                .processed_nodes,
                prepared.processed_nodes
            );
            fixture.limits = with_maximum_nodes(&fixture.limits, prepared.processed_nodes - 1);
            let request = fixture.request(language, &[]);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 10,
                    limit: 9,
                }))
            ));
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    100,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new("Sub Run()\nCall Target(1,2,3,4,5,6,7,8)\nEnd Sub\n");
            fixture.limits = with_maximum_nodes(&fixture.limits, 7);
            let request = fixture.request(language, &[]);
            assert!(matches!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new()
                ),
                Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 8,
                    limit: 7,
                }))
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_contextual_name_keeps_recovered_native_route() {
        for call in ["Call Name", "Call Name()", "Call nAmE ( )"] {
            let source = format!("Sub Run()\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "a recovered contextual callee must retain whole-module fallback: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert!(native.traversal.fully_traversed);
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the native contextual callee requires recovery: {language}, {call}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_contextual_line_matches_the_private_native_route() {
        for language in ["vba", "visual-basic-classic"] {
            compare_native(
                &Fixture::new("Sub Run()\nCall Line\nEnd Sub\n"),
                language,
                &[],
            );
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_underscore_name_matches_the_private_native_route() {
        for language in ["vba", "visual-basic-classic"] {
            compare_native(
                &Fixture::new("Sub Run()\nCall _Target()\nEnd Sub\n"),
                language,
                &[],
            );
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_boundaries_keep_whole_native_fallback() {
        for call in [
            "Call Target(1 +)",
            "Call Target value",
            "Call Target(value:=1)",
            "Call Target(,)",
            "Call Module.Target()",
            "Call .Target()",
            "Call Target() extra",
            "Call Target()()",
            "Call Target(",
            "Call Target)",
            "Call Target(())",
            "Call[Target]",
            "Call_Target",
            "Call\x0cTarget()",
            "Call\nTarget()",
            "Call Target$()",
            "Call Me()",
            "Call Rem()",
            "Call True()",
        ] {
            let source = format!("Sub Run()\nDim value As Long\nvalue = 7\n{call}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "an unsupported call tail retains whole-module native parsing: {language}, {call}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_zero_argument_call_candidate_ceilings_and_cancellation_are_exact() {
        let source = "Sub Run()\nDim value As Long\nCall Target()\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let request = fixture.request(language, &[]);
            let cancellation = Cancellation::new();
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            let identity_candidates = candidates(
                &prepared.inventory,
                request.source().bytes(),
                false,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap();
            assert!(
                identity_candidates
                    .iter()
                    .all(|candidate| candidate.role != StructuralRole::Reference)
            );
            let identity = normalize_query_candidates(
                identity_candidates,
                &request,
                usize::MAX,
                &cancellation,
            )
            .unwrap();
            let full_candidates = candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap();
            let references: Vec<_> = full_candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Reference)
                .map(|candidate| {
                    (
                        source.get(candidate.start..candidate.end).unwrap(),
                        candidate.syntax,
                        candidate.required,
                    )
                })
                .collect();
            assert_eq!(
                references,
                [
                    ("Call Target()", "vba.application", false),
                    ("Target", "vba.invoked_identifier", false),
                ]
            );
            let full =
                normalize_query_candidates(full_candidates, &request, usize::MAX, &cancellation)
                    .unwrap();
            assert_eq!(identity.required, full.required);
            for optional in [false, true] {
                let exact = candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap()
                .len();
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact,
                        &cancellation,
                    )
                    .unwrap()
                    .len(),
                    exact
                );
                assert_eq!(
                    candidates(
                        &prepared.inventory,
                        request.source().bytes(),
                        optional,
                        exact - 1,
                        &cancellation,
                    )
                    .unwrap_err(),
                    AdapterError::RejectedRequest(RequestError::ProviderLimit {
                        resource: ResourceKind::SyntaxNodes,
                        observed: exact,
                        limit: exact - 1,
                    })
                );
            }
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    false,
                    0,
                    &cancellation,
                )
                .unwrap_err(),
                AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 9,
                    limit: 0,
                })
            );
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    true,
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest,
                })
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    fn radix_module_source(literal: &str, newline: &str) -> String {
        format!(
            "Attribute VB_Name = \"ModuleA\"{newline}Const Seed={literal}{newline}Dim GlobalValues(-{literal} To +{literal}, {literal}) As Long{newline}Function Fetch(ByVal item As Long, Optional extra As Long={literal}) As Long{newline}Fetch=item{newline}End Function{newline}Sub Target(ByVal item As Long){newline}End Sub{newline}Sub Run(){newline}Dim sink As Long, localValues(-{literal} To +{literal}, {literal}) As Long{newline}Const Local As Long={literal}{newline}sink={literal}{newline}sink=Fetch(localValues({literal}, {literal}), Seed)+{literal}{newline}Call Target({literal}){newline}Call Target(Fetch({literal})+Local){newline}End Sub{newline}"
        )
    }

    #[test]
    fn first_party_vba_date_controls_keep_complete_native_fallback() {
        for source in [
            "Const Seed=#1/1/2000#\n",
            "Dim values(#1/1/2000#)\n",
            "Sub Run(Optional value As Date=#1/1/2000#)\nEnd Sub\n",
            "Sub Run()\nDim value As Long\nvalue=#1/1/2000#\nEnd Sub\n",
            "Sub Run()\nsink=Buffer(#1/1/2000#)\nEnd Sub\n",
            "Sub Run()\nsink=Inner(local + #1/1/2000#)\nEnd Sub\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = source.replace('\n', newline);
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::with_deadline(
                        std::time::Instant::now() + std::time::Duration::from_secs(60),
                    );
                    assert!(
                        prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap()
                        .is_none()
                    );
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                    let native = fixture
                        .provider
                        .prepare_tree(
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap();
                    assert!(native.traversal.fully_traversed);
                    assert_eq!(
                        native.traversal.coverage,
                        CoverageStatus::Complete,
                        "{language}, {source}"
                    );
                    let pack = fixture
                        .provider
                        .query_packs
                        .get_for_source(native.family, request.source().path().as_str())
                        .unwrap();
                    let extracted = pack
                        .extract(
                            native.family,
                            &native.tree,
                            request.source().bytes(),
                            request.limits().max_syntax_nodes(),
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                    assert!(extracted.limit.is_none());
                    let expected = normalize_query_candidates(
                        extracted.candidates,
                        &request,
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                    let output = rootlight_adapter_sdk::execute_parse(
                        &fixture.provider, &request,
                        rootlight_adapter_sdk::MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        &cancellation,
                    ).unwrap();
                    assert_eq!(
                        output.report().coverage().status(),
                        CoverageStatus::Complete
                    );
                    assert_eq!(
                        output.report().coverage().covered_source_bytes(),
                        source.len()
                    );
                    assert_eq!(output.facts(), expected.facts);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_float_tokens_match_complete_compatible_native_facts() {
        for atom in ["1.5", ".5", "1.", "1E2", "1e-2", "1.E+2"] {
            for suffix in ["", "!", "#", "@"] {
                let literal = format!("{atom}{suffix}");
                for newline in ["\n", "\r\n"] {
                    let source = radix_module_source(&literal, newline);
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for body in [
            "Call Target(-1.5)",
            "Call Target(1.5)",
            "Call Target(1 + 2.0)",
            "sink = Buffer(1.5)",
            "Dim first, last(1.5)",
            "Dim value As Long\nvalue = 1.5",
        ] {
            compare_native_procedural_body(body);
        }
        for source in [
            "Sub Run(Optional value=1.5)\nEnd Sub\n",
            "Sub Run(Optional value As Double=1.5)\nEnd Sub\n",
            "Const Value=1.5\n",
            "Dim items(1.5)\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
        for literal in ["1.5", ".5#", "1E2", "1e-2@", "7!", "7#"] {
            for body in [
                format!("sink={literal} ^ 3"),
                format!("sink={literal} & \"x\""),
                format!("sink={literal}+3"),
                format!("Call Target({literal}+3)"),
            ] {
                compare_native_procedural_body(&body);
            }
        }
        for body in ["sink=1.5!^2", "sink=7#&8"] {
            compare_native_procedural_body(body);
        }
    }

    #[test]
    fn first_party_vba_float_metadata_and_exponents_preserve_fact_work() {
        use rootlight_vba::ModuleFloatSuffix;

        // Equal-width spelling changes preserve structural facts, not content hashes;
        // D/d has direct own-source evidence rather than a native grammar oracle.
        for newline in ["\n", "\r\n"] {
            let baseline = radix_module_source("00007", newline);
            for language in ["vba", "visual-basic-classic"] {
                let baseline_fixture = Fixture::new(&baseline);
                let baseline_request = baseline_fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let baseline_prepared = prepare(
                    &baseline_fixture.provider,
                    &baseline_request,
                    None,
                    &[],
                    baseline_fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                for (literal, suffix) in [
                    ("0.125", None),
                    (".1250", None),
                    ("0.12!", Some(ModuleFloatSuffix::Exclamation)),
                    ("0.12#", Some(ModuleFloatSuffix::Hash)),
                    ("0.12@", Some(ModuleFloatSuffix::At)),
                    ("01E+2", None),
                    ("01e-2", None),
                    ("01D+2", None),
                    ("01d-2", None),
                    ("0007!", Some(ModuleFloatSuffix::Exclamation)),
                    ("0007#", Some(ModuleFloatSuffix::Hash)),
                    ("0007@", Some(ModuleFloatSuffix::At)),
                    ("1.E2!", Some(ModuleFloatSuffix::Exclamation)),
                    (".1D2@", Some(ModuleFloatSuffix::At)),
                    ("1D+2#", Some(ModuleFloatSuffix::Hash)),
                    ("0001.", None),
                ] {
                    let source = radix_module_source(literal, newline);
                    assert_eq!(source.len(), baseline.len());
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let prepared = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .unwrap();
                    assert_eq!(prepared.processed_nodes, baseline_prepared.processed_nodes);
                    assert_eq!(
                        prepared.inventory.statement_count(),
                        baseline_prepared.inventory.statement_count()
                    );
                    let run = &prepared.inventory.procedures()[2];
                    let ModuleBodyStatement::Declaration(local) = &run.statements()[1] else {
                        panic!("floating-point local constant remains atomic");
                    };
                    let ModuleBodyStatement::Assignment(assignment) = &run.statements()[2] else {
                        panic!("floating-point assignment remains atomic");
                    };
                    let ModuleBodyStatement::ExpressionAssignment(expression) =
                        &run.statements()[3]
                    else {
                        panic!("composed floating-point assignment retains expression evidence");
                    };
                    let ModuleBodyStatement::Call(call) = &run.statements()[4] else {
                        panic!("floating-point argument retains the atomic Call route");
                    };
                    let ModuleBodyStatement::ExpressionCall(expression_call) = &run.statements()[5]
                    else {
                        panic!("composed floating-point Call retains expression evidence");
                    };
                    let mut values = vec![
                        prepared.inventory.constants()[0].value(),
                        prepared.inventory.procedures()[0].parameters()[1]
                            .default_value()
                            .unwrap(),
                        local.value().unwrap(),
                        assignment.value(),
                        &call.argument_values()[0],
                    ];
                    values.extend(expression.inventory().values());
                    values.extend(expression_call.inventory().values());
                    let mut numeric = 0;
                    let mut names = 0;
                    for value in values {
                        assert_eq!(value.integer_suffix(), None);
                        match value.kind() {
                            ModuleScalarValueKind::UnsignedFloat => {
                                numeric += 1;
                                assert_eq!(value.float_suffix(), suffix);
                                assert_eq!(&source[value.span()], literal);
                                if let Some(suffix) = suffix {
                                    assert_eq!(literal.chars().last(), Some(suffix.as_char()));
                                }
                            }
                            ModuleScalarValueKind::Name => {
                                names += 1;
                                assert_eq!(value.float_suffix(), None);
                            }
                            ModuleScalarValueKind::UnsignedDecimal
                            | ModuleScalarValueKind::UnsignedHexadecimal
                            | ModuleScalarValueKind::UnsignedOctal
                            | ModuleScalarValueKind::String
                            | ModuleScalarValueKind::Boolean => {
                                panic!("fixture has only FLOAT atoms and two name reads")
                            }
                        }
                    }
                    assert_eq!((numeric, names), (9, 2));
                    for optional in [false, true] {
                        let actual = candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        let expected = candidates(
                            &baseline_prepared.inventory,
                            baseline.as_bytes(),
                            optional,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        assert_eq!(actual, expected, "{language}, {literal}");
                        let actual =
                            normalize_query_candidates(actual, &request, usize::MAX, &cancellation)
                                .unwrap();
                        let expected = normalize_query_candidates(
                            expected,
                            &baseline_request,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        assert_eq!(actual.required, expected.required);
                        assert_eq!(actual.facts, expected.facts);
                    }
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_float_operators_are_not_integer_type_suffixes() {
        // The retained native number regex accepts INTEGER suffixes on FLOAT;
        // it is not an oracle for these authored operator boundaries.
        for (literal, operator, right) in [
            ("1.5", "^", "2"),
            ("1D2", "^", "2"),
            ("1.5", "&", "8"),
            ("1d2", "&", "8"),
        ] {
            for language in ["vba", "visual-basic-classic"] {
                let mut work = None;
                for spacing in ["", " "] {
                    let source = format!(
                        "Sub Run()\nsink={literal}{spacing}{operator}{spacing}{right}\nEnd Sub\n"
                    );
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let cancellation = Cancellation::new();
                    let prepared = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .unwrap();
                    let ModuleBodyStatement::ExpressionAssignment(assignment) =
                        &prepared.inventory.procedures()[0].statements()[0]
                    else {
                        panic!("FLOAT power or concatenation retains a complete expression");
                    };
                    assert_eq!(assignment.inventory().work_nodes(), 3);
                    assert_eq!(assignment.inventory().values().len(), 2);
                    assert!(assignment.inventory().applications().is_empty());
                    let values = assignment.inventory().values();
                    assert_eq!(values[0].kind(), ModuleScalarValueKind::UnsignedFloat);
                    assert_eq!(&source[values[0].span()], literal);
                    assert_eq!(values[1].kind(), ModuleScalarValueKind::UnsignedDecimal);
                    assert_eq!(&source[values[1].span()], right);
                    for value in values {
                        assert_eq!(value.integer_suffix(), None);
                        assert_eq!(value.float_suffix(), None);
                    }
                    let captures = candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        true,
                        usize::MAX,
                        &cancellation,
                    )
                    .unwrap();
                    let references = captures
                        .iter()
                        .filter(|capture| capture.role == StructuralRole::Reference)
                        .collect::<Vec<_>>();
                    assert_eq!(references.len(), 1);
                    assert_eq!(&source[references[0].start..references[0].end], "sink");
                    if let Some(previous) = work {
                        assert_eq!(prepared.processed_nodes, previous);
                    }
                    work = Some(prepared.processed_nodes);
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_float_tails_reject_the_whole_module() {
        for literal in [
            "1E",
            "1D+",
            ".5e-",
            ".E2",
            "1..2",
            "1.2.3",
            "1E2D3",
            "1.5%",
            "1E2^",
            "1#!",
            "1@@",
            "1.5$",
            "&H1#",
            "&O7!",
            "1.5member",
            "1.5&",
        ] {
            for body in [
                format!("sink={literal}"),
                format!("Call Target({literal})"),
                format!("Dim values({literal})"),
                format!("Const Local As Long={literal}"),
            ] {
                let source = format!(
                    "Const Accepted=1.5\nSub First()\nEnd Sub\nSub Run()\nDim retained As Long\n{body}\nEnd Sub\n"
                );
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    assert!(
                        prepare(
                            &fixture.provider,
                            &fixture.request(language, &[]),
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &Cancellation::new(),
                        )
                        .unwrap()
                        .is_none(),
                        "{language}, {body}"
                    );
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_integer_suffixes_match_complete_native_source_facts() {
        for atom in ["0", "007", "32767", "&H0", "&hA", "&H7FFF"] {
            for suffix in ['%', '&', '^'] {
                let literal = format!("{atom}{suffix}");
                for newline in ["\n", "\r\n"] {
                    let source = radix_module_source(&literal, newline);
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for body in [
            "sink = 1 + &H2%",
            "Call Target(1 + &H2%)",
            "Dim first, last(&H2%)",
            "Dim first, last(2&)",
            "Dim value As Long\nvalue = &H10%",
        ] {
            compare_native_procedural_body(body);
        }
        for source in [
            "Sub Run(Optional value=1%)\nEnd Sub\n",
            "Sub Run(Optional value As Long=&H10%)\nEnd Sub\n",
            "Const Value=&H10%\n",
            "Dim items(&HFF%)\n",
            "Dim items(2&)\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_integer_suffixes_retain_metadata_without_new_numeric_facts() {
        use rootlight_vba::ModuleIntegerSuffix;

        // Equal-width source tokens keep ordered structural spans comparable;
        // spelling metadata does not justify numeric references or runtime types.
        for newline in ["\n", "\r\n"] {
            let baseline = radix_module_source("00007", newline);
            for language in ["vba", "visual-basic-classic"] {
                let baseline_fixture = Fixture::new(&baseline);
                let baseline_request = baseline_fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let baseline_prepared = prepare(
                    &baseline_fixture.provider,
                    &baseline_request,
                    None,
                    &[],
                    baseline_fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                for atom in ["0007", "&H07", "&h07", "&O07", "&o07", "&007"] {
                    for suffix in [
                        ModuleIntegerSuffix::Percent,
                        ModuleIntegerSuffix::Ampersand,
                        ModuleIntegerSuffix::Caret,
                    ] {
                        let literal = format!("{atom}{}", suffix.as_char());
                        let source = radix_module_source(&literal, newline);
                        assert_eq!(source.len(), baseline.len());
                        let fixture = Fixture::new(&source);
                        let request = fixture.request(language, &[]);
                        let prepared = prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &cancellation,
                        )
                        .unwrap()
                        .unwrap();
                        assert_eq!(prepared.processed_nodes, baseline_prepared.processed_nodes);
                        assert_eq!(
                            prepared.inventory.statement_count(),
                            baseline_prepared.inventory.statement_count()
                        );
                        let run = &prepared.inventory.procedures()[2];
                        let ModuleBodyStatement::Declaration(local) = &run.statements()[1] else {
                            panic!("the authored local constant remains atomic");
                        };
                        let ModuleBodyStatement::Assignment(assignment) = &run.statements()[2]
                        else {
                            panic!("the authored scalar assignment remains atomic");
                        };
                        let ModuleBodyStatement::ExpressionAssignment(expression) =
                            &run.statements()[3]
                        else {
                            panic!("the composed assignment keeps expression records");
                        };
                        let ModuleBodyStatement::Call(call) = &run.statements()[4] else {
                            panic!("the typed atomic argument preserves the atomic Call API");
                        };
                        let ModuleBodyStatement::ExpressionCall(expression_call) =
                            &run.statements()[5]
                        else {
                            panic!("the composed Call keeps expression records");
                        };
                        let mut values = vec![
                            prepared.inventory.constants()[0].value(),
                            prepared.inventory.procedures()[0].parameters()[1]
                                .default_value()
                                .unwrap(),
                            local.value().unwrap(),
                            assignment.value(),
                            &call.argument_values()[0],
                        ];
                        values.extend(expression.inventory().values());
                        values.extend(expression_call.inventory().values());
                        let mut numeric = 0;
                        let mut names = 0;
                        for value in values {
                            match value.kind() {
                                ModuleScalarValueKind::UnsignedDecimal
                                | ModuleScalarValueKind::UnsignedHexadecimal
                                | ModuleScalarValueKind::UnsignedOctal => {
                                    numeric += 1;
                                    assert_eq!(value.integer_suffix(), Some(suffix));
                                    assert_eq!(&source[value.span()], literal);
                                }
                                ModuleScalarValueKind::Name => {
                                    names += 1;
                                    assert_eq!(value.integer_suffix(), None);
                                }
                                ModuleScalarValueKind::UnsignedFloat
                                | ModuleScalarValueKind::String
                                | ModuleScalarValueKind::Boolean => {
                                    panic!(
                                        "the written fixture has only numeric atoms and two name reads"
                                    );
                                }
                            }
                        }
                        assert_eq!((numeric, names), (9, 2));
                        for optional in [false, true] {
                            let actual = candidates(
                                &prepared.inventory,
                                source.as_bytes(),
                                optional,
                                usize::MAX,
                                &cancellation,
                            )
                            .unwrap();
                            let expected = candidates(
                                &baseline_prepared.inventory,
                                baseline.as_bytes(),
                                optional,
                                usize::MAX,
                                &cancellation,
                            )
                            .unwrap();
                            assert_eq!(actual, expected, "{language}, {literal}");
                            let actual = normalize_query_candidates(
                                actual,
                                &request,
                                usize::MAX,
                                &cancellation,
                            )
                            .unwrap();
                            let expected = normalize_query_candidates(
                                expected,
                                &baseline_request,
                                usize::MAX,
                                &cancellation,
                            )
                            .unwrap();
                            assert_eq!(actual.required, expected.required);
                            assert_eq!(actual.facts, expected.facts);
                        }
                        assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                        assert_eq!(fixture.provider.stats().cache.entries, 0);
                    }
                }
            }
        }
    }

    #[test]
    fn first_party_vba_integer_suffix_boundaries_preserve_operators_and_whole_module_closure() {
        for literal in ["2%", "2&", "2^", "&H7%", "&h7&", "&H7^"] {
            for body in [
                format!("sink={literal} ^ 3"),
                format!("sink={literal} & \"x\""),
                format!("sink={literal}+3"),
                format!("Call Target({literal}+3)"),
            ] {
                compare_native_procedural_body(&body);
            }
        }
        for literal in [
            "2%%",
            "2&&",
            "2^^",
            "2%&",
            "2&^",
            "2^%",
            "2!!",
            "2##",
            "2@@",
            "2$",
            "2^2",
            "2&2",
            "&H7%member",
            "&H%%",
            "&77%z",
            "&O8%",
            "&8&",
        ] {
            for body in [
                format!("sink={literal}"),
                format!("Call Target({literal})"),
                format!("Dim values({literal})"),
                format!("Const Local As Long={literal}"),
            ] {
                let source = format!(
                    "Const Accepted=2%\nSub First()\nEnd Sub\nSub Run()\nDim retained As Long\n{body}\nEnd Sub\n"
                );
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    assert!(
                        prepare(
                            &fixture.provider,
                            &fixture.request(language, &[]),
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &Cancellation::new(),
                        )
                        .unwrap()
                        .is_none(),
                        "{language}, {body}"
                    );
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_hex_atoms_and_bounds_match_complete_native_source_facts() {
        for literal in [
            "&H0",
            "&h7",
            "&H0f",
            "&hAb",
            "&H7FFF",
            "&H8000",
            "&HFFFF",
            "&HFFFFFFFF",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = radix_module_source(literal, newline);
                for language in ["vba", "visual-basic-classic"] {
                    compare_native(&Fixture::new(&source), language, &[]);
                }
            }
        }
        let magnitude = format!("&H{}", "A".repeat(2048));
        for language in ["vba", "visual-basic-classic"] {
            compare_native(
                &Fixture::new(&radix_module_source(&magnitude, "\n")),
                language,
                &[],
            );
        }
        for body in [
            "sink = 1 + &H2",
            "Call Target(1 + &H2)",
            "Dim first, last(&H2)",
            "Dim value As Long\nvalue = &H10",
        ] {
            compare_native_procedural_body(body);
        }
        for source in [
            "Sub Run(Optional value As Long = &H10)\nEnd Sub\n",
            "Const Value = &H10\n",
            "Dim items(&HFF)\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                compare_native(&Fixture::new(source), language, &[]);
            }
        }
    }

    #[test]
    fn first_party_vba_radix_spelling_does_not_fabricate_numeric_reference_facts() {
        // These equal-width tokens change source identity, not declaration,
        // call or value-read ranges. Octal is proved directly: the retained
        // native grammar does not admit every normative octal spelling.
        for newline in ["\n", "\r\n"] {
            let decimal = radix_module_source("0007", newline);
            for language in ["vba", "visual-basic-classic"] {
                let decimal_fixture = Fixture::new(&decimal);
                let cancellation = Cancellation::new();
                let decimal_request = decimal_fixture.request(language, &[]);
                let decimal_prepared = prepare(
                    &decimal_fixture.provider,
                    &decimal_request,
                    None,
                    &[],
                    decimal_fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                for literal in ["&H07", "&h07", "&O07", "&o07", "&007"] {
                    let source = radix_module_source(literal, newline);
                    assert_eq!(source.len(), decimal.len());
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    let prepared = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .unwrap();
                    assert_eq!(prepared.processed_nodes, decimal_prepared.processed_nodes);
                    assert_eq!(
                        prepared.inventory.statement_count(),
                        decimal_prepared.inventory.statement_count(),
                    );
                    for optional in [false, true] {
                        let actual = candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        let expected = candidates(
                            &decimal_prepared.inventory,
                            decimal.as_bytes(),
                            optional,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        assert_eq!(actual, expected, "{language}, {literal}");
                        assert!(!actual.iter().any(|candidate| {
                            candidate.role == StructuralRole::Reference
                                && source.get(candidate.start..candidate.end) == Some(literal)
                        }));
                        let normalized =
                            normalize_query_candidates(actual, &request, usize::MAX, &cancellation)
                                .unwrap();
                        let decimal_normalized = normalize_query_candidates(
                            expected,
                            &decimal_request,
                            usize::MAX,
                            &cancellation,
                        )
                        .unwrap();
                        assert_eq!(normalized.required, decimal_normalized.required);
                        assert_eq!(normalized.facts, decimal_normalized.facts);
                        assert!(normalized.facts.iter().all(|fact| {
                            fact.span().file() == fixture.source.span().file()
                                && usize::try_from(fact.span().end_byte()).unwrap() <= source.len()
                        }));
                    }
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
    }

    #[test]
    fn first_party_vba_numeric_array_work_and_candidate_ceilings_are_conserved() {
        for literal in [
            "0007", "&H07", "&h07", "&O07", "&o07", "&007", "0007%", "&H07%", "&h07%", "&O07%",
            "&o07%", "&007%", "0007&", "&H07&", "&h07&", "&O07&", "&o07&", "&007&", "0007^",
            "&H07^", "&h07^", "&O07^", "&o07^", "&007^", "1.5", ".5", "1.", "1E2", "1e-2", "1D2",
            "1d+2", "1.5!", "1.5#", "1.5@", "7!", "7#", "7@",
        ] {
            let source = format!(
                "Sub Run()\nDim first, grid(-{literal} To +{literal}, {literal}) As Long, pending()\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                let mut fixture = Fixture::new(&source);
                let cancellation = Cancellation::new();
                let request = fixture.request(language, &[]);
                let prepared = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap()
                .unwrap();
                assert_eq!(prepared.processed_nodes, 16);
                let ModuleBodyStatement::Variables(list) =
                    &prepared.inventory.procedures()[0].statements()[0]
                else {
                    panic!("complete local array declaration expected");
                };
                assert_eq!(list.variables()[1].array_work_nodes(), 8);
                assert_eq!(list.variables()[2].array_work_nodes(), 1);
                for (optional, exact) in [(false, 13), (true, 17)] {
                    let all = candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        optional,
                        exact,
                        &cancellation,
                    )
                    .unwrap();
                    assert_eq!(all.len(), exact);
                    assert_eq!(
                        normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                            .unwrap()
                            .required,
                        13,
                    );
                    for (limit, observed) in [(exact - 1, exact), (0, 13)] {
                        assert_eq!(
                            candidates(
                                &prepared.inventory,
                                source.as_bytes(),
                                optional,
                                limit,
                                &cancellation,
                            )
                            .unwrap_err(),
                            AdapterError::RejectedRequest(RequestError::ProviderLimit {
                                resource: ResourceKind::SyntaxNodes,
                                observed,
                                limit,
                            }),
                        );
                    }
                }
                for limit in [16, 15, 1] {
                    fixture.limits = with_maximum_nodes(&fixture.limits, limit);
                    let request = fixture.request(language, &[]);
                    let result = prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    );
                    if limit == 16 {
                        assert_eq!(result.unwrap().unwrap().processed_nodes, 16);
                    } else {
                        assert_eq!(
                            result.err().unwrap(),
                            AdapterError::RejectedRequest(RequestError::ProviderLimit {
                                resource: ResourceKind::SyntaxNodes,
                                observed: limit + 1,
                                limit,
                            }),
                        );
                    }
                }
                assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
                assert!(matches!(
                    candidates(
                        &prepared.inventory,
                        source.as_bytes(),
                        true,
                        100,
                        &cancellation,
                    ),
                    Err(AdapterError::Cancelled {
                        reason: rootlight_cancel::CancellationReason::ClientRequest
                    }),
                ));
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_radix_tails_never_publish_an_admitted_module_prefix() {
        for literal in [
            "&H",
            "&hG",
            "&O",
            "&o8",
            "&8",
            "&77%%",
            "&H7&&",
            "&o7^^",
            "&H7!",
            "&O7@",
            "&7#",
            "&H7$",
            "&H7.member",
            "&O7z",
            "&H 7",
            "& _\n77",
        ] {
            for body in [
                format!("sink={literal}"),
                format!("Call Target({literal})"),
                format!("sink=Outer({literal}+1)"),
                format!("Dim values({literal})"),
                format!("Const Local As Long={literal}"),
            ] {
                let source = format!(
                    "Const Accepted=&H7\nSub First()\nEnd Sub\nSub Run()\nDim retained As Long\n{body}\nEnd Sub\n"
                );
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    assert!(
                        prepare(
                            &fixture.provider,
                            &request,
                            None,
                            &[],
                            fixture.provider.config.default_settings(),
                            &Cancellation::new(),
                        )
                        .unwrap()
                        .is_none(),
                        "unsupported radix tail must retain whole-module fallback: {language}, {body:?}"
                    );
                    assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                    assert_eq!(fixture.provider.stats().cache.entries, 0);
                }
            }
        }
        for literal in ["&77", "&O7", "&H1"] {
            for language in ["vba", "visual-basic-classic"] {
                let source = format!("Sub Run()\nsink=[value] {literal}\nEnd Sub\n");
                let fixture = Fixture::new(&source);
                assert!(
                    prepare(
                        &fixture.provider,
                        &fixture.request(language, &[]),
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                );
            }
        }
        for body in [
            "sink=[value] & 77",
            "sink=[value] & &77",
            "sink=[value] & &H1",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                let source = format!("Sub Run()\n{body}\nEnd Sub\n");
                let fixture = Fixture::new(&source);
                assert!(
                    prepare(
                        &fixture.provider,
                        &fixture.request(language, &[]),
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_some(),
                );
            }
        }
    }

    #[test]
    fn first_party_vba_local_variable_lists_and_arrays_match_exact_native_facts() {
        for ty in [
            "",
            " As String",
            " As Boolean",
            " As Byte",
            " As Integer",
            " As Long",
            " As LongLong",
            " As LongPtr",
            " As Single",
            " As Double",
            " As Currency",
            " As Date",
            " As Variant",
            " As Object",
        ] {
            for bounds in [
                "",
                "2",
                "+2",
                "-2",
                "-2 To +3",
                "0 To 2, 1 To 3",
                "2,3,4",
                "- _\n 2",
            ] {
                for newline in ["\n", "\r\n"] {
                    let bounds = bounds.replace('\n', newline);
                    let source = format!(
                        "Sub Run(){newline}Dim [first], second{ty}, grid({bounds}) As Long, pending(){newline}End Sub{newline}"
                    );
                    for language in ["vba", "visual-basic-classic"] {
                        compare_native(&Fixture::new(&source), language, &[]);
                    }
                }
            }
        }
        for body in [
            "Dim value",
            "Dim values(1) As Long",
            "Dim first As Long, second As Long",
            "Dim first, [FIRST](2), last As Long",
            "Dim [True], [Rem](), Name(2), Line(1To2)As Long",
            "Dim first, Rem2, Remember, [Dim](2), [ReDim]()",
            "Dim first:Dim last():Call Target(first):End Sub\nSub NextProcedure()",
            "Dim _\r\n first _\n, _\r\n[second] _\n( _\r\n-1 _\nTo +2,3) _\r\nAs _\nVariant, last ' trailing",
            "Dim grid(1)As Long, pending(1To -2), first(1 To-2), last(1To+2)",
            "Dim first, grid(2)\nConst Seed As Long=7\nfirst=Seed\nCall Target(grid(1), first+1)",
        ] {
            compare_native_procedural_body(body);
        }
        let declarations = (0..128)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join(",");
        compare_native_procedural_body(&format!("Dim {declarations}"));
        let magnitude = "9".repeat(2048);
        compare_native_procedural_body(&format!(
            "Dim first, grid(-{magnitude} To +{magnitude}, {magnitude}) As Long"
        ));
        let source = "Dim accepted, earlier(2)\nSub First()\nEnd Sub\nDim first, second\nSub Run()\nDim local, other As Long\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            compare_native(&Fixture::new(source), language, &[]);
        }
    }

    #[test]
    fn first_party_vba_local_list_late_invalid_members_keep_whole_native_fallback() {
        for declaration in [
            "Dim first,",
            "Dim first,,last",
            "Dim ,last",
            "Dim first, last As",
            "Dim first, last As Product",
            "Dim first, last As New Object",
            "Dim first, last As String * 3",
            "Dim first, last$",
            "Dim first, last=1",
            "Dim first, last(1+2)",
            "Dim first, last(Limit)",
            "Dim first, last(&H2%%)",
            "Dim first, last(2&&)",
            "Dim first, last(1.5%)",
            "Dim first, last(,2)",
            "Dim first, last(2,)",
            "Dim first, last(2,,3)",
            "Dim first, last(1 To)",
            "Dim first, last((2))",
            "Dim first, last(2))",
            "Dim first, last() As Long()",
            "Dim first, last.member",
            "Dim first, last trailing",
            "Dim first, True",
            "Dim first, Rem",
            "Static first, last",
            "ReDim first(2), last(3)",
            "Private first, last",
            "Dim first, last\0",
            "Dim first, last\rname",
        ] {
            let source = format!(
                "Sub Accepted()\nDim earlier, values(2) As Long\nEnd Sub\nSub Run()\nDim retained\n{declaration}\nEnd Sub\n"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                assert!(
                    prepare(
                        &fixture.provider,
                        &fixture.request(language, &[]),
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "unsupported late member cannot publish an own prefix: {language}, {declaration:?}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_local_list_work_and_candidate_limits_are_exact() {
        let source = "Sub Run()\nDim first, grid(-1 To +2, 3) As Long, pending()\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut fixture = Fixture::new(source);
            let cancellation = Cancellation::new();
            let request = fixture.request(language, &[]);
            let prepared = prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.inventory.statement_count(), 3);
            assert_eq!(prepared.processed_nodes, 16);
            let ModuleBodyStatement::Variables(declaration) =
                &prepared.inventory.procedures()[0].statements()[0]
            else {
                panic!("complete local variable list expected");
            };
            assert_eq!(declaration.variables().len(), 3);
            assert_eq!(declaration.variables()[0].type_range(), None);
            assert_eq!(declaration.variables()[1].array_work_nodes(), 8);
            assert_eq!(declaration.variables()[2].array_work_nodes(), 1);
            for (optional, exact) in [(false, 13), (true, 17)] {
                let all = candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(all.len(), exact);
                assert_eq!(
                    normalize_query_candidates(all, &request, usize::MAX, &cancellation)
                        .unwrap()
                        .required,
                    13,
                );
                for (limit, observed) in [(exact - 1, exact), (0, 13)] {
                    assert_eq!(
                        candidates(
                            &prepared.inventory,
                            source.as_bytes(),
                            optional,
                            limit,
                            &cancellation,
                        )
                        .unwrap_err(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed,
                            limit,
                        }),
                    );
                }
            }
            for limit in [16, 15, 1] {
                fixture.limits = with_maximum_nodes(&fixture.limits, limit);
                let request = fixture.request(language, &[]);
                let result = prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                );
                if limit == 16 {
                    assert_eq!(result.unwrap().unwrap().processed_nodes, 16);
                } else {
                    assert_eq!(
                        result.err().unwrap(),
                        AdapterError::RejectedRequest(RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: limit + 1,
                            limit,
                        }),
                    );
                }
            }
            assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
            assert!(matches!(
                candidates(
                    &prepared.inventory,
                    source.as_bytes(),
                    true,
                    100,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: rootlight_cancel::CancellationReason::ClientRequest
                }),
            ));
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
            assert_eq!(fixture.provider.stats().cache.entries, 0);
        }
    }

    #[test]
    fn first_party_vba_scalar_body_boundaries_keep_whole_native_fallback() {
        for body in [
            "Dim value As Long\nCall Run(Nested(1).member)",
            "Dim value As Long\nRun value",
            "Dim value As Long\nholder.Value = 1",
            "Dim values(Limit) As Long",
            "Dim first As Long, second As UserType",
            "Dim value$",
            "Dim value As UserType",
            "Const seed As Long = value",
            "Dim value As Long\nvalue = #1/1/2000#",
            "Dim value As Long\nvalue = &H10%%",
            "Dim value As Long\nvalue = Nothing",
            "Dim value As Long\nvalue = Me",
            "Dim value As Long\nMe = value",
            "Dim value As Long\nIf True Then\nvalue = 1\nEnd If",
            "Dim value As Long\nvalue = \"unterminated _\n",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(&source);
                let request = fixture.request(language, &[]);
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_none(),
                    "the entire unsupported body retains native parsing: {language}, {body}"
                );
                assert_eq!(fixture.provider.stats().pooled_parsers, 0);
                assert_eq!(fixture.provider.stats().cache.entries, 0);
            }
        }
    }

    #[test]
    fn first_party_vba_scalar_candidate_ceilings_and_cancellation_are_exact() {
        let fixture = Fixture::new(
            "Sub Run()\nDim value As Long\nConst seed As Long = 7\nvalue = seed\nEnd Sub\n",
        );
        let request = fixture.request("vba", &[]);
        let cancellation = Cancellation::new();
        let prepared = prepare(
            &fixture.provider,
            &request,
            None,
            &[],
            fixture.provider.config.default_settings(),
            &cancellation,
        )
        .unwrap()
        .unwrap();
        for optional in [false, true] {
            let exact = candidates(
                &prepared.inventory,
                request.source().bytes(),
                optional,
                request.limits().max_syntax_nodes(),
                &cancellation,
            )
            .unwrap()
            .len();
            assert!(exact > 1);
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact,
                    &cancellation,
                )
                .unwrap()
                .len(),
                exact
            );
            assert_eq!(
                candidates(
                    &prepared.inventory,
                    request.source().bytes(),
                    optional,
                    exact - 1,
                    &cancellation,
                )
                .unwrap_err(),
                AdapterError::RejectedRequest(RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: exact,
                    limit: exact - 1,
                })
            );
        }
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            candidates(
                &prepared.inventory,
                request.source().bytes(),
                true,
                request.limits().max_syntax_nodes(),
                &cancellation,
            ),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest,
            })
        ));
        assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        assert_eq!(fixture.provider.stats().cache.entries, 0);
    }

    #[test]
    fn first_party_vba_admitted_ascii_edges_match_the_private_native_route() {
        const MODULE: &str = "Sub Run()\nEnd Sub\n";
        for language in ["vba", "visual-basic-classic"] {
            let mut admitted = 0_usize;
            for byte in 0_u8..=127 {
                let edge = char::from(byte);
                for source in [format!("{edge}{MODULE}"), format!("{MODULE}{edge}")] {
                    let fixture = Fixture::new(&source);
                    let request = fixture.request(language, &[]);
                    // Unsupported input retains the native route; only complete admission
                    // creates a first-party/native equivalence obligation.
                    if prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &Cancellation::new(),
                    )
                    .unwrap()
                    .is_some()
                    {
                        admitted += 1;
                        compare_native(&fixture, language, &[]);
                    }
                }
            }
            assert!(admitted > 0, "the ASCII matrix must exercise admission");
        }
    }

    #[test]
    fn first_party_vba_admitted_names_and_suffixes_match_the_private_native_route() {
        let mut sources = Vec::new();
        for name in [
            "Run",
            "_",
            "_Run",
            "Rem",
            "rem",
            "rEm",
            "Remnant",
            "Reminder",
            "Sub",
            "Function",
            "End",
            "Name",
            "Public",
            "Option",
            "String",
            "True",
            "Nothing",
            "[Rem]",
            "[Sub]",
            "[Function]",
            "[End]",
            "[Name]",
            "[_]",
        ] {
            for kind in ["Sub", "Function"] {
                for parameters in ["", "()", " ()"] {
                    sources.push(format!("{kind} {name}{parameters}\nEnd {kind}\n"));
                }
            }
        }
        for name in ["Result", "Rem"] {
            for suffix in ['%', '&', '^', '!', '#', '@', '$'] {
                sources.push(format!("Function {name}{suffix}()\nEnd Function\n"));
            }
        }
        for language in ["vba", "visual-basic-classic"] {
            let mut admitted = 0_usize;
            for source in &sources {
                let fixture = Fixture::new(source);
                let request = fixture.request(language, &[]);
                if prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new(),
                )
                .unwrap()
                .is_some()
                {
                    admitted += 1;
                    compare_native(&fixture, language, &[]);
                }
            }
            assert!(
                admitted >= 28,
                "the name and suffix matrix must exercise admission"
            );
        }
    }

    #[test]
    fn first_party_vba_recovered_module_syntax_keeps_the_native_route() {
        for source in [
            "Option Explicit:Sub Run()\nEnd Sub\n",
            "Attribute VB_Name = \"ModuleA\":Sub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub:",
            "Sub Rem\nEnd Sub\n",
            "Sub Rem()\nEnd Sub\n",
            "Sub Rem ()\nEnd Sub\n",
            "Function Rem()\nEnd Function\n",
            "Function Rem!()\nEnd Function\n",
        ] {
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source);
                let request = fixture.request(language, &[]);
                let cancellation = Cancellation::new();
                let native = fixture
                    .provider
                    .prepare_tree(
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(
                    native.traversal.coverage,
                    CoverageStatus::Unknown,
                    "the authored boundary requires native recovery: {source}"
                );
                assert!(
                    prepare(
                        &fixture.provider,
                        &request,
                        None,
                        &[],
                        fixture.provider.config.default_settings(),
                        &cancellation,
                    )
                    .unwrap()
                    .is_none(),
                    "recovered native source must not gain complete admission: {source}"
                );
            }
        }
    }

    #[test]
    fn first_party_vba_included_ranges_fall_back_with_exact_native_identity_closure() {
        let source =
            "Attribute VB_Name = \"ModuleA\"\nSub First()\nEnd Sub\nSub Second()\nEnd Sub\n";
        let start = u64::try_from(source.find("Sub Second").unwrap()).unwrap();
        let end = u64::try_from(source.trim_end().len()).unwrap();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let cancellation = Cancellation::with_deadline(
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            );
            let request = fixture.request(language, &[(start, end)]);
            assert!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation
                )
                .unwrap()
                .is_none()
            );
            let native = fixture
                .provider
                .prepare_tree(
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &cancellation,
                )
                .unwrap();
            assert!(native.traversal.fully_traversed);
            assert_eq!(native.traversal.coverage, CoverageStatus::Bounded);
            let pack = fixture
                .provider
                .query_packs
                .get_for_source(native.family, request.source().path().as_str())
                .unwrap();
            let legacy = pack
                .extract(
                    native.family,
                    &native.tree,
                    request.source().bytes(),
                    request.limits().max_syntax_nodes(),
                    usize::MAX,
                    &cancellation,
                )
                .unwrap();
            assert!(legacy.limit.is_none());
            let expected =
                normalize_query_candidates(legacy.candidates, &request, usize::MAX, &cancellation)
                    .unwrap();
            let expected_identity = normalize_query_candidates(
                pack.extract_identity(
                    native.family,
                    &native.tree,
                    request.source().bytes(),
                    request.limits().max_syntax_nodes(),
                    &cancellation,
                )
                .unwrap(),
                &request,
                usize::MAX,
                &cancellation,
            )
            .unwrap();
            assert_eq!(
                fixture
                    .provider
                    .required_syntax_fact_count(&request, &cancellation)
                    .unwrap(),
                expected_identity.required
            );
            let output = rootlight_adapter_sdk::execute_parse(
                &fixture.provider,
                &request,
                rootlight_adapter_sdk::MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .unwrap();
            assert_eq!(output.facts(), expected.facts);
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        }
    }

    #[test]
    fn first_party_vba_never_accepts_a_supported_prefix_or_object_context() {
        for source in [
            "Sub Run()\nDim value As Long\nvalue = #1/1/2000#\nEnd Sub\n",
            "Sub Run()\nEnd Sub\n#If Host Then\n#End If\n",
        ] {
            let fixture = Fixture::new(source);
            let request = fixture.request("vba", &[]);
            assert!(
                prepare(
                    &fixture.provider,
                    &request,
                    None,
                    &[],
                    fixture.provider.config.default_settings(),
                    &Cancellation::new()
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(fixture.provider.stats().pooled_parsers, 0);
        }
        let fixture = Fixture::new("Attribute VB_Name = \"ModuleA\"\nSub Run()\nEnd Sub\n");
        let analysis = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("vba").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(fixture.source.content_hash()),
            &fixture.limits,
        )
        .unwrap()
        .with_native_vba_module(NativeVbaModuleKind::Object)
        .unwrap();
        let request = analysis.to_parse_request();
        assert!(
            prepare(
                &fixture.provider,
                &request,
                None,
                &[],
                fixture.provider.config.default_settings(),
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }
}
