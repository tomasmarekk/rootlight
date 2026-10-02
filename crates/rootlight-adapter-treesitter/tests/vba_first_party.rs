//! Public SDK contracts for the closed first-party procedural VBA route.
//! Native fallback remains whole-module; source identity and resource admission
//! must not change when a file crosses between the two parser routes.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, IncludedRange, LanguageId, MemoryAdmissionPolicy, NativeVbaModuleKind,
    ParseOutput, ParseRequest, ResourceKind, SinkError, StreamLimits, SyntaxFact, SyntaxFactKind,
    execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
    VBA_PROCEDURAL_FRONTEND_VERSION,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, IrLimits, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const SOURCE_LIMIT: usize = 1024 * 1024;
const CLOSED_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nOption Explicit\r\nPublic Sub Run()\r\n' only authored trivia\r\nEnd Sub\r\nPrivate Function Count() As Long\r\nEnd Function\r\n";
const SCALAR_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nOption Explicit\r\nSub Run()\r\nDim count As Long\r\nConst Limit As Long = 7\r\ncount = Limit\r\nLet count = 1\r\nEnd Sub\r\nFunction Value() As Long\r\nValue = 7\r\nEnd Function\r\n";
const CALL_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Notify()\r\nEnd Sub\r\nSub Run()\r\nCall Notify\r\nCall [Notify]( )\r\nEnd Sub\r\n";
const PARAMETER_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Accept(ByVal [value] As Long, ByRef text As String)\r\n[value] = [value]\r\ntext = text\r\nEnd Sub\r\nFunction Echo(item As Long) As Long\r\nEcho = item\r\nEnd Function\r\n";
const CALL_ARGUMENT_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Run()\r\nDim value As Long\r\nCall Target(value, 7, \"a,\"\"b\"\":()\", True, [value])\r\nCall Target( value, value )\r\nCall Target()\r\nEnd Sub\r\n";
const APPLICATION_ASSIGNMENT_MODULE: &[u8] = b"Private items(2) As Long\r\nSub Run()\r\nDim result As Variant\r\nDim value As Long\r\nresult = items()\r\nLet [result] = [items]( 007 )\r\nresult = items(value, True, False, \"a,\"\"b\"\":()\", [value])\r\nresult = items ( value )\r\nresult = items( _\r\n value, _\r\n 7)\r\nEnd Sub\r\n";
const EXPRESSION_ASSIGNMENT_MODULE: &[u8] = b"Sub Run(ByVal value As Long)\r\nDim result As Variant\r\nresult = -(value + 007) * +2 ^ 3 ^ 4\r\nresult = Not (value = 1) And True Or False Xor True Eqv False Imp True\r\nresult = \"a,\"\"b\"\":()\" & \"x\"\r\nresult = Outer(Inner(value + 1), items(-(value - 2))) + value\r\nEnd Sub\r\n";
const EXPRESSION_CALL_MODULE: &[u8] = b"Sub Run(ByVal value As Long, ByVal other As Long)\r\nCall Target(value + 1, Inner(other) * -2)\r\nEnd Sub\r\n";
const IMPLICIT_CALL_MODULE: &[u8] = b"Sub Run(ByVal value As Long, ByVal other As Long)\r\nTarget value + 1, Inner(other) * -2\r\nEnd Sub\r\n";
const STATEMENT_VALUE_APPLICATION_MODULE: &[u8] = b"Sub Run(ByVal value As Long, ByVal other As Long)\r\nTarget (value + 1, Inner(other) * -2)\r\nEnd Sub\r\n";
const OPTIONAL_PARAMETER_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Accept(required As Long, Optional ByVal count As Long = 007, Optional [text] As String = \"a,\"\"b\"\":()\", Optional flag As Boolean = False, Optional unset As Long)\r\ncount = required\r\n[text] = [text]\r\nEnd Sub\r\n";
const OMITTED_TYPE_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Accept(required, ByVal typed As Long, Optional ByVal count = 007, Optional [text] = \"a,\"\"b\"\":()\", Optional flag = False, Optional unset)\r\ncount = required\r\n[text] = [text]\r\nEnd Sub\r\nFunction Echo(ByRef value) As Long\r\nEcho = value\r\nEnd Function\r\n";
const RADIX_MODULE: &str = "Attribute VB_Name = \"Radix\"\nConst ModuleSeed As Long = &H0A\nPrivate slots(-&O1 To +&H2, &3) As Long\nSub Target(ByVal first As Long, Optional ByVal second As Long = &o7)\nEnd Sub\nFunction Inner(ByVal value As Long) As Long\nInner = &H1\nEnd Function\nSub Run()\nDim local As Long, grid(-&H1 To +&O2, &3) As Long\nConst LocalSeed As Long = &O7\nlocal = &hF\nLet local = &77\nlocal = LocalSeed\nlocal = grid(&H0, &O1)\nCall Target(&H1, &o2)\nCall Target(local + &H2, Inner(&3) * -&H1)\nEnd Sub\n";

#[test]
fn accepted_modules_preserve_exact_identity_without_native_pool_or_cache_work() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(CLOSED_MODULE);
        let provider = provider();
        let limits = limits(4096);
        let request = fixture.request(&limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert!(required > 1);
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            CLOSED_MODULE.len()
        );
        assert!(output.diagnostics().is_empty());
        for name in [b"Run".as_slice(), b"Count"] {
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && bytes_at(CLOSED_MODULE, fact.span()) == name
            }));
        }
        for fact in output.facts() {
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
            if let Some(parent) = fact.parent() {
                assert!(
                    output
                        .facts()
                        .iter()
                        .any(|candidate| candidate.local_id() == parent)
                );
            }
        }
        let repeated = parse(&provider, &request);
        assert_eq!(output.facts(), repeated.facts());
        assert_eq!(output.report(), repeated.report());
        let stats = provider.stats();
        assert_eq!(stats.pooled_parsers, 0);
        assert_eq!(stats.checked_out_parsers, 0);
        assert_eq!(stats.cache.entries, 0);
    }
}

#[test]
fn scalar_modules_preserve_local_identity_and_value_reads_without_native_work() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(SCALAR_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str() == "vba.value_identifier.reference"
                && bytes_at(SCALAR_MODULE, fact.span()) == b"Limit"
        }));
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str() == "vba.let_identifier.reference"
                && bytes_at(SCALAR_MODULE, fact.span()) == b"count"
        }));
        let exact_limits = limits(required);
        let request = fixture.request(&exact_limits, language, Vec::new());
        let exact = parse(&provider, &request);
        for name in [b"Run".as_slice(), b"Value", b"count", b"Limit"] {
            for result in [&output, &exact] {
                assert!(result.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str().ends_with(".definition")
                        && bytes_at(SCALAR_MODULE, fact.span()) == name
                }));
            }
        }
        let short_limits = limits(required - 1);
        let request = fixture.request(&short_limits, language, Vec::new());
        assert_eq!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            })
        );
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn radix_literals_preserve_exact_source_identity_without_numeric_facts() {
    for newline in ["\n", "\r\n"] {
        let source = RADIX_MODULE.replace('\n', newline);
        let bytes = source.as_bytes();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(bytes);
            let provider = provider();
            let budget = limits(4096);
            let request = fixture.request(&budget, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 32);
            let output = parse(&provider, &request);
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                bytes.len()
            );
            assert!(output.diagnostics().is_empty());
            assert_eq!(
                fixture.source.content_hash(),
                rootlight_ids::content_hash(bytes)
            );
            let exact_limits = limits(required);
            let exact = parse(
                &provider,
                &fixture.request(&exact_limits, language, Vec::new()),
            );
            for (clause, name, syntax) in [
                (
                    "ModuleSeed As Long = &H0A",
                    "ModuleSeed",
                    "vba.constant.declaration",
                ),
                (
                    "slots(-&O1 To +&H2, &3) As Long",
                    "slots",
                    "vba.variable.declaration",
                ),
                ("ByVal first As Long", "first", "vba.parameter.declaration"),
                (
                    "Optional ByVal second As Long = &o7",
                    "second",
                    "vba.omittable_parameter.declaration",
                ),
                ("ByVal value As Long", "value", "vba.parameter.declaration"),
                ("local As Long", "local", "vba.variable.declaration"),
                (
                    "grid(-&H1 To +&O2, &3) As Long",
                    "grid",
                    "vba.variable.declaration",
                ),
                (
                    "LocalSeed As Long = &O7",
                    "LocalSeed",
                    "vba.constant.declaration",
                ),
            ] {
                let start = source.find(clause).unwrap();
                let name_start = start + clause.find(name).unwrap();
                for result in [&output, &exact] {
                    let declaration = result
                        .facts()
                        .iter()
                        .find(|fact| {
                            fact.syntax_kind().as_str() == syntax
                                && fact.span().start_byte() == u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(start + clause.len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(bytes_at(bytes, declaration.span()), clause.as_bytes());
                    let definition = result
                        .facts()
                        .iter()
                        .find(|fact| {
                            fact.syntax_kind().as_str() == "vba.identifier.definition"
                                && fact.span().start_byte() == u64::try_from(name_start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(name_start + name.len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(definition.parent(), Some(declaration.local_id()));
                    assert_eq!(definition.depth(), declaration.depth() + 1);
                    assert_eq!(bytes_at(bytes, definition.span()), name.as_bytes());
                    assert_eq!(definition.span().file(), fixture.source.span().file());
                }
            }
            for literal in [
                "&H0A", "&O1", "&H2", "&3", "&o7", "&H1", "&O2", "&O7", "&hF", "&77", "&H0", "&o2",
            ] {
                for (start, _) in source.match_indices(literal) {
                    assert!(
                        !output.facts().iter().any(|fact| {
                            fact.syntax_kind().as_str().ends_with(".reference")
                                && fact.span().start_byte() >= u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    <= u64::try_from(start + literal.len()).unwrap()
                        }),
                        "numeric syntax never fabricates an identifier: {language}, {literal}"
                    );
                }
            }
            for fact in output.facts() {
                assert_eq!(fact.span().file(), fixture.source.span().file());
                assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
                if let Some(parent) = fact.parent() {
                    assert!(
                        output
                            .facts()
                            .iter()
                            .any(|candidate| candidate.local_id() == parent)
                    );
                }
            }
            assert!(
                exact
                    .facts()
                    .iter()
                    .all(|fact| !fact.syntax_kind().as_str().ends_with(".reference"))
            );
            let repeated = parse(&provider, &request);
            assert_eq!(output.facts(), repeated.facts());
            assert_eq!(output.report(), repeated.report());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn integer_suffixes_preserve_exact_sdk_identity_without_numeric_or_type_facts() {
    for literal in ["007", "&Hf", "&h0A", "&O7", "&o0007", "&77"] {
        for suffix in ['%', '&', '^'] {
            assert_numeric_sdk_identity(&format!("{literal}{suffix}"));
        }
    }
}

#[test]
fn float_literals_preserve_exact_sdk_identity_without_numeric_or_type_facts() {
    for literal in [
        "1.5", ".5", "5.", "1E3", "1e+3", "1D-3", "1d+3", "7!", "7#", "7@", "1.5!", "1e-3#",
        "1D+3@",
    ] {
        assert_numeric_sdk_identity(literal);
    }
}

fn assert_numeric_sdk_identity(literal: &str) {
    assert_numeric_sdk_initializer_identity(literal, literal);
}

fn assert_numeric_sdk_initializer_identity(literal: &str, initializer: &str) {
    let optional = format!("Optional ByVal second As Long = {initializer}");
    let module_constant = format!("ModuleSeed As Long = {initializer}");
    let module_array = format!("slots(-{literal} To +{literal}, {literal}) As Long");
    let local_array = format!("grid(-{literal} To +{literal}, {literal}) As Long");
    let local_constant = format!("LocalSeed As Long = {initializer}");
    let source = format!(
        "Attribute VB_Name = \"Suffix\"\n' retained header trivia\nConst {module_constant}\nPrivate {module_array}\nSub Target(ByVal first As Long, {optional})\nEnd Sub\nFunction Inner(ByVal value As Long) As Long\nInner = {literal}\nEnd Function\nSub Run()\nDim local As Long, {local_array}\nConst {local_constant}\nlocal = {literal}\nLet local = {literal}\nlocal = LocalSeed\nlocal = grid({literal}, {literal})\nCall Target({literal})\nCall Target(local + {literal}, Inner({literal}) * -{literal})\nEnd Sub\n"
    );
    for newline in ["\n", "\r\n"] {
        let source = source.replace('\n', newline);
        let module_constant = module_constant.replace('\n', newline);
        let local_constant = local_constant.replace('\n', newline);
        let optional = optional.replace('\n', newline);
        let bytes = source.as_bytes();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(bytes);
            let provider = provider();
            let budget = limits(4096);
            let request = fixture.request(&budget, language, Vec::new());
            let inventory = rootlight_vba::module_inventory(
                bytes,
                rootlight_vba::ModuleLimits::new(bytes.len(), 4096, 4096),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let rootlight_vba::ModuleBodyStatement::Declaration(local_constant_record) =
                &inventory.procedures()[2].statements()[1]
            else {
                panic!("local constant remains an atomic declaration");
            };
            for (clause, value) in [
                (module_constant.as_str(), inventory.constants()[0].value()),
                (
                    optional.as_str(),
                    inventory.procedures()[0].parameters()[1]
                        .default_value()
                        .unwrap(),
                ),
                (
                    local_constant.as_str(),
                    local_constant_record.value().unwrap(),
                ),
            ] {
                let clause_start = source.find(clause).unwrap();
                let initializer_start = clause_start + clause.find(" = ").unwrap() + 3;
                let atom_start =
                    initializer_start + initializer.replace('\n', newline).find(literal).unwrap();
                assert!(matches!(
                    value.kind(),
                    rootlight_vba::ModuleScalarValueKind::UnsignedDecimal
                        | rootlight_vba::ModuleScalarValueKind::UnsignedHexadecimal
                        | rootlight_vba::ModuleScalarValueKind::UnsignedOctal
                        | rootlight_vba::ModuleScalarValueKind::UnsignedFloat
                ));
                assert_eq!(&bytes[value.span()], literal.as_bytes());
                assert_eq!(value.span(), atom_start..atom_start + literal.len());
                assert_eq!(
                    value.initializer_span(),
                    initializer_start..atom_start + literal.len()
                );
                assert_eq!(
                    value.unary_minus_span(),
                    initializer
                        .starts_with('-')
                        .then_some(initializer_start..initializer_start + 1)
                );
                assert_eq!(
                    value.initializer_work_nodes(),
                    usize::from(initializer.starts_with('-'))
                );
            }
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 32, "{language}, {literal}");
            let full = parse(&provider, &request);
            assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
            assert_eq!(full.report().coverage().covered_source_bytes(), bytes.len());
            assert!(full.diagnostics().is_empty());
            for value in [
                inventory.constants()[0].value(),
                inventory.procedures()[0].parameters()[1]
                    .default_value()
                    .unwrap(),
                local_constant_record.value().unwrap(),
            ] {
                let initializer = value.initializer_span();
                assert!(!full.facts().iter().any(|fact| {
                    fact.span().start_byte() >= u64::try_from(initializer.start).unwrap()
                        && fact.span().end_byte() <= u64::try_from(initializer.end).unwrap()
                }));
            }
            assert_eq!(
                fixture.source.content_hash(),
                rootlight_ids::content_hash(bytes)
            );
            let exact_budget = limits(required);
            let exact = parse(
                &provider,
                &fixture.request(&exact_budget, language, Vec::new()),
            );
            for (clause, name, syntax) in [
                (
                    module_constant.as_str(),
                    "ModuleSeed",
                    "vba.constant.declaration",
                ),
                (module_array.as_str(), "slots", "vba.variable.declaration"),
                ("ByVal first As Long", "first", "vba.parameter.declaration"),
                (
                    optional.as_str(),
                    "second",
                    "vba.omittable_parameter.declaration",
                ),
                ("ByVal value As Long", "value", "vba.parameter.declaration"),
                ("local As Long", "local", "vba.variable.declaration"),
                (local_array.as_str(), "grid", "vba.variable.declaration"),
                (
                    local_constant.as_str(),
                    "LocalSeed",
                    "vba.constant.declaration",
                ),
            ] {
                let start = source.find(clause).unwrap();
                let name_start = start + clause.find(name).unwrap();
                for output in [&full, &exact] {
                    let declaration = output
                        .facts()
                        .iter()
                        .find(|fact| {
                            fact.syntax_kind().as_str() == syntax
                                && fact.span().start_byte() == u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(start + clause.len()).unwrap()
                        })
                        .unwrap();
                    let definition = output
                        .facts()
                        .iter()
                        .find(|fact| {
                            fact.syntax_kind().as_str() == "vba.identifier.definition"
                                && fact.span().start_byte() == u64::try_from(name_start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(name_start + name.len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(bytes_at(bytes, declaration.span()), clause.as_bytes());
                    assert_eq!(bytes_at(bytes, definition.span()), name.as_bytes());
                    assert_eq!(definition.parent(), Some(declaration.local_id()));
                    assert_eq!(definition.depth(), declaration.depth() + 1);
                }
            }
            assert_eq!(source.match_indices(literal).count(), 18);
            for (start, _) in source.match_indices(literal) {
                assert!(
                    !full.facts().iter().any(|fact| {
                        fact.span().start_byte() >= u64::try_from(start).unwrap()
                            && fact.span().end_byte()
                                <= u64::try_from(start + literal.len()).unwrap()
                    }),
                    "no numeric or suffix type fact: {language}, {literal}, {start}"
                );
            }
            for fact in full.facts() {
                assert_eq!(fact.span().file(), fixture.source.span().file());
                assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
                if let Some(parent) = fact.parent() {
                    assert!(
                        full.facts()
                            .iter()
                            .any(|candidate| candidate.local_id() == parent)
                    );
                }
            }
            assert!(
                exact
                    .facts()
                    .iter()
                    .all(|fact| !fact.syntax_kind().as_str().ends_with(".reference"))
            );
            let repeated = parse(&provider, &request);
            assert_eq!(repeated.facts(), full.facts());
            assert_eq!(repeated.report(), full.report());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn signed_numeric_initializers_preserve_unsigned_atoms_and_exact_sdk_clauses() {
    for literal in [
        "007", "7%", "&Hf&", "&o7^", "&77", "1.5", ".5#", "0.#", "1e-3", "1D+3", "7!", "1D+3@",
    ] {
        for trivia in ["", " _\n "] {
            assert_numeric_sdk_initializer_identity(literal, &format!("-{trivia}{literal}"));
        }
    }
}

#[test]
fn signed_numeric_initializers_charge_sign_work_without_more_required_facts() {
    let unsigned = "Sub Run(Optional a=7, Optional b=7, Optional c=7, Optional d=7, Optional e=7, Optional f=7, Optional g=7, Optional h=7)\nEnd Sub\n";
    let signed = unsigned.replace("=7", "=-7");
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(signed.as_bytes());
        let provider = provider();
        let base = limits(4096);
        let request = fixture.request(&base, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 23);
        let full = parse(&provider, &request);
        assert_eq!(full.report().resources().syntax_nodes(), 28);
        assert_eq!(full.facts().len(), 24);
        let unsigned_fixture = Fixture::new(unsigned.as_bytes());
        let unsigned_output = parse(
            &provider,
            &unsigned_fixture.request(&base, language, Vec::new()),
        );
        assert_eq!(unsigned_output.report().resources().syntax_nodes(), 20);
        assert_eq!(unsigned_output.facts().len(), full.facts().len());
        assert_eq!(
            provider
                .required_syntax_fact_count(
                    &unsigned_fixture.request(&base, language, Vec::new()),
                    &deadline(),
                )
                .unwrap(),
            required
        );
        for capacity in [required, full.facts().len()] {
            let budget = limits(capacity);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert_eq!(output.facts().len(), capacity);
            assert_eq!(
                output
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == "vba.identifier.definition")
                    .count(),
                9
            );
        }
        let short = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            })
        );
        for maximum in [28, 27] {
            let budget = AnalysisLimits::new(
                base.max_source_bytes(),
                maximum,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let result = execute_parse(
                &provider,
                &fixture.request(&budget, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if maximum == 28 {
                assert_eq!(result.unwrap().facts(), full.facts());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::RejectedRequest(
                        rootlight_adapter_sdk::RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 28,
                            limit: 27,
                        }
                    )
                );
            }
        }
        assert!(matches!(
            AnalysisLimits::new(
                base.max_source_bytes(),
                0,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            ),
            Err(rootlight_adapter_sdk::LimitError::Zero {
                field: "analysis.max_syntax_nodes",
            })
        ));
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled,
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        ));
        assert_eq!(parse(&provider, &request).facts(), full.facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn signed_initializer_unsupported_tails_preserve_whole_native_fallback() {
    for initializer in [
        "+7",
        "--7",
        "-+7",
        "-(7)",
        "-Seed",
        "-True",
        "-\"text\"",
        "-7 + 1",
        "-1D+",
        "-1.5%",
        "-&H1#",
        "-7 trailing",
    ] {
        for statement in [
            format!("Const Bad As Long = {initializer}\n"),
            format!("Sub Other(Optional value As Long = {initializer})\nEnd Sub\n"),
            format!("Sub Other()\nConst Bad As Long = {initializer}\nEnd Sub\n"),
        ] {
            let source = format!("Sub First()\nEnd Sub\n{statement}");
            for newline in ["\n", "\r\n"] {
                let source = source.replace('\n', newline);
                for language in ["vba", "visual-basic-classic"] {
                    let fixture = Fixture::new(source.as_bytes());
                    let provider = provider();
                    let budget = limits(4096);
                    let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
                    assert!(
                        provider.stats().pooled_parsers > 0,
                        "{language}: {statement}"
                    );
                    assert_eq!(provider.stats().checked_out_parsers, 0);
                    assert!(output.facts().iter().any(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && bytes_at(source.as_bytes(), fact.span()) == b"First"
                    }));
                }
            }
        }
    }
}

#[test]
fn radix_call_limits_preserve_mandatory_identity_and_typed_cancellation() {
    let literals = ["&Hf", "&h0A", "&O7", "&o0007", "&77"].map(str::to_owned);
    for literal in literals.into_iter().chain([
        format!("&H{}", "F".repeat(2048)),
        format!("&{}", "7".repeat(2048)),
    ]) {
        assert_literal_call_limits(&literal);
    }
}

#[test]
fn integer_suffix_call_limits_preserve_mandatory_identity_and_typed_cancellation() {
    for literal in ["007", "&Hf", "&h0A", "&O7", "&o0007", "&77"] {
        for suffix in ['%', '&', '^'] {
            assert_literal_call_limits(&format!("{literal}{suffix}"));
        }
    }
}

#[test]
fn float_call_limits_preserve_mandatory_identity_and_typed_cancellation() {
    for literal in [
        "1.5", ".5", "5.", "1E3", "1e+3", "1D-3", "1d+3", "7!", "7#", "7@", "1.5!", "1e-3#",
        "1D+3@",
    ] {
        assert_literal_call_limits(literal);
    }
    for literal in [
        format!("{}.5D+1@", "7".repeat(2048)),
        format!(".{}e-1!", "5".repeat(2048)),
    ] {
        assert_literal_call_limits(&literal);
    }
}

#[test]
fn nothing_call_limits_preserve_mandatory_identity_and_typed_cancellation() {
    for literal in ["Nothing", "nOtHiNg", "NOTHING"] {
        assert_literal_call_limits(literal);
    }
}

fn assert_literal_call_limits(literal: &str) {
    let source =
        format!("Sub Run(ByVal value As Long)\nCall Target({literal}, &O2, &3)\nEnd Sub\n");
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source.as_bytes());
        let provider = provider();
        let base = limits(4096);
        let request = fixture.request(&base, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 9);
        let full = parse(&provider, &request);
        assert_eq!(full.report().resources().syntax_nodes(), 9);
        assert_eq!(full.facts().len(), 12);
        for capacity in [required, full.facts().len() - 1, full.facts().len()] {
            let budget = limits(capacity);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert_eq!(output.facts().len(), capacity);
            assert_eq!(
                output.report().coverage().status(),
                if capacity == full.facts().len() {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                }
            );
            for name in [b"Run".as_slice(), b"value"] {
                assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                    == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == name));
            }
            if capacity == required {
                assert!(
                    output
                        .facts()
                        .iter()
                        .all(|fact| !fact.syntax_kind().as_str().ends_with(".reference"))
                );
            }
        }
        let short = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1
            })
        );
        // Work is nine, but full optional output independently needs twelve candidates.
        for maximum in [12, 11] {
            let budget = AnalysisLimits::new(
                base.max_source_bytes(),
                maximum,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let result = execute_parse(
                &provider,
                &fixture.request(&budget, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if maximum == 12 {
                assert_eq!(result.unwrap().facts(), full.facts());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::RejectedRequest(
                        rootlight_adapter_sdk::RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 12,
                            limit: 11
                        }
                    )
                );
            }
        }
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(parse(&provider, &request).facts(), full.facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn radix_literal_late_invalid_tails_preserve_whole_native_fallback() {
    for statement in [
        "value = &H",
        "value = &O",
        "value = &O8",
        "value = &08",
        "value = &H1G",
        "value = &H1&&",
        "value = &O7%%",
        "value = &7!",
        "Call Target(&H1, &O8)",
        "Dim grid(&H1, &O8) As Long",
        "Const Seed As Long = &H1&^",
        "value = 7^^",
        "value = 7^%",
        "value = &H1!",
        "value = &O7#",
        "value = &7@",
        "Call Target(7%, &H1&^)",
        "Dim grid(-&H1% To +&O7^^) As Long",
    ] {
        let source = format!(
            "Sub First()\nEnd Sub\nSub Run()\nDim value As Long\nvalue = &H1\n{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert!(
                provider.stats().pooled_parsers > 0,
                "whole native fallback: {language}, {statement}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                == "vba.identifier.definition"
                && bytes_at(source.as_bytes(), fact.span()) == b"First"));
        }
    }
}

#[test]
fn float_literal_late_invalid_tails_preserve_whole_native_fallback() {
    for statement in [
        "value = 1E",
        "value = 1E+",
        "value = .5D-",
        "value = 1.5!#",
        "value = 7@%",
        "value = 1.5%",
        "value = &H1#",
        "value = 1D+3 trailing",
        "Call Target(1.5, 2D+)",
        "Dim grid(-1.5 To +2D+)",
        "Const Seed As Double = 1.5@#",
        "Const Seed As Double = -1.5 + 2",
        "Sub Nested(Optional value = 1D+)\nEnd Sub",
    ] {
        let source = format!(
            "Sub First()\nEnd Sub\nSub Run()\nDim value As Double\nvalue = 1.5\n{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {statement}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == b"First"
            }));
        }
    }
}

#[test]
fn radix_literal_edit_invalidates_a_real_native_tree_without_identity_fabrication() {
    assert_radix_literal_native_edit("&H1");
}

#[test]
fn integer_suffix_edits_invalidate_real_native_trees_without_identity_fabrication() {
    for literal in ["7", "&H1"] {
        for suffix in ['%', '&', '^'] {
            assert_radix_literal_native_edit(&format!("{literal}{suffix}"));
        }
    }
}

#[test]
fn float_literal_edits_invalidate_real_native_trees_without_identity_fabrication() {
    for literal in ["1.5", ".5E+2!", "1D-3", "7#", "5.@"] {
        assert_radix_literal_native_edit(literal);
    }
}

fn assert_radix_literal_native_edit(literal: &str) {
    const NATIVE: &str = "Sub Run()\r\nDim value As Long\r\nvalue = #1/1/2000#\r\nEnd Sub\r\n";
    let own = format!("Sub Run()\r\nDim value As Long\r\nvalue = {literal}\r\nEnd Sub\r\n");
    for language in ["vba", "visual-basic-classic"] {
        let mut fixture = Fixture::new(NATIVE.as_bytes());
        let provider = provider();
        let budget = limits(4096);
        let settings = ParserSettings::new(512).unwrap();
        let previous = provider
            .execute_with_previous(
                &fixture.request(&budget, language, Vec::new()),
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let continuation = previous
            .previous()
            .expect("date syntax owns a real cached native tree");
        assert_eq!(
            previous.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        let old_hash = fixture.source.content_hash();
        let old_stats = provider.stats();
        assert!(old_stats.pooled_parsers > 0);
        assert!(old_stats.cache.entries > 0);
        fixture.rewrite(own.as_bytes());
        assert_ne!(old_hash, fixture.source.content_hash());
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(own.as_bytes())
        );
        let request = fixture.request(&budget, language, Vec::new());
        let start = NATIVE.find("#1/1/2000#").unwrap();
        let edit = SourceEdit::new(start, start + "#1/1/2000#".len(), literal).unwrap();
        let output = provider
            .execute_with_previous(
                &request,
                Some(continuation),
                &[edit],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            output.reuse_status(),
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
        assert_eq!(
            output.reuse_key().grammar_version(),
            VBA_PROCEDURAL_FRONTEND_VERSION
        );
        assert!(output.previous().is_none());
        assert_eq!(
            output.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.output().report().coverage().covered_source_bytes(),
            own.len()
        );
        assert!(output.output().diagnostics().is_empty());
        assert_eq!(output.output().facts(), parse(&provider, &request).facts());
        assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
        assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn local_dim_lists_preserve_each_required_declarator_and_procedure_owner() {
    let source = b"Sub Run()\r\nDim alpha, beta As Long, grid(-1 To +2, 003) As Variant, pending()\r\nDim [Rem] As String, [Public](), first, _\r\n second(2,3)As Long ' retained note\r\nalpha = beta: beta = alpha\r\nEnd Sub\r\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 23);
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert_eq!(
            full.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(source)
        );
        assert!(full.diagnostics().is_empty());
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        for (output, optional) in [(&full, true), (&exact, false)] {
            let procedure = output
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
                .unwrap();
            let variables = output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.variable.declaration")
                .collect::<Vec<_>>();
            assert_eq!(variables.len(), 8);
            let mut ids = std::collections::BTreeSet::new();
            for (clause, name) in [
                (b"alpha".as_slice(), b"alpha".as_slice()),
                (b"beta As Long", b"beta"),
                (b"grid(-1 To +2, 003) As Variant", b"grid"),
                (b"pending()", b"pending"),
                (b"[Rem] As String", b"[Rem]"),
                (b"[Public]()", b"[Public]"),
                (b"first", b"first"),
                (b"second(2,3)As Long", b"second"),
            ] {
                let declaration = variables
                    .iter()
                    .find(|fact| bytes_at(source, fact.span()) == clause)
                    .unwrap();
                assert!(ids.insert(declaration.local_id()));
                assert_eq!(declaration.parent(), Some(procedure.local_id()));
                assert_eq!(declaration.depth(), 5);
                assert_eq!(declaration.span().file(), fixture.source.span().file());
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.parent() == Some(declaration.local_id())
                            && fact.syntax_kind().as_str() == "vba.identifier.definition"
                    })
                    .unwrap();
                assert_eq!(bytes_at(source, definition.span()), name);
                assert_eq!(
                    definition.span().start_byte(),
                    declaration.span().start_byte()
                );
                assert_eq!(definition.depth(), 6);
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| fact.syntax_kind().as_str()
                            == "vba.visibility_unknown.expression"
                            && fact.span() == declaration.span())
                        .count(),
                    usize::from(optional)
                );
                assert!(!output.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str().ends_with(".reference")
                        && declaration.span().start_byte() <= fact.span().start_byte()
                        && fact.span().end_byte() <= declaration.span().end_byte()
                }));
            }
            assert!(
                output
                    .facts()
                    .iter()
                    .all(|fact| fact.parent().is_none_or(|parent| output
                        .facts()
                        .iter()
                        .any(|candidate| candidate.local_id() == parent)))
            );
        }
        assert_eq!(full.facts(), parse(&provider, &request).facts());
        assert_eq!(full.report(), parse(&provider, &request).report());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn local_dim_list_limits_keep_exact_identity_and_typed_cancellation() {
    let source = b"Sub Run()\nDim first, grid(-1 To +2,003) As Long\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let base = limits(4096);
        let request = fixture.request(&base, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 11);
        let full = parse(&provider, &request);
        assert_eq!(full.report().resources().syntax_nodes(), 14);
        for maximum in [required, full.facts().len() - 1, full.facts().len()] {
            let budget = limits(maximum);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert_eq!(output.facts().len(), maximum);
            assert_eq!(
                output.report().coverage().status(),
                if maximum == full.facts().len() {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                }
            );
            for name in [b"first".as_slice(), b"grid"] {
                assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                    == "vba.identifier.definition"
                    && bytes_at(source, fact.span()) == name));
            }
        }
        let short = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1
            })
        );
        for maximum in [14, 13] {
            let budget = AnalysisLimits::new(
                base.max_source_bytes(),
                maximum,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let result = execute_parse(
                &provider,
                &fixture.request(&budget, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if maximum == 14 {
                assert_eq!(result.unwrap().facts(), full.facts());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::RejectedRequest(
                        rootlight_adapter_sdk::RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 14,
                            limit: 13
                        }
                    )
                );
            }
        }
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(parse(&provider, &request).facts(), full.facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn local_dim_lists_keep_late_unsupported_declarators_on_the_whole_native_route() {
    for declaration in [
        "Dim good, bad As Product",
        "Dim good, bad As New Product",
        "Dim good, bad$",
        "Dim good, bad(1 + 2)",
        "Dim good, bad(1E+)",
        "Dim good, bad(1,)",
        "Dim good, bad As Long=1",
        "Dim good,",
        "Dim good,,other",
        "Dim good, bad As",
        "Dim good, bad(1) extra",
    ] {
        let source = format!("Sub Run()\n{declaration}\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {declaration}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                == "vba.identifier.definition"
                && bytes_at(source.as_bytes(), fact.span()) == b"Run"));
        }
    }
}

#[test]
fn local_dim_list_edit_invalidates_a_real_native_tree_with_source_bound_identity() {
    const NATIVE: &str = "Dim first, grid(-1 To +2, #1/1/2000#) As Long";
    const OWN: &str = "Dim first, grid(-1 To +2, 003) As Long";
    let native = format!("Sub Run()\r\n{NATIVE}\r\nEnd Sub\r\n");
    let own = format!("Sub Run()\r\n{OWN}\r\nEnd Sub\r\n");
    for language in ["vba", "visual-basic-classic"] {
        let mut fixture = Fixture::new(native.as_bytes());
        let provider = provider();
        let budget = limits(4096);
        let settings = ParserSettings::new(512).unwrap();
        let request = fixture.request(&budget, language, Vec::new());
        let previous = provider
            .execute_with_previous(
                &request,
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let continuation = previous
            .previous()
            .expect("date bounds retain a real native continuation");
        assert_eq!(
            previous.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        let old_hash = fixture.source.content_hash();
        let old_stats = provider.stats();
        assert!(old_stats.pooled_parsers > 0);
        assert!(old_stats.cache.entries > 0);
        fixture.rewrite(own.as_bytes());
        let request = fixture.request(&budget, language, Vec::new());
        assert_ne!(old_hash, fixture.source.content_hash());
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(own.as_bytes())
        );
        let start = native.find(NATIVE).unwrap();
        let edit = SourceEdit::new(start, start + NATIVE.len(), OWN).unwrap();
        let output = provider
            .execute_with_previous(
                &request,
                Some(continuation),
                &[edit],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            output.reuse_status(),
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
        assert_eq!(
            output.reuse_key().grammar_version(),
            VBA_PROCEDURAL_FRONTEND_VERSION
        );
        assert!(output.previous().is_none());
        assert_eq!(
            output.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(output.output().facts(), parse(&provider, &request).facts());
        assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
        assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn grouped_module_variables_keep_individual_required_source_identity() {
    let source = b"Attribute VB_Name = \"Grouped\"\r\nPublic alpha, beta As Long, grid(-1 To +2, 003) As Variant, pending()\r\nPrivate [Public] As String, [Rem]()\r\nDim first, _\r\n second(2,3)As Long\r\nSub Run()\r\nCall grid()\r\nEnd Sub\r\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert_eq!(
            full.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(full.diagnostics().is_empty());
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        for (output, includes_visibility) in [(&full, true), (&exact, false)] {
            let declarations = output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.variable.declaration")
                .collect::<Vec<_>>();
            assert_eq!(declarations.len(), 8);
            for (clause, name, visibility) in [
                (
                    b"alpha".as_slice(),
                    b"alpha".as_slice(),
                    "vba.public.expression",
                ),
                (
                    b"beta As Long".as_slice(),
                    b"beta".as_slice(),
                    "vba.public.expression",
                ),
                (
                    b"grid(-1 To +2, 003) As Variant".as_slice(),
                    b"grid".as_slice(),
                    "vba.public.expression",
                ),
                (
                    b"pending()".as_slice(),
                    b"pending".as_slice(),
                    "vba.public.expression",
                ),
                (
                    b"[Public] As String".as_slice(),
                    b"[Public]".as_slice(),
                    "vba.private.expression",
                ),
                (
                    b"[Rem]()".as_slice(),
                    b"[Rem]".as_slice(),
                    "vba.private.expression",
                ),
                (
                    b"first".as_slice(),
                    b"first".as_slice(),
                    "vba.visibility_unknown.expression",
                ),
                (
                    b"second(2,3)As Long".as_slice(),
                    b"second".as_slice(),
                    "vba.visibility_unknown.expression",
                ),
            ] {
                let declaration = declarations
                    .iter()
                    .find(|fact| bytes_at(source, fact.span()) == clause)
                    .unwrap();
                assert_eq!(declaration.depth(), 3);
                let owner = output
                    .facts()
                    .iter()
                    .find(|fact| Some(fact.local_id()) == declaration.parent())
                    .unwrap();
                assert_eq!(owner.syntax_kind().as_str(), "vba.file.scope");
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.parent() == Some(declaration.local_id())
                            && fact.syntax_kind().as_str() == "vba.identifier.definition"
                    })
                    .unwrap();
                assert_eq!(bytes_at(source, definition.span()), name);
                assert_eq!(
                    definition.span().start_byte(),
                    declaration.span().start_byte()
                );
                assert_eq!(definition.span().file(), fixture.source.span().file());
                assert_eq!(definition.depth(), 4);
                if includes_visibility {
                    assert!(
                        output
                            .facts()
                            .iter()
                            .any(|fact| fact.syntax_kind().as_str() == visibility
                                && fact.span() == declaration.span())
                    );
                }
                assert!(output.facts().iter().all(|fact| {
                    !fact.syntax_kind().as_str().ends_with(".reference")
                        || fact.span().end_byte() <= declaration.span().start_byte()
                        || fact.span().start_byte() >= declaration.span().end_byte()
                }));
            }
        }
        assert_eq!(full.facts(), parse(&provider, &request).facts());
        let short_limits = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short_limits, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1
            })
        );
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn module_numeric_arrays_keep_exact_declarations_and_required_identity() {
    let source = b"Attribute VB_Name = \"Arrays\"\r\nDim items()\r\nPrivate grid(-1 To +2, 003) As Long\r\nPublic [more] _\r\n( _\r\n- _\r\n2 _\r\n)\r\nSub Run()\r\nCall items()\r\nEnd Sub\r\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert_eq!(
            full.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(full.diagnostics().is_empty());
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        for (output, includes_visibility) in [(&full, true), (&exact, false)] {
            let variables = output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.variable.declaration")
                .collect::<Vec<_>>();
            assert_eq!(variables.len(), 3);
            for (clause, name, visibility) in [
                (
                    b"items()".as_slice(),
                    b"items".as_slice(),
                    "vba.visibility_unknown.expression",
                ),
                (
                    b"grid(-1 To +2, 003) As Long".as_slice(),
                    b"grid".as_slice(),
                    "vba.private.expression",
                ),
                (
                    b"[more] _\r\n( _\r\n- _\r\n2 _\r\n)".as_slice(),
                    b"[more]".as_slice(),
                    "vba.public.expression",
                ),
            ] {
                let declaration = variables
                    .iter()
                    .find(|fact| bytes_at(source, fact.span()) == clause)
                    .unwrap();
                assert_eq!(declaration.depth(), 3);
                let owner = output
                    .facts()
                    .iter()
                    .find(|fact| Some(fact.local_id()) == declaration.parent())
                    .unwrap();
                assert_eq!(owner.syntax_kind().as_str(), "vba.file.scope");
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.parent() == Some(declaration.local_id())
                            && fact.syntax_kind().as_str() == "vba.identifier.definition"
                    })
                    .unwrap();
                assert_eq!(bytes_at(source, definition.span()), name);
                assert_eq!(
                    definition.span().start_byte(),
                    declaration.span().start_byte()
                );
                assert_eq!(definition.span().file(), fixture.source.span().file());
                assert_eq!(definition.depth(), 4);
                if includes_visibility {
                    assert!(output.facts().iter().any(|fact| {
                        fact.syntax_kind().as_str() == visibility
                            && fact.span() == declaration.span()
                    }));
                }
                assert!(output.facts().iter().all(|fact| {
                    !fact.syntax_kind().as_str().ends_with(".reference")
                        || fact.span().end_byte() <= declaration.span().start_byte()
                        || fact.span().start_byte() >= declaration.span().end_byte()
                }));
            }
        }
        assert_eq!(full.facts(), parse(&provider, &request).facts());
        let short_limits = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short_limits, language, Vec::new()),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            })
        );
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn module_scalar_variables_keep_written_visibility_and_lexical_owners() {
    let source = b"Attribute VB_Name = \"Authored\"\r\nPublic stored As Long\r\nPrivate [text]\r\nDim hidden As Boolean\r\nSub Run(ByVal stored As Long)\r\nDim hidden As Boolean\r\nstored = stored\r\nhidden = hidden\r\nEnd Sub\r\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(output.diagnostics().is_empty());
        let variables = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.variable.declaration")
            .collect::<Vec<_>>();
        assert_eq!(variables.len(), 4);
        let mut module_count = 0;
        let mut body_count = 0;
        for variable in variables {
            let parent = output
                .facts()
                .iter()
                .find(|fact| Some(fact.local_id()) == variable.parent())
                .unwrap();
            let (name, visibility) = match parent.syntax_kind().as_str() {
                "vba.file.scope" => {
                    module_count += 1;
                    assert_eq!(variable.depth(), 3);
                    match bytes_at(source, variable.span()) {
                        b"stored As Long" => (b"stored".as_slice(), "vba.public.expression"),
                        b"[text]" => (b"[text]".as_slice(), "vba.private.expression"),
                        b"hidden As Boolean" => {
                            (b"hidden".as_slice(), "vba.visibility_unknown.expression")
                        }
                        written => {
                            panic!("a module variable retains its exact declarator: {written:?}")
                        }
                    }
                }
                "vba.sub_function.declaration" => {
                    body_count += 1;
                    assert_eq!(bytes_at(source, variable.span()), b"hidden As Boolean");
                    (b"hidden".as_slice(), "vba.visibility_unknown.expression")
                }
                kind => panic!("a variable has an exact file or procedure owner: {kind}"),
            };
            let definition = output
                .facts()
                .iter()
                .find(|fact| {
                    fact.parent() == Some(variable.local_id())
                        && fact.syntax_kind().as_str() == "vba.identifier.definition"
                })
                .unwrap();
            assert_eq!(bytes_at(source, definition.span()), name);
            assert_eq!(definition.depth(), variable.depth() + 1);
            assert!(
                output
                    .facts()
                    .iter()
                    .any(|fact| fact.syntax_kind().as_str() == visibility
                        && fact.span() == variable.span())
            );
        }
        assert_eq!((module_count, body_count), (3, 1));
        assert_eq!(output.facts(), parse(&provider, &request).facts());
        let exact_limits = limits(required);
        let exact_request = fixture.request(&exact_limits, language, Vec::new());
        let exact = parse(&provider, &exact_request);
        assert_eq!(
            exact
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.variable.declaration")
                .count(),
            4
        );
        assert_eq!(
            exact
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.parameter.declaration")
                .count(),
            1
        );
        let short_limits = limits(required - 1);
        let short_request = fixture.request(&short_limits, language, Vec::new());
        assert_eq!(
            execute_parse(
                &provider,
                &short_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            })
        );
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        ));
        let stats = provider.stats();
        assert_eq!(stats.pooled_parsers, 0);
        assert_eq!(stats.checked_out_parsers, 0);
        assert_eq!(stats.cache.entries, 0);
    }
}

#[test]
fn module_constants_and_named_defaults_keep_file_and_formal_ownership() {
    let source = b"Attribute VB_Name = \"Authored\"\r\nOption Explicit\r\nPublic Const Fallback = 7\r\nPrivate Const Label As String = \"text\"\r\nSub Accept(Optional Fallback = [FALLBACK], Optional text As String = Label)\r\nConst Fallback As Long = 8\r\nEnd Sub\r\nSub Run()\r\nCall Accept()\r\nEnd Sub\r\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(output.diagnostics().is_empty());
        let constants: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.constant.declaration")
            .collect();
        assert_eq!(constants.len(), 3);
        let mut module_constants = 0;
        let mut local_constants = 0;
        for constant in constants {
            let parent = output
                .facts()
                .iter()
                .find(|fact| Some(fact.local_id()) == constant.parent())
                .unwrap();
            match parent.syntax_kind().as_str() {
                "vba.file.scope" => {
                    module_constants += 1;
                    assert_eq!(constant.depth(), 3);
                    assert!(matches!(
                        bytes_at(source, constant.span()),
                        b"Fallback = 7" | b"Label As String = \"text\""
                    ));
                }
                "vba.sub_function.declaration" => {
                    local_constants += 1;
                    assert_eq!(bytes_at(source, constant.span()), b"Fallback As Long = 8");
                }
                kind => panic!("a scalar constant has an authored file or procedure owner: {kind}"),
            }
            let definition = output
                .facts()
                .iter()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "vba.identifier.definition"
                        && fact.parent() == Some(constant.local_id())
                })
                .unwrap();
            assert_eq!(definition.depth(), constant.depth() + 1);
        }
        assert_eq!((module_constants, local_constants), (2, 1));
        let defaults: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "vba.default_constant_identifier.reference"
            })
            .collect();
        assert_eq!(defaults.len(), 2);
        for default in defaults {
            assert!(matches!(
                bytes_at(source, default.span()),
                b"[FALLBACK]" | b"Label"
            ));
            let formal = output
                .facts()
                .iter()
                .find(|fact| Some(fact.local_id()) == default.parent())
                .unwrap();
            assert_eq!(
                formal.syntax_kind().as_str(),
                "vba.omittable_parameter.declaration"
            );
            assert_eq!(default.depth(), formal.depth() + 1);
        }
        for (kind, written) in [
            ("vba.public.expression", b"Fallback = 7".as_slice()),
            (
                "vba.private.expression",
                b"Label As String = \"text\"".as_slice(),
            ),
        ] {
            assert!(
                output
                    .facts()
                    .iter()
                    .any(|fact| fact.syntax_kind().as_str() == kind
                        && bytes_at(source, fact.span()) == written)
            );
        }
        assert_eq!(output.facts(), parse(&provider, &request).facts());
        let exact_limits = limits(required);
        let exact_request = fixture.request(&exact_limits, language, Vec::new());
        let exact = parse(&provider, &exact_request);
        assert_eq!(
            exact
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.constant.declaration")
                .count(),
            3
        );
        let short_limits = limits(required - 1);
        let short_request = fixture.request(&short_limits, language, Vec::new());
        assert_eq!(
            execute_parse(
                &provider,
                &short_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1,
            })
        );
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled,
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        let stats = provider.stats();
        assert_eq!(stats.pooled_parsers, 0);
        assert_eq!(stats.checked_out_parsers, 0);
        assert_eq!(stats.cache.entries, 0);
    }
}

#[test]
fn explicit_calls_preserve_exact_application_and_callee_spans_without_native_work() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(CALL_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        let applications = ["Call Notify", "Call [Notify]( )"];
        let source = std::str::from_utf8(CALL_MODULE).unwrap();
        for (kind, spellings, offset) in [
            ("vba.application.reference", applications, 0),
            (
                "vba.invoked_identifier.reference",
                ["Notify", "[Notify]"],
                "Call ".len(),
            ),
        ] {
            let facts = output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == kind)
                .collect::<Vec<_>>();
            assert_eq!(facts.len(), applications.len());
            for (index, (fact, spelling)) in facts.iter().zip(spellings).enumerate() {
                let start = source.find(applications[index]).unwrap() + offset;
                assert_eq!(bytes_at(CALL_MODULE, fact.span()), spelling.as_bytes());
                assert_eq!(fact.span().start_byte(), u64::try_from(start).unwrap());
                assert_eq!(
                    fact.span().end_byte(),
                    u64::try_from(start + spelling.len()).unwrap()
                );
            }
        }
        let exact_limits = limits(required);
        let request = fixture.request(&exact_limits, language, Vec::new());
        let exact = parse(&provider, &request);
        for name in [b"Run".as_slice(), b"Notify"] {
            assert!(exact.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && bytes_at(CALL_MODULE, fact.span()) == name
            }));
        }
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn scalar_call_arguments_preserve_exact_reads_and_literal_source_without_native_work() {
    let source = std::str::from_utf8(CALL_ARGUMENT_MODULE).unwrap();
    let applications = [
        "Call Target(value, 7, \"a,\"\"b\"\":()\", True, [value])",
        "Call Target( value, value )",
        "Call Target()",
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(CALL_ARGUMENT_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        assert_eq!(request.source().source_ref(), &fixture.source);
        assert_eq!(
            fixture.source.content_hash(),
            fixture.snapshot.content_hash()
        );
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 10);
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(output.diagnostics().is_empty());
        let calls = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.application.reference")
            .collect::<Vec<_>>();
        let callees = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.invoked_identifier.reference")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), applications.len());
        assert_eq!(callees.len(), applications.len());
        for ((call, callee), written) in calls.iter().zip(callees).zip(applications) {
            let start = source.find(written).unwrap();
            assert_eq!(
                bytes_at(CALL_ARGUMENT_MODULE, call.span()),
                written.as_bytes()
            );
            assert_eq!(call.span().start_byte(), u64::try_from(start).unwrap());
            assert_eq!(
                call.span().end_byte(),
                u64::try_from(start + written.len()).unwrap()
            );
            assert_eq!(bytes_at(CALL_ARGUMENT_MODULE, callee.span()), b"Target");
            assert_eq!(
                callee.span().start_byte(),
                u64::try_from(start + "Call ".len()).unwrap()
            );
            assert_eq!(
                callee.span().end_byte(),
                u64::try_from(start + "Call Target".len()).unwrap()
            );
        }
        let first_start = source.find(applications[0]).unwrap();
        let second_start = source.find(applications[1]).unwrap();
        let mut expected_reads = vec![
            (
                first_start + applications[0].find("value").unwrap(),
                "value",
            ),
            (
                first_start + applications[0].find("[value]").unwrap(),
                "[value]",
            ),
        ];
        expected_reads.extend(
            applications[1]
                .match_indices("value")
                .map(|(offset, _)| (second_start + offset, "value")),
        );
        let reads = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.value_identifier.reference")
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), expected_reads.len());
        for (read, (start, written)) in reads.iter().zip(expected_reads) {
            assert_eq!(
                bytes_at(CALL_ARGUMENT_MODULE, read.span()),
                written.as_bytes()
            );
            assert_eq!(read.span().start_byte(), u64::try_from(start).unwrap());
            assert_eq!(
                read.span().end_byte(),
                u64::try_from(start + written.len()).unwrap()
            );
        }
        let local = output
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && bytes_at(CALL_ARGUMENT_MODULE, fact.span()) == b"value"
            })
            .unwrap();
        for (index, read) in reads.iter().enumerate() {
            assert_ne!(read.local_id(), local.local_id());
            assert!(
                reads
                    .iter()
                    .skip(index + 1)
                    .all(|other| other.local_id() != read.local_id())
            );
        }
        let literal = "\"a,\"\"b\"\":()\"";
        let strings = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "vba.string.string"
                    && bytes_at(CALL_ARGUMENT_MODULE, fact.span()) == literal.as_bytes()
            })
            .collect::<Vec<_>>();
        assert_eq!(strings.len(), 1);
        let string_start = source.find(literal).unwrap();
        assert_eq!(
            strings[0].span().start_byte(),
            u64::try_from(string_start).unwrap()
        );
        assert_eq!(
            strings[0].span().end_byte(),
            u64::try_from(string_start + literal.len()).unwrap()
        );
        for fact in output.facts() {
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
        }
        let repeated = parse(&provider, &request);
        assert_eq!(output.facts(), repeated.facts());
        assert_eq!(output.report(), repeated.report());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn scalar_call_arguments_do_not_change_required_identity_admission() {
    for arguments in ["", "1", "value, 7, \"x\", True, [value]"] {
        let source = format!("Sub Run()\nDim value As Long\nCall Target({arguments})\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 9);
            let exact_limits = limits(required);
            let request = fixture.request(&exact_limits, language, Vec::new());
            let output = parse(&provider, &request);
            assert_eq!(output.facts().len(), required);
            for name in [b"Run".as_slice(), b"value"] {
                assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                    == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == name));
            }
            let short_limits = limits(required - 1);
            let request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            let cancellation = deadline();
            assert!(cancellation.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn rhs_applications_preserve_exact_source_facts_without_native_work() {
    let source = std::str::from_utf8(APPLICATION_ASSIGNMENT_MODULE).unwrap();
    let statements = [
        ("result = items()", "result", "items()", "items"),
        (
            "Let [result] = [items]( 007 )",
            "[result]",
            "[items]( 007 )",
            "[items]",
        ),
        (
            "result = items(value, True, False, \"a,\"\"b\"\":()\", [value])",
            "result",
            "items(value, True, False, \"a,\"\"b\"\":()\", [value])",
            "items",
        ),
        (
            "result = items ( value )",
            "result",
            "items ( value )",
            "items",
        ),
        (
            "result = items( _\r\n value, _\r\n 7)",
            "result",
            "items( _\r\n value, _\r\n 7)",
            "items",
        ),
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(APPLICATION_ASSIGNMENT_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            APPLICATION_ASSIGNMENT_MODULE.len()
        );
        assert!(output.diagnostics().is_empty());
        let scope = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.scope")
            .unwrap();
        assert_eq!(scope.depth(), 3);
        assert_eq!(scope.kind(), SyntaxFactKind::Scope);
        let procedure = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
            .unwrap();
        assert_eq!(procedure.parent(), Some(scope.local_id()));
        assert_eq!(procedure.depth(), 4);
        assert_eq!(procedure.kind(), SyntaxFactKind::Declaration);
        for (statement, destination, application, callee) in statements {
            let statement_start = source.find(statement).unwrap();
            for (syntax, written) in [
                ("vba.let_identifier.reference", destination),
                ("vba.application.reference", application),
                ("vba.invoked_get_identifier.reference", callee),
            ] {
                let start = statement_start + statement.find(written).unwrap();
                let matching = output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matching.len(), 1, "{language}: {statement}: {syntax}");
                let fact = matching[0];
                assert_eq!(fact.kind(), SyntaxFactKind::Occurrence);
                assert_eq!(
                    fact.span().end_byte(),
                    u64::try_from(start + written.len()).unwrap()
                );
                assert_eq!(
                    bytes_at(APPLICATION_ASSIGNMENT_MODULE, fact.span()),
                    written.as_bytes()
                );
                assert_eq!(fact.parent(), Some(procedure.local_id()));
                assert_eq!(fact.depth(), 5);
            }
        }
        for syntax in [
            "vba.application.reference",
            "vba.invoked_get_identifier.reference",
            "vba.let_identifier.reference",
        ] {
            assert_eq!(
                output
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == syntax)
                    .count(),
                statements.len()
            );
        }
        let expected_reads = [
            (statements[2].0, "value", 0),
            (statements[2].0, "[value]", 0),
            (statements[3].0, "value", 0),
            (statements[4].0, "value", 0),
        ];
        let reads = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.value_identifier.reference")
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), expected_reads.len());
        for (statement, written, ordinal) in expected_reads {
            let start = source.find(statement).unwrap()
                + statement.match_indices(written).nth(ordinal).unwrap().0;
            let read = reads
                .iter()
                .find(|fact| fact.span().start_byte() == u64::try_from(start).unwrap())
                .unwrap();
            assert_eq!(
                read.span().end_byte(),
                u64::try_from(start + written.len()).unwrap()
            );
            assert_eq!(
                bytes_at(APPLICATION_ASSIGNMENT_MODULE, read.span()),
                written.as_bytes()
            );
            assert_eq!(read.parent(), Some(procedure.local_id()));
            assert_eq!(read.depth(), 5);
            assert_eq!(read.kind(), SyntaxFactKind::Occurrence);
        }
        let literal = "\"a,\"\"b\"\":()\"";
        let strings = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "vba.string.string"
                    && bytes_at(APPLICATION_ASSIGNMENT_MODULE, fact.span()) == literal.as_bytes()
            })
            .collect::<Vec<_>>();
        assert_eq!(strings.len(), 1);
        assert_eq!(strings[0].kind(), SyntaxFactKind::StringLiteral);
        let string_start = source.find(literal).unwrap();
        assert_eq!(
            strings[0].span().start_byte(),
            u64::try_from(string_start).unwrap()
        );
        assert_eq!(
            strings[0].span().end_byte(),
            u64::try_from(string_start + literal.len()).unwrap()
        );
        assert_eq!(strings[0].parent(), Some(procedure.local_id()));
        assert_eq!(strings[0].depth(), 5);
        for literal in ["007", "True", "False", "7)"] {
            let start = source.find(literal).unwrap();
            assert!(!output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".reference")
                    && fact.span().start_byte() == u64::try_from(start).unwrap()
            }));
        }
        for fact in output.facts() {
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
        }
        let repeated = parse(&provider, &request);
        assert_eq!(repeated.facts(), output.facts());
        assert_eq!(repeated.report(), output.report());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn rhs_application_arguments_do_not_change_required_identity_or_cancellation() {
    for arguments in ["", "007", "value, 7, True, False, \"written\", [value]"] {
        let source = format!(
            "Private items(2) As Long\nSub Run()\nDim result As Variant\nDim value As Long\nresult = items({arguments})\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let full_request = fixture.request(&full_limits, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&full_request, &deadline())
                .unwrap();
            assert_eq!(required, 13);
            let full = parse(&provider, &full_request);
            assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
            assert!(full.facts().len() > required + 2);
            for capacity in [required, required + 2] {
                let clipped_limits = limits(capacity);
                let request = fixture.request(&clipped_limits, language, Vec::new());
                let clipped = parse(&provider, &request);
                assert_eq!(clipped.facts().len(), capacity);
                assert_eq!(
                    clipped.report().coverage().status(),
                    CoverageStatus::Bounded
                );
                assert!(
                    clipped.diagnostics().iter().any(|diagnostic| {
                        diagnostic.code().as_str() == "syntax-extraction-limit"
                    })
                );
                for name in ["items", "Run", "result", "value"] {
                    assert!(clipped.facts().iter().any(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && bytes_at(source.as_bytes(), fact.span()) == name.as_bytes()
                    }));
                }
                assert_eq!(
                    provider
                        .required_syntax_fact_count(&request, &deadline())
                        .unwrap(),
                    required
                );
            }
            let short_limits = limits(required - 1);
            let short_request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &short_request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            let cancelled = deadline();
            assert!(cancelled.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &full_request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(parse(&provider, &full_request).facts(), full.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn richer_rhs_applications_keep_whole_native_fallback() {
    for expression in [
        "Target(slot:=1)",
        "Target(,1)",
        "Target(ByVal value)",
        "Target(1)(2)",
        "owner.Target(1)",
        "Target(1E+)",
        "Line(1)",
    ] {
        let source = format!(
            "Sub Run()\nDim result As Variant\nDim value As Long\nresult = {expression}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let output = parse(
                &provider,
                &fixture.request(&full_limits, language, Vec::new()),
            );
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {expression}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == b"Run"
            }));
        }
    }
}

#[test]
fn implicit_calls_match_native_reference_identity_for_the_same_complete_source() {
    for newline in ["\n", "\r\n"] {
        for statement in [
            "Target",
            "[Target]",
            "ExitSub",
            "ExitFunction",
            "Exit0Sub",
            "Exit0Function",
            "Dim0first",
            "third",
            "Shared value",
            "Target value",
            "Target _",
            "Target +value",
            "Target -value",
            "Target value, other",
            "Target value + 1, Inner(other) * -2",
            "Target Not True, -other",
            "Target (value + 1), other",
            "[Target] (value), Inner(other)",
            "Target (value) * -2",
            "Target (value) + Inner(other)",
            "Target value = other",
            "Target (value) + 1 = other",
            "Target -(value) = other",
            "Target Not (value) = other",
            "Target other, (value) = other",
            "Target value And (other) = value",
            "Target \"(value)\"",
            "[Name] value",
            "[Line] value",
            "Target \"a,\"\"b\"\":()\", True, &H2, 1.25E-2",
        ] {
            let source = format!(
                "Sub Run(ByVal value As Long, ByVal other As Long){newline}  {statement} ' ignored End Sub: Target(){newline}value=1{newline}End Sub{newline}"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let first_party = provider();
                let native = provider();
                let budget = limits(4096);
                let first_request = fixture.request(&budget, language, Vec::new());
                let native_request = fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                );
                let first_output = parse(&first_party, &first_request);
                let native_output = parse(&native, &native_request);
                assert_eq!(
                    first_output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert!(
                    first_output.diagnostics().is_empty(),
                    "{language}: {statement}"
                );
                assert!(
                    native_output.diagnostics().is_empty(),
                    "native {language}: {statement}"
                );
                assert_eq!(
                    first_party.stats().pooled_parsers,
                    0,
                    "{language}: {statement}"
                );
                assert_eq!(first_party.stats().checked_out_parsers, 0);
                assert_eq!(first_party.stats().cache.entries, 0);
                assert!(native.stats().pooled_parsers > 0);
                let references = |output: &ParseOutput| {
                    let mut records = output
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.kind() == SyntaxFactKind::Occurrence
                                && fact.syntax_kind().as_str().ends_with(".reference")
                        })
                        .map(|fact| {
                            (
                                fact.span().start_byte(),
                                fact.span().end_byte(),
                                fact.syntax_kind().as_str().to_owned(),
                                fact.local_id(),
                                fact.parent(),
                                fact.depth(),
                            )
                        })
                        .collect::<Vec<_>>();
                    records.sort();
                    records
                };
                assert_eq!(
                    references(&first_output),
                    references(&native_output),
                    "{language}: {statement}"
                );
                let start = source.find(statement).unwrap();
                for (syntax, length) in [
                    ("vba.application.reference", statement.len()),
                    (
                        "vba.invoked_identifier.reference",
                        statement.split_ascii_whitespace().next().unwrap().len(),
                    ),
                ] {
                    assert_eq!(
                        first_output
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.syntax_kind().as_str() == syntax
                                    && fact.span().start_byte() == u64::try_from(start).unwrap()
                                    && fact.span().end_byte()
                                        == u64::try_from(start + length).unwrap()
                            })
                            .count(),
                        1,
                        "{language}: {statement}: {syntax}"
                    );
                }
            }
        }
        let statement =
            format!("[Target] _{newline} (value _{newline} + 1), _{newline} Inner(other) * -2");
        let source = format!(
            "Sub Run(ByVal value As Long, ByVal other As Long){newline}{statement}{newline}End Sub{newline}"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                == "vba.application.reference"
                && bytes_at(source.as_bytes(), fact.span()) == statement.as_bytes()));
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn implicit_call_raw_continuation_boundary_matches_forced_native_without_fabricated_arguments() {
    for newline in ["\n", "\r\n"] {
        let source = format!("Sub Run(){newline}Target _{newline}{newline}End Sub{newline}");
        let Err(rootlight_vba::ModuleError::Unsupported { range }) =
            rootlight_vba::module_inventory(
                source.as_bytes(),
                rootlight_vba::ModuleLimits::new(source.len(), 4096, 4096),
                &mut || Ok::<(), ()>(()),
            )
        else {
            panic!("the complete raw continuation must stay on whole-module native fallback");
        };
        assert_eq!(&source[range], format!("Target _{newline}"));
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let fallback = provider();
            let forced_native = provider();
            let budget = limits(4096);
            let fallback_output = parse(&fallback, &fixture.request(&budget, language, Vec::new()));
            let native_output = parse(
                &forced_native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert!(fallback.stats().pooled_parsers > 0);
            assert!(forced_native.stats().pooled_parsers > 0);
            assert_eq!(fallback_output.facts(), native_output.facts());
            assert!(
                !native_output
                    .facts()
                    .iter()
                    .any(
                        |fact| fact.syntax_kind().as_str() == "vba.value_identifier.reference"
                            && bytes_at(source.as_bytes(), fact.span()) == b"_"
                    )
            );
            assert_eq!(
                native_output
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str()
                        == "vba.invoked_identifier.reference"
                        && bytes_at(source.as_bytes(), fact.span()) == b"Target")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn nothing_values_preserve_exact_native_facts_and_source_identity() {
    let statements = [
        "Set item = nOtHiNg",
        "Let item = Nothing",
        "item = NOTHING",
        "Call Consume(Nothing, [Nothing], \"Nothing\", Nothing0, Nothing_)",
        "item = Outer(Nothing)",
        "item = Outer(Nothing, Inner(Nothing))",
        "Call Consume((Nothing), Inner(Nothing))",
        "item = (Nothing)",
        "item = Not (item Is Nothing)",
        "item = Nothing + 1",
        "item = Not Nothing",
        "Set item = Outer((Nothing), item Is Nothing)",
        "Consume Nothing",
        "Outer (Nothing)",
    ];
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "Sub Run(ByVal item As Object){newline}Dim [Nothing] As Object, Nothing0 As Object, Nothing_ As Object{newline}{}{newline}End Sub{newline}",
            statements.join(newline)
        );
        let bytes = source.as_bytes();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(bytes);
            let first_party = provider();
            let native = provider();
            let budget = limits(4096);
            let request = fixture.request(&budget, language, Vec::new());
            let required = first_party
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            let output = parse(&first_party, &request);
            let reference = parse(
                &native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert_eq!(output.facts(), reference.facts(), "{language}: {source}");
            assert_eq!(output.diagnostics(), reference.diagnostics());
            assert!(output.diagnostics().is_empty());
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                bytes.len()
            );
            assert_eq!(
                fixture.source.content_hash(),
                rootlight_ids::content_hash(bytes)
            );
            assert!(native.stats().pooled_parsers > 0);
            let mut bare_literals = 0;
            for (start, _) in source.to_ascii_lowercase().match_indices("nothing") {
                let preceding = start.checked_sub(1).and_then(|index| bytes.get(index));
                let following = bytes.get(start + 7);
                if preceding.is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'[' | b'"')
                }) || following.is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    continue;
                }
                bare_literals += 1;
                let literal_start = u64::try_from(start).unwrap();
                assert!(
                    !output.facts().iter().any(|fact| {
                        fact.kind() == SyntaxFactKind::Occurrence
                            && fact.span().start_byte() >= literal_start
                            && fact.span().end_byte() <= literal_start + 7
                    }),
                    "no literal occurrence: {language}: {start}"
                );
            }
            assert_eq!(bare_literals, 17);
            for token in [b"[Nothing]".as_slice(), b"Nothing0", b"Nothing_"] {
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "vba.value_identifier.reference"
                                && bytes_at(bytes, fact.span()) == token
                        })
                        .count(),
                    1
                );
            }
            assert_eq!(
                output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == "vba.string.string"
                            && bytes_at(bytes, fact.span()) == b"\"Nothing\""
                    })
                    .count(),
                1
            );
            let exact = parse(
                &first_party,
                &fixture.request(&limits(required), language, Vec::new()),
            );
            assert_eq!(exact.facts().len(), required);
            let identity = |parsed: &ParseOutput| {
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.syntax_kind().as_str(),
                            "vba.file.root"
                                | "vba.file.module"
                                | "vba.file.scope"
                                | "vba.sub_function.declaration"
                                | "vba.sub_function.scope"
                                | "vba.sub_function.signature"
                                | "vba.parameter.declaration"
                                | "vba.variable.declaration"
                                | "vba.identifier.definition"
                        )
                    })
                    .map(|fact| {
                        let parent = fact.parent().map(|id| {
                            let parent = parsed
                                .facts()
                                .iter()
                                .find(|candidate| candidate.local_id() == id)
                                .unwrap();
                            (
                                parent.kind(),
                                parent.span(),
                                parent.syntax_kind().as_str().to_owned(),
                                parent.depth(),
                            )
                        });
                        (
                            fact.kind(),
                            fact.span(),
                            fact.syntax_kind().as_str().to_owned(),
                            fact.depth(),
                            parent,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(identity(&output).len(), required);
            assert_eq!(identity(&exact), identity(&output));
            assert_eq!(exact.report().coverage().status(), CoverageStatus::Bounded);
            assert!(
                exact
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
            );
            let short = limits(required - 1);
            assert_eq!(
                execute_parse(
                    &first_party,
                    &fixture.request(&short, language, Vec::new()),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1
                })
            );
            assert_eq!(first_party.stats().pooled_parsers, 0);
            assert_eq!(first_party.stats().checked_out_parsers, 0);
            assert_eq!(first_party.stats().cache.entries, 0);
        }
    }
}

#[test]
fn nothing_initializer_and_richer_tails_preserve_whole_native_fallback() {
    for tail in [
        "Const Seed = Nothing\n",
        "Sub Run(Optional item As Object = Nothing)\nEnd Sub\n",
        "Sub Run()\nDim slots(Nothing) As Long\nEnd Sub\n",
        "Sub Run()\nSet sink = Nothing()\nEnd Sub\n",
        "Sub Run()\nSet sink = Nothing.Member\nEnd Sub\n",
        "Sub Run()\nCall Consume(Nothing.Member)\nEnd Sub\n",
        "Sub Run()\nsink = Outer(Nothing, value:=1)\nEnd Sub\n",
        "Sub Run()\nsink = (Nothing) extra\nEnd Sub\n",
    ] {
        let source = format!("Sub First()\nSet sink = Nothing\nEnd Sub\n{tail}");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let routed = provider();
            let native = provider();
            let budget = limits(4096);
            let output = parse(&routed, &fixture.request(&budget, language, Vec::new()));
            let reference = parse(
                &native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert!(routed.stats().pooled_parsers > 0, "{language}: {tail}");
            assert_eq!(routed.stats().checked_out_parsers, 0);
            assert_eq!(output.facts(), reference.facts(), "{language}: {tail}");
            assert_eq!(
                output.diagnostics(),
                reference.diagnostics(),
                "{language}: {tail}"
            );
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == b"First"
            }));
        }
    }
}

#[test]
fn authored_assignment_modes_preserve_exact_native_facts_and_source_identity() {
    for newline in ["\n", "\r\n"] {
        for language in ["vba", "visual-basic-classic"] {
            let source = format!(
                "Sub Run(ByVal value As Long){newline}Dim sink As Variant{newline}sEt [sink] = value{newline}Set sink = Buffer(value){newline}Set _{newline} sink = Outer(Inner(value + 1), 2) + value{newline}Set sink = (value < 1 And other > 2 And flag){newline}Let sink = value{newline}sink = value{newline}[Set] = [Let]{newline}End Sub{newline}"
            );
            let fixture = Fixture::new(source.as_bytes());
            let first_party = provider();
            let native = provider();
            let budget = limits(4096);
            let request = fixture.request(&budget, language, Vec::new());
            let required = first_party
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            let output = parse(&first_party, &request);
            let reference = parse(
                &native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert_eq!(first_party.stats().pooled_parsers, 0);
            assert_eq!(first_party.stats().checked_out_parsers, 0);
            assert_eq!(first_party.stats().cache.entries, 0);
            assert!(native.stats().pooled_parsers > 0);
            assert_eq!(output.facts(), reference.facts(), "{language}: {source}");
            assert_eq!(output.diagnostics(), reference.diagnostics());
            assert!(output.diagnostics().is_empty());
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert_eq!(
                fixture.source.content_hash(),
                rootlight_ids::content_hash(source.as_bytes())
            );
            let destinations: Vec<_> = output
                .facts()
                .iter()
                .filter(|fact| {
                    matches!(
                        fact.syntax_kind().as_str(),
                        "vba.set_identifier.reference" | "vba.let_identifier.reference"
                    )
                })
                .collect();
            assert_eq!(destinations.len(), 7);
            assert_eq!(
                destinations
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == "vba.set_identifier.reference")
                    .count(),
                4
            );
            for fact in destinations {
                assert!(matches!(
                    bytes_at(source.as_bytes(), fact.span()),
                    b"sink" | b"[sink]" | b"[Set]"
                ));
                assert_eq!(fact.span().file(), fixture.source.span().file());
            }
            let exact_budget = limits(required);
            let exact = parse(
                &first_party,
                &fixture.request(&exact_budget, language, Vec::new()),
            );
            assert_eq!(exact.facts().len(), required);
            let mandatory_identity = |parsed: &ParseOutput| {
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.syntax_kind().as_str(),
                            "vba.file.root"
                                | "vba.file.module"
                                | "vba.file.scope"
                                | "vba.sub_function.declaration"
                                | "vba.sub_function.scope"
                                | "vba.sub_function.signature"
                                | "vba.parameter.declaration"
                                | "vba.variable.declaration"
                                | "vba.identifier.definition"
                        )
                    })
                    .map(|fact| {
                        let parent = fact.parent().map(|id| {
                            let parent = parsed
                                .facts()
                                .iter()
                                .find(|candidate| candidate.local_id() == id)
                                .unwrap();
                            (
                                parent.kind(),
                                parent.span(),
                                parent.syntax_kind().as_str().to_owned(),
                                parent.depth(),
                            )
                        });
                        (
                            fact.kind(),
                            fact.span(),
                            fact.syntax_kind().as_str().to_owned(),
                            fact.depth(),
                            parent,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let mandatory = mandatory_identity(&output);
            assert_eq!(mandatory.len(), required);
            assert_eq!(mandatory_identity(&exact), mandatory);
            assert_eq!(exact.report().coverage().status(), CoverageStatus::Bounded);
            assert!(
                exact
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
            );
            assert_eq!(
                first_party
                    .required_syntax_fact_count(
                        &fixture.request(&exact_budget, language, Vec::new()),
                        &deadline()
                    )
                    .unwrap(),
                required
            );
            let short_budget = limits(required - 1);
            assert_eq!(
                execute_parse(
                    &first_party,
                    &fixture.request(&short_budget, language, Vec::new()),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            assert_eq!(first_party.stats().pooled_parsers, 0);
        }
    }
}

#[test]
fn set_logical_arguments_preserve_source_recovered_native_application_facts() {
    for operator in ["And", "Or", "Xor", "Eqv", "Imp"] {
        for newline in ["\n", "\r\n"] {
            let expression = format!("Outer(left < right {operator} third > last)");
            let source =
                format!("Sub Run(){newline}Set sink = {expression}{newline}End Sub{newline}");
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let first_party = provider();
                let native = provider();
                let budget = limits(4096);
                let actual = parse(
                    &first_party,
                    &fixture.request(&budget, language, Vec::new()),
                );
                let reference = parse(
                    &native,
                    &fixture.request(
                        &budget,
                        language,
                        vec![IncludedRange::new(
                            fixture.source.span(),
                            LanguageId::new(language).unwrap(),
                        )],
                    ),
                );
                assert_eq!(actual.facts(), reference.facts(), "{language}: {source}");
                assert_eq!(actual.diagnostics(), reference.diagnostics());
                assert!(actual.diagnostics().is_empty());
                assert_eq!(
                    actual.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert_eq!(
                    actual.report().coverage().covered_source_bytes(),
                    source.len()
                );
                assert_eq!(first_party.stats().pooled_parsers, 0);
                assert_eq!(first_party.stats().checked_out_parsers, 0);
                assert_eq!(first_party.stats().cache.entries, 0);
                assert!(native.stats().pooled_parsers > 0);
                let start = u64::try_from(source.find(&expression).unwrap()).unwrap();
                assert_eq!(
                    actual
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "vba.application.reference"
                                && fact.span().start_byte() == start
                                && fact.span().end_byte()
                                    == start + u64::try_from(expression.len()).unwrap()
                        })
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn authored_set_octal_sources_remain_complete_without_native_parser_work() {
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "Sub Run(){newline}Dim sink As Variant{newline}sEt _{newline} [sink] = &O017{newline}End Sub{newline}"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let first_party = provider();
            let budget = limits(4096);
            let output = parse(
                &first_party,
                &fixture.request(&budget, language, Vec::new()),
            );
            assert!(output.diagnostics().is_empty());
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert_eq!(first_party.stats().pooled_parsers, 0);
            assert_eq!(first_party.stats().checked_out_parsers, 0);
            assert_eq!(first_party.stats().cache.entries, 0);
            let destination = output
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "vba.set_identifier.reference")
                .unwrap();
            assert_eq!(bytes_at(source.as_bytes(), destination.span()), b"[sink]");
            let literal_start = u64::try_from(source.find("&O017").unwrap()).unwrap();
            assert!(!output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".reference")
                    && fact.span().start_byte() >= literal_start
                    && fact.span().end_byte() <= literal_start + 5
            }));
            assert_eq!(
                fixture.source.content_hash(),
                rootlight_ids::content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn unsupported_set_tails_preserve_whole_native_facts_and_diagnostics() {
    for statement in [
        "Set sink = Nothing.Member",
        "Set sink = Me",
        "Set sink = New Worker",
        "Set sink = AddressOf Worker",
        "Set holder.sink = value",
        "Set sink(0) = value",
        "Set sink = Outer(value:=1)",
        "Set sink = Outer(,1)",
        "Set sink = Outer(1)(2)",
        "Set sink = value.Member",
        "Set sink = left < 1 And right > 2",
        "Set sink = left = middle = right",
        "Set sink = (left = middle = right)",
        "Set sink = Outer(left = middle = right)",
        "Set sink = Not left = middle + right = last",
        "Set sink = -(value + 1) extra",
    ] {
        let source = format!(
            "Sub First()\nSet sink = value\nEnd Sub\nSub Run()\nSet sink = Buffer(0)\n{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let routed = provider();
            let native = provider();
            let budget = limits(4096);
            let output = parse(&routed, &fixture.request(&budget, language, Vec::new()));
            let reference = parse(
                &native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert!(routed.stats().pooled_parsers > 0, "{language}: {statement}");
            assert_eq!(routed.stats().checked_out_parsers, 0);
            assert_eq!(output.facts(), reference.facts(), "{language}: {statement}");
            assert_eq!(
                output.diagnostics(),
                reference.diagnostics(),
                "{language}: {statement}"
            );
            assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                == "vba.identifier.definition"
                && bytes_at(source.as_bytes(), fact.span()) == b"First"));
        }
    }
}

#[test]
fn parenthesized_statements_preserve_exact_native_source_roles() {
    for newline in ["\n", "\r\n"] {
        for (statement, grouped_call) in [
            ("Target (value)".to_owned(), true),
            ("Target (value + 1)".to_owned(), true),
            ("[Target] ((value))".to_owned(), true),
            ("Target (Inner(value, other))".to_owned(), true),
            ("Target (\"a,b\")".to_owned(), true),
            (format!("[Target] _{newline} (value _{newline} + 1)"), true),
            ("Target(value, other)".to_owned(), false),
            ("Target (value, other)".to_owned(), false),
            ("Target()".to_owned(), false),
            ("[Target] ( )".to_owned(), false),
            ("Target ((value + 1), Inner(other) * -2)".to_owned(), false),
            (
                format!("[Target] _{newline} (value, _{newline} Inner(other))"),
                false,
            ),
        ] {
            let source = format!(
                "Sub First(){newline}Target value+1{newline}End Sub{newline}Sub Run(ByVal value As Long){newline}{statement} ' ignored (value), other{newline}End Sub{newline}"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let first_party = provider();
                let native = provider();
                let budget = limits(4096);
                let output = parse(
                    &first_party,
                    &fixture.request(&budget, language, Vec::new()),
                );
                let reference = parse(
                    &native,
                    &fixture.request(
                        &budget,
                        language,
                        vec![IncludedRange::new(
                            fixture.source.span(),
                            LanguageId::new(language).unwrap(),
                        )],
                    ),
                );
                assert_eq!(
                    first_party.stats().pooled_parsers,
                    0,
                    "{language}: {statement}"
                );
                assert_eq!(first_party.stats().checked_out_parsers, 0);
                assert_eq!(first_party.stats().cache.entries, 0);
                assert!(native.stats().pooled_parsers > 0);
                assert_eq!(output.facts(), reference.facts(), "{language}: {statement}");
                assert_eq!(output.diagnostics(), reference.diagnostics());
                assert!(output.diagnostics().is_empty(), "{language}: {statement}");
                assert_eq!(
                    output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert_eq!(
                    output.report().coverage().covered_source_bytes(),
                    source.len()
                );
                let start = source.find(&statement).unwrap();
                let callee = if statement.starts_with('[') {
                    "[Target]"
                } else {
                    "Target"
                };
                let matches_callee = |fact: &&SyntaxFact| {
                    fact.span().start_byte() == u64::try_from(start).unwrap()
                        && fact.span().end_byte() == u64::try_from(start + callee.len()).unwrap()
                };
                let (expected, excluded) = if grouped_call {
                    (
                        "vba.invoked_identifier.reference",
                        "vba.invoked_get_identifier.reference",
                    )
                } else {
                    (
                        "vba.invoked_get_identifier.reference",
                        "vba.invoked_identifier.reference",
                    )
                };
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(matches_callee)
                        .filter(|fact| fact.syntax_kind().as_str() == expected)
                        .count(),
                    1,
                    "{language}: {statement}"
                );
                assert!(
                    !output
                        .facts()
                        .iter()
                        .filter(matches_callee)
                        .any(|fact| fact.syntax_kind().as_str() == excluded)
                );
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "vba.application.reference"
                                && fact.span().start_byte() == u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(start + statement.len()).unwrap()
                        })
                        .count(),
                    1,
                    "{language}: {statement}"
                );
            }
        }
    }
}

#[test]
fn parenthesized_statement_arguments_preserve_group_and_application_boundaries() {
    for newline in ["\n", "\r\n"] {
        for (statement, expected_arguments, grouped_call) in [
            ("Target()".to_owned(), Vec::new(), false),
            (
                "Target (value)".to_owned(),
                vec!["(value)".to_owned()],
                true,
            ),
            (
                "[Target] ((value))".to_owned(),
                vec!["((value))".to_owned()],
                true,
            ),
            (
                "Target (value + 1, Inner(other) * -2)".to_owned(),
                vec!["value + 1".to_owned(), "Inner(other) * -2".to_owned()],
                false,
            ),
            (
                format!("[Target] _{newline} (value _{newline} + 1, _{newline} Inner(other))"),
                vec![format!("value _{newline} + 1"), "Inner(other)".to_owned()],
                false,
            ),
        ] {
            let source = format!(
                "Sub Run(ByVal value As Long, ByVal other As Long){newline}{statement} ' ignored (other){newline}End Sub{newline}"
            );
            let inventory = rootlight_vba::module_inventory(
                source.as_bytes(),
                rootlight_vba::ModuleLimits::new(source.len(), 4096, 4096),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let [written] = inventory.procedures()[0].statements() else {
                panic!("one complete parenthesized statement is required");
            };
            let (statement_range, callee_range, parentheses, arguments, expressions, actual_call) =
                match written {
                    rootlight_vba::ModuleBodyStatement::ValueApplication(application) => (
                        application.statement(),
                        application.callee(),
                        application.arguments(),
                        application.argument_expressions(),
                        application.inventory(),
                        false,
                    ),
                    rootlight_vba::ModuleBodyStatement::ExpressionCall(call) => (
                        call.statement(),
                        call.callee(),
                        call.arguments(),
                        call.argument_expressions(),
                        call.inventory(),
                        true,
                    ),
                    rootlight_vba::ModuleBodyStatement::Declaration(_)
                    | rootlight_vba::ModuleBodyStatement::Variables(_)
                    | rootlight_vba::ModuleBodyStatement::Assignment(_)
                    | rootlight_vba::ModuleBodyStatement::ApplicationAssignment(_)
                    | rootlight_vba::ModuleBodyStatement::ExpressionAssignment(_)
                    | rootlight_vba::ModuleBodyStatement::Call(_)
                    | rootlight_vba::ModuleBodyStatement::ProcedureExit(_) => {
                        panic!("complete parenthesized statement expected: {statement}")
                    }
                };
            assert_eq!(actual_call, grouped_call, "{statement}");
            let start = source.find(&statement).unwrap();
            let end = start + statement.len();
            let callee = if statement.starts_with('[') {
                "[Target]"
            } else {
                "Target"
            };
            assert_eq!(statement_range, start..end);
            assert_eq!(&source[callee_range.clone()], callee);
            assert_eq!(
                &source[parentheses.clone()],
                &statement[statement.find('(').unwrap()..]
            );
            assert_eq!(source.as_bytes()[parentheses.start], b'(');
            assert_eq!(source.as_bytes()[parentheses.end - 1], b')');
            assert_eq!(arguments.len(), expected_arguments.len());
            for (range, expected) in arguments.iter().zip(&expected_arguments) {
                if grouped_call {
                    assert_eq!(range, &parentheses);
                } else {
                    assert!(parentheses.start < range.start && range.end < parentheses.end);
                }
                assert_eq!(&source[range.clone()], expected);
            }
            let root = &expressions.applications()[0];
            assert_eq!(root.application(), statement_range);
            assert_eq!(root.callee(), callee_range);
            assert_eq!(root.arguments(), parentheses);
            assert_eq!(root.argument_expressions(), arguments);
        }
    }
}

#[test]
fn implicit_grouped_assignment_candidates_preserve_exact_native_classification() {
    for newline in ["\n", "\r\n"] {
        // Both language profiles select the call reading for the logical RHS.
        // Preserve exact source classification without inferring runtime legality.
        for (statement, expected_callee) in [
            (
                "Target (0) = value".to_owned(),
                "vba.invoked_let_identifier.reference",
            ),
            (
                "Target ((0)) = value".to_owned(),
                "vba.invoked_let_identifier.reference",
            ),
            (
                "[Target] (0) = value + 1".to_owned(),
                "vba.invoked_let_identifier.reference",
            ),
            (
                "Target (0) = value And True".to_owned(),
                "vba.invoked_identifier.reference",
            ),
            (
                "Target (Inner(value)) = value".to_owned(),
                "vba.invoked_let_identifier.reference",
            ),
            (
                format!("Target _{newline} (0) _{newline} = value"),
                "vba.invoked_let_identifier.reference",
            ),
        ] {
            let source = format!(
                "Sub Target(ByVal flag As Boolean){newline}End Sub{newline}Sub Run(ByVal value As Long){newline}{statement}{newline}End Sub{newline}"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let fallback = provider();
                let native = provider();
                let budget = limits(4096);
                let output = parse(&fallback, &fixture.request(&budget, language, Vec::new()));
                let reference = parse(
                    &native,
                    &fixture.request(
                        &budget,
                        language,
                        vec![IncludedRange::new(
                            fixture.source.span(),
                            LanguageId::new(language).unwrap(),
                        )],
                    ),
                );
                assert!(
                    fallback.stats().pooled_parsers > 0,
                    "{language}: {statement}"
                );
                assert_eq!(fallback.stats().checked_out_parsers, 0);
                assert_eq!(output.facts(), reference.facts(), "{language}: {statement}");
                assert_eq!(output.diagnostics(), reference.diagnostics());
                assert_eq!(
                    output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                let start = source.find(&statement).unwrap();
                let callee = statement.split_ascii_whitespace().next().unwrap();
                let start = u64::try_from(start).unwrap();
                let end = start + u64::try_from(callee.len()).unwrap();
                let actual = output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.span().start_byte() == start && fact.span().end_byte() == end
                    })
                    .map(|fact| {
                        (
                            fact.kind(),
                            fact.syntax_kind().as_str().to_owned(),
                            start,
                            end,
                        )
                    })
                    .collect::<Vec<_>>();
                let expected = vec![(
                    SyntaxFactKind::Occurrence,
                    expected_callee.to_owned(),
                    start,
                    end,
                )];
                assert_eq!(actual, expected, "{language}: {statement}");
            }
        }
    }
}

#[test]
fn implicit_and_value_application_budgets_preserve_mandatory_identity_without_native_retry() {
    for source in [IMPLICIT_CALL_MODULE, STATEMENT_VALUE_APPLICATION_MODULE] {
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source);
            let provider = provider();
            let base = limits(4096);
            let request = fixture.request(&base, language, Vec::new());
            let full = parse(&provider, &request);
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 11);
            assert_eq!(full.report().resources().syntax_nodes(), 18);
            let mandatory_identity = |output: &ParseOutput| {
                output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.syntax_kind().as_str(),
                            "vba.file.root"
                                | "vba.file.module"
                                | "vba.file.scope"
                                | "vba.sub_function.declaration"
                                | "vba.sub_function.scope"
                                | "vba.sub_function.signature"
                                | "vba.parameter.declaration"
                                | "vba.identifier.definition"
                        )
                    })
                    .map(|fact| {
                        let parent = fact.parent().map(|id| {
                            let parent = output
                                .facts()
                                .iter()
                                .find(|candidate| candidate.local_id() == id)
                                .expect("mandatory parent remains in the output");
                            (
                                parent.kind(),
                                parent.span(),
                                parent.syntax_kind().as_str().to_owned(),
                                parent.depth(),
                            )
                        });
                        (
                            fact.kind(),
                            fact.span(),
                            fact.syntax_kind().as_str().to_owned(),
                            fact.depth(),
                            parent,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let mandatory = mandatory_identity(&full);
            assert_eq!(mandatory.len(), required);
            for capacity in required..=full.facts().len() {
                let budget = limits(capacity);
                let bounded = fixture.request(&budget, language, Vec::new());
                let output = parse(&provider, &bounded);
                assert_eq!(output.facts().len(), capacity);
                assert_eq!(
                    provider
                        .required_syntax_fact_count(&bounded, &deadline())
                        .unwrap(),
                    required
                );
                assert_eq!(
                    output.report().coverage().status(),
                    if capacity == full.facts().len() {
                        CoverageStatus::Complete
                    } else {
                        CoverageStatus::Bounded
                    }
                );
                assert_eq!(
                    output.diagnostics().iter().any(|diagnostic| {
                        diagnostic.code().as_str() == "syntax-extraction-limit"
                    }),
                    capacity < full.facts().len()
                );
                // Local ordinals are assigned after optional retention. Compare all
                // mandatory source records and their owning source records instead.
                assert_eq!(mandatory_identity(&output), mandatory);
                for (index, fact) in output.facts().iter().enumerate() {
                    assert_eq!(fact.local_id(), u64::try_from(index + 1).unwrap());
                    assert_eq!(fact.span().file(), fixture.source.span().file());
                    if let Some(id) = fact.parent() {
                        assert_ne!(id, fact.local_id());
                        let parent = output
                            .facts()
                            .iter()
                            .find(|candidate| candidate.local_id() == id)
                            .expect("every local parent identity is retained");
                        assert_eq!(fact.depth(), parent.depth() + 1);
                        assert!(parent.span().start_byte() <= fact.span().start_byte());
                        assert!(parent.span().end_byte() >= fact.span().end_byte());
                    } else {
                        assert_eq!(fact.depth(), 0);
                    }
                }
                if capacity == required {
                    assert_eq!(mandatory_identity(&output).len(), output.facts().len());
                }
                assert_eq!(parse(&provider, &bounded).facts(), output.facts());
            }
            let short = limits(required - 1);
            let short_request = fixture.request(&short, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &short_request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1
                })
            );
            let budget = AnalysisLimits::new(
                base.max_source_bytes(),
                17,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let bounded = fixture.request(&budget, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &bounded,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::RejectedRequest(rootlight_adapter_sdk::RequestError::ProviderLimit {
                    resource: ResourceKind::SyntaxNodes,
                    observed: 18,
                    limit: 17
                })
            );
            let cancelled = deadline();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(parse(&provider, &request).facts(), full.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn implicit_calls_keep_unsupported_heads_and_complete_tails_on_the_whole_native_route() {
    for statement in [
        "Target(named:=value, other)",
        "Target (value, other,)",
        "Target value,",
        "Target ,value",
        "Target value,,other",
        "Target value:=1",
        "Target ByVal value",
        "holder.Target value",
        "Target value.member",
        "Target #1/1/2000#",
        "Target value$",
        "Target 1E+",
        "Target Inner(value)(other)",
        "Target value other",
        "Target value +",
        "Target (value))",
        "Target = value, other",
        "Call Target value",
        "Name value",
        "Line (value), other",
        "Print value",
        "Return",
        "Target _\n",
    ] {
        let source = format!(
            "Sub First()\nTarget value+1\nEnd Sub\nSub Run(ByVal value As Long)\n{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {statement}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| fact.syntax_kind().as_str()
                == "vba.identifier.definition"
                && bytes_at(source.as_bytes(), fact.span()) == b"First"));
        }
    }
}

#[test]
fn expression_calls_preserve_exact_application_callee_and_read_identity() {
    const STATEMENT: &str = "Call Target(value + 1, Inner(other) * -2)";
    let source = std::str::from_utf8(EXPRESSION_CALL_MODULE).unwrap();
    let start = source.find(STATEMENT).unwrap();
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(EXPRESSION_CALL_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        assert_eq!(request.source().source_ref(), &fixture.source);
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(EXPRESSION_CALL_MODULE)
        );
        assert_eq!(
            provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap(),
            11
        );
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            EXPRESSION_CALL_MODULE.len()
        );
        assert_eq!(output.report().resources().syntax_nodes(), 18);
        assert!(output.diagnostics().is_empty());
        let declaration = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
            .unwrap();
        let references = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str().ends_with(".reference")
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 6);
        let mut ids = std::collections::BTreeSet::new();
        for (syntax, written, offset) in [
            ("vba.application.reference", STATEMENT, 0),
            (
                "vba.invoked_identifier.reference",
                "Target",
                STATEMENT.find("Target").unwrap(),
            ),
            (
                "vba.value_identifier.reference",
                "value",
                STATEMENT.find("value").unwrap(),
            ),
            (
                "vba.application.reference",
                "Inner(other)",
                STATEMENT.find("Inner").unwrap(),
            ),
            (
                "vba.invoked_get_identifier.reference",
                "Inner",
                STATEMENT.find("Inner").unwrap(),
            ),
            (
                "vba.value_identifier.reference",
                "other",
                STATEMENT.find("other").unwrap(),
            ),
        ] {
            let matches = references
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == syntax
                        && fact.span().start_byte() == u64::try_from(start + offset).unwrap()
                        && fact.span().end_byte()
                            == u64::try_from(start + offset + written.len()).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "{language}: {syntax}: {written}");
            let fact = matches[0];
            assert_eq!(
                bytes_at(EXPRESSION_CALL_MODULE, fact.span()),
                written.as_bytes()
            );
            assert!(ids.insert(fact.local_id()));
            assert_eq!(fact.parent(), Some(declaration.local_id()));
            assert_eq!(fact.depth(), 5);
            assert_eq!(fact.span().file(), fixture.source.span().file());
        }
        assert!(output.facts().iter().all(|fact| {
            fact.span().file() == fixture.source.span().file()
                && fact.span().end_byte() <= fixture.source.span().end_byte()
                && fact
                    .parent()
                    .is_none_or(|id| output.facts().iter().any(|parent| parent.local_id() == id))
        }));
        let repeated = parse(&provider, &request);
        assert_eq!(repeated.facts(), output.facts());
        assert_eq!(repeated.report(), output.report());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn expression_call_lists_keep_written_groups_strings_and_logical_captures() {
    for (statement, reads, applications, strings) in [
        (
            "Call [Target](-([value] + 007), [Inner](other + other))",
            3,
            2,
            0,
        ),
        (
            "Call Target(\"a,\"\"b\"\":()\" & \"x\", Not (value = 1) And other <> 2)",
            2,
            1,
            2,
        ),
        (
            "Call Target(value < 1 < 2, value = 1 Xor other <> 2)",
            3,
            1,
            0,
        ),
        (
            "Call Target(value = 1 Imp other = 2, Inner(value <> other))",
            4,
            2,
            0,
        ),
        (
            "Call _\r\n Target(value + _\r\n 1, Inner(other) * -2)",
            2,
            2,
            0,
        ),
    ] {
        let source = format!(
            "Sub Run(ByVal value As Variant, ByVal other As Variant)\r\n{statement}\r\nEnd Sub\r\n"
        );
        let start = source.find(statement).unwrap();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            let output = parse(&provider, &request);
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete,
                "{language}: {statement}"
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert!(output.diagnostics().is_empty());
            assert_eq!(
                provider
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap(),
                11
            );
            let body = output
                .facts()
                .iter()
                .filter(|fact| {
                    fact.span().start_byte() >= u64::try_from(start).unwrap()
                        && fact.span().end_byte() <= u64::try_from(start + statement.len()).unwrap()
                })
                .collect::<Vec<_>>();
            let read_facts = body
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.value_identifier.reference")
                .collect::<Vec<_>>();
            assert_eq!(read_facts.len(), reads);
            let mut read_ids = std::collections::BTreeSet::new();
            for read in read_facts {
                assert!(
                    [b"value".as_slice(), b"[value]", b"other"]
                        .contains(&bytes_at(source.as_bytes(), read.span()))
                );
                assert!(read_ids.insert(read.local_id()));
            }
            let app_facts = body
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.application.reference")
                .collect::<Vec<_>>();
            assert_eq!(app_facts.len(), applications);
            assert_eq!(
                app_facts
                    .iter()
                    .filter(|fact| bytes_at(source.as_bytes(), fact.span()) == statement.as_bytes())
                    .count(),
                1
            );
            assert_eq!(body.iter().filter(|fact| fact.syntax_kind().as_str() == "vba.invoked_identifier.reference").count(), 1);
            assert_eq!(
                body.iter()
                    .filter(|fact| fact.syntax_kind().as_str()
                        == "vba.invoked_get_identifier.reference")
                    .count(),
                applications - 1
            );
            assert_eq!(
                body.iter()
                    .filter(|fact| fact.kind() == SyntaxFactKind::StringLiteral)
                    .count(),
                strings
            );
            for written in ["\"a,\"\"b\"\":()\"", "\"x\""] {
                if let Some(offset) = statement.find(written) {
                    assert_eq!(
                        body.iter()
                            .filter(|fact| {
                                fact.kind() == SyntaxFactKind::StringLiteral
                                    && fact.span().start_byte()
                                        == u64::try_from(start + offset).unwrap()
                                    && fact.span().end_byte()
                                        == u64::try_from(start + offset + written.len()).unwrap()
                                    && bytes_at(source.as_bytes(), fact.span())
                                        == written.as_bytes()
                            })
                            .count(),
                        1
                    );
                }
            }
            assert_eq!(parse(&provider, &request).facts(), output.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn expression_call_budgets_retain_mandatory_identity_and_cancel_without_retry() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(EXPRESSION_CALL_MODULE);
        let provider = provider();
        let base = limits(4096);
        let request = fixture.request(&base, language, Vec::new());
        let full = parse(&provider, &request);
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 11);
        for capacity in [
            required,
            required + 1,
            full.facts().len() - 1,
            full.facts().len(),
        ] {
            let budget = limits(capacity);
            let bounded = fixture.request(&budget, language, Vec::new());
            let output = parse(&provider, &bounded);
            assert_eq!(output.facts().len(), capacity);
            assert_eq!(
                output.report().coverage().status(),
                if capacity == full.facts().len() {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                }
            );
            for name in [b"Run".as_slice(), b"value", b"other"] {
                assert!(output.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str() == "vba.identifier.definition"
                        && bytes_at(EXPRESSION_CALL_MODULE, fact.span()) == name
                }));
            }
            if capacity == required {
                assert!(
                    output
                        .facts()
                        .iter()
                        .all(|fact| !fact.syntax_kind().as_str().ends_with(".reference"))
                );
            }
        }
        let short = limits(required - 1);
        let short_request = fixture.request(&short, language, Vec::new());
        assert_eq!(
            execute_parse(
                &provider,
                &short_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1
            })
        );
        for maximum in [18, 17] {
            let budget = AnalysisLimits::new(
                base.max_source_bytes(),
                maximum,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let request = fixture.request(&budget, language, Vec::new());
            let result = execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if maximum == 18 {
                let output = result.unwrap();
                assert_eq!(output.report().resources().syntax_nodes(), 18);
                assert_eq!(output.facts(), full.facts());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::RejectedRequest(
                        rootlight_adapter_sdk::RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 18,
                            limit: 17,
                        }
                    )
                );
            }
        }
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(parse(&provider, &request).facts(), full.facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn expression_calls_keep_unsupported_argument_and_root_tails_on_native_route() {
    for statement in [
        "Call Target(slot:=value + 1)",
        "Call Target(, value + 1)",
        "Call Target(ByVal value + 1)",
        "Call owner.Target(value + 1)",
        "Call Target(value + &H1&^)",
        "Call Target(value + 1E+)",
        "Call Target(value + #1/2/2000#)",
        "Call Target(value% + 1)",
        "Call Target(value + owner.Field)",
        "Call Target value + 1",
        "Call Target(1) + 2",
        "Call (Target(1))",
        "Call Target(1)(2)",
        "Call Target(value +)",
        "Call Target(value + 1,)",
        "Call Target(value + 1) trailing",
    ] {
        let source =
            format!("Sub First()\nEnd Sub\nSub Run(ByVal value As Long)\n{statement}\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let budget = limits(4096);
            let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {statement}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && bytes_at(source.as_bytes(), fact.span()) == b"First"
            }));
        }
    }
}

#[test]
fn expression_call_edit_invalidates_real_native_tree_without_cache_or_identity_fabrication() {
    const NATIVE_STATEMENT: &str = "Call Target(value + #1/1/2000#, Inner(other) * -2)";
    const OWN_STATEMENT: &str = "Call Target(value + 1, Inner(other) * -2)";
    let native = format!(
        "Sub Run(ByVal value As Long, ByVal other As Long)\r\n{NATIVE_STATEMENT}\r\nEnd Sub\r\n"
    );
    for language in ["vba", "visual-basic-classic"] {
        let mut fixture = Fixture::new(native.as_bytes());
        let provider = provider();
        let budget = limits(4096);
        let settings = ParserSettings::new(512).unwrap();
        let request = fixture.request(&budget, language, Vec::new());
        let previous = provider
            .execute_with_previous(
                &request,
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let continuation = previous
            .previous()
            .expect("the unsupported date argument retains a native tree");
        assert_eq!(
            previous.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        let old_hash = fixture.source.content_hash();
        let old_stats = provider.stats();
        assert!(old_stats.pooled_parsers > 0);
        assert!(old_stats.cache.entries > 0);
        fixture.rewrite(EXPRESSION_CALL_MODULE);
        let request = fixture.request(&budget, language, Vec::new());
        assert_ne!(old_hash, fixture.source.content_hash());
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(EXPRESSION_CALL_MODULE)
        );
        let start = native.find(NATIVE_STATEMENT).unwrap();
        let edit = SourceEdit::new(start, start + NATIVE_STATEMENT.len(), OWN_STATEMENT).unwrap();
        let output = provider
            .execute_with_previous(
                &request,
                Some(continuation),
                &[edit],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            output.reuse_status(),
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
        assert_eq!(
            output.reuse_key().grammar_version(),
            VBA_PROCEDURAL_FRONTEND_VERSION
        );
        assert!(output.previous().is_none());
        assert_eq!(
            output.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(output.output().facts(), parse(&provider, &request).facts());
        assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
        assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn expression_assignments_preserve_exact_flat_capture_identity() {
    let source = std::str::from_utf8(EXPRESSION_ASSIGNMENT_MODULE).unwrap();
    let statements = [
        "result = -(value + 007) * +2 ^ 3 ^ 4",
        "result = Not (value = 1) And True Or False Xor True Eqv False Imp True",
        "result = \"a,\"\"b\"\":()\" & \"x\"",
        "result = Outer(Inner(value + 1), items(-(value - 2))) + value",
    ];
    let applications = [
        ("Outer(Inner(value + 1), items(-(value - 2)))", "Outer"),
        ("Inner(value + 1)", "Inner"),
        ("items(-(value - 2))", "items"),
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(EXPRESSION_ASSIGNMENT_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        assert_eq!(
            provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap(),
            11
        );
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(output.diagnostics().is_empty());
        let scope = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.scope")
            .unwrap();
        let declaration = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
            .unwrap();
        assert_eq!(scope.depth(), 3);
        assert_eq!(declaration.parent(), Some(scope.local_id()));
        assert_eq!(declaration.depth(), 4);
        let references = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str().ends_with(".reference")
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 15);
        for statement in statements {
            let start = source.find(statement).unwrap();
            let destination = references
                .iter()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "vba.let_identifier.reference"
                        && fact.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap();
            assert_eq!(
                destination.span().end_byte(),
                u64::try_from(start + "result".len()).unwrap()
            );
        }
        let reads = references
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.value_identifier.reference")
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 5);
        for statement in [statements[0], statements[1], statements[3]] {
            let statement_start = source.find(statement).unwrap();
            for (offset, written) in statement.match_indices("value") {
                let start = statement_start + offset;
                assert!(reads.iter().any(|fact| {
                    fact.span().start_byte() == u64::try_from(start).unwrap()
                        && fact.span().end_byte() == u64::try_from(start + written.len()).unwrap()
                        && bytes_at(EXPRESSION_ASSIGNMENT_MODULE, fact.span()) == written.as_bytes()
                }));
            }
        }
        for (application, callee) in applications {
            let start = source.find(application).unwrap();
            for (syntax, written) in [
                ("vba.application.reference", application),
                ("vba.invoked_get_identifier.reference", callee),
            ] {
                let matches = references
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(start).unwrap()
                            && fact.span().end_byte()
                                == u64::try_from(start + written.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "{language}: {syntax}: {written}");
                assert_eq!(
                    bytes_at(EXPRESSION_ASSIGNMENT_MODULE, matches[0].span()),
                    written.as_bytes()
                );
            }
        }
        for fact in references {
            assert_eq!(fact.parent(), Some(declaration.local_id()));
            assert_eq!(fact.depth(), 5);
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
        }
        let strings = output
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::StringLiteral)
            .collect::<Vec<_>>();
        assert_eq!(strings.len(), 2);
        for written in ["\"a,\"\"b\"\":()\"", "\"x\""] {
            let start = source.find(written).unwrap();
            let literal = strings
                .iter()
                .find(|fact| fact.span().start_byte() == u64::try_from(start).unwrap())
                .unwrap();
            assert_eq!(
                literal.span().end_byte(),
                u64::try_from(start + written.len()).unwrap()
            );
            assert_eq!(
                bytes_at(EXPRESSION_ASSIGNMENT_MODULE, literal.span()),
                written.as_bytes()
            );
            assert_eq!(literal.parent(), Some(declaration.local_id()));
            assert_eq!(literal.depth(), 5);
        }
        assert_eq!(parse(&provider, &request).facts(), output.facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn comparison_chains_and_logical_mixes_preserve_exact_first_party_captures() {
    for (expression, applications) in [
        ("value < 1 < 2", Vec::new()),
        ("value = 1 And value <> 2", Vec::new()),
        ("value = 1 Or value <> 2", Vec::new()),
        ("value = 1 Xor value <> 2", Vec::new()),
        ("value = 1 Eqv value <> 2", Vec::new()),
        ("value = 1 Imp value <> 2", Vec::new()),
        (
            "Outer(Inner(value < 1 < 2), value = 1 Xor value <> 2)",
            vec![
                (
                    "Outer(Inner(value < 1 < 2), value = 1 Xor value <> 2)",
                    "Outer",
                ),
                ("Inner(value < 1 < 2)", "Inner"),
            ],
        ),
    ] {
        let statement = format!("result = {expression}");
        let source = format!(
            "Sub Run(ByVal value As Long)\r\nDim result As Variant\r\n{statement}\r\nEnd Sub\r\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            assert_eq!(
                provider
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap(),
                11
            );
            let output = parse(&provider, &request);
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete,
                "{language}: {expression}"
            );
            assert_eq!(
                output.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert!(output.diagnostics().is_empty());
            let declaration = output
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
                .unwrap();
            let references = output
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Occurrence
                        && fact.syntax_kind().as_str().ends_with(".reference")
                })
                .collect::<Vec<_>>();
            let statement_start = source.find(&statement).unwrap();
            let expression_start = statement_start + "result = ".len();
            let mut expected = vec![("vba.let_identifier.reference", statement_start, "result")];
            for (offset, written) in expression.match_indices("value") {
                expected.push((
                    "vba.value_identifier.reference",
                    expression_start + offset,
                    written,
                ));
            }
            for &(application, callee) in &applications {
                let start = expression_start + expression.find(application).unwrap();
                expected.push(("vba.application.reference", start, application));
                expected.push(("vba.invoked_get_identifier.reference", start, callee));
            }
            assert_eq!(references.len(), expected.len());
            for (syntax, start, written) in expected {
                let matches = references
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(start).unwrap()
                            && fact.span().end_byte()
                                == u64::try_from(start + written.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "{language}: {expression}: {written}");
                let fact = matches[0];
                assert_eq!(bytes_at(source.as_bytes(), fact.span()), written.as_bytes());
                assert_eq!(fact.parent(), Some(declaration.local_id()));
                assert_eq!(fact.depth(), 5);
                assert_eq!(fact.span().file(), fixture.source.span().file());
                assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
            }
            assert_eq!(parse(&provider, &request).facts(), output.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn expression_capture_budgets_preserve_declarations_and_cancellation() {
    for expression in [
        "value + 1",
        "-([value] + 1) * +2 ^ 3 ^ 4",
        "-([value] + _\r\n 1) * +2",
        "Not (value = 1) And (value <> 2)",
        "Outer(Inner(value + 1), (value * 2)) + value",
        "value < 1 < 2",
        "value = 1 And value <> 2",
        "value = 1 Or value <> 2",
        "value = 1 Xor value <> 2",
        "value = 1 Eqv value <> 2",
        "value = 1 Imp value <> 2",
        "Outer(Inner(value < 1 < 2), value = 1 Xor value <> 2)",
        "\"written\" & \"a,\"\"b\"\":()\"",
    ] {
        let source = format!(
            "Sub Run(ByVal value As Long)\nDim result As Variant\nresult = {expression}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 11);
            let full = parse(&provider, &request);
            assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
            assert!(full.facts().len() > required + 1);
            for capacity in [required, required + 1] {
                let clipped_limits = limits(capacity);
                let clipped_request = fixture.request(&clipped_limits, language, Vec::new());
                let clipped = parse(&provider, &clipped_request);
                assert_eq!(clipped.facts().len(), capacity);
                assert_eq!(
                    clipped.report().coverage().status(),
                    CoverageStatus::Bounded
                );
                assert!(
                    clipped.diagnostics().iter().any(|diagnostic| {
                        diagnostic.code().as_str() == "syntax-extraction-limit"
                    })
                );
                for name in ["Run", "value", "result"] {
                    assert!(clipped.facts().iter().any(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && bytes_at(source.as_bytes(), fact.span()) == name.as_bytes()
                    }));
                }
                assert_eq!(
                    provider
                        .required_syntax_fact_count(&clipped_request, &deadline())
                        .unwrap(),
                    required
                );
            }
            let short_limits = limits(required - 1);
            let short_request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &short_request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            let cancelled = deadline();
            assert!(cancelled.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(parse(&provider, &request).facts(), full.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn expression_operator_families_keep_exact_reads_and_work_limits() {
    for operator in [
        "+", "-", "*", "/", "\\", "^", "Mod", "&", "=", "<>", "<", ">", "<=", ">=", "Is", "Like",
        "And", "Or", "Xor", "Eqv", "Imp",
    ] {
        let source = format!(
            "Sub Run(ByVal left As Variant, ByVal right As Variant)\nDim result As Variant\nLet [result] = [left] {operator} right\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let output = parse(
                &provider,
                &fixture.request(&full_limits, language, Vec::new()),
            );
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete,
                "{language}: {operator}"
            );
            let start = source.find("Let [result]").unwrap();
            let declaration = output
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "vba.sub_function.declaration")
                .unwrap();
            let references = output
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Occurrence
                        && fact.span().start_byte() >= u64::try_from(start).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(references.len(), 3);
            for (syntax, written) in [
                ("vba.let_identifier.reference", "[result]"),
                ("vba.value_identifier.reference", "[left]"),
                ("vba.value_identifier.reference", "right"),
            ] {
                let offset = start + source[start..].find(written).unwrap();
                let fact = references
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(offset).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    fact.span().end_byte(),
                    u64::try_from(offset + written.len()).unwrap()
                );
                assert_eq!(bytes_at(source.as_bytes(), fact.span()), written.as_bytes());
                assert_eq!(fact.parent(), Some(declaration.local_id()));
                assert_eq!(fact.depth(), 5);
            }
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
    let source = b"Sub Run()\nDim result As Variant\nresult = (1 + 2) * (3 + 4)\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let base = limits(4096);
        for maximum in [15, 14] {
            let bounded = AnalysisLimits::new(
                base.max_source_bytes(),
                maximum,
                base.max_syntax_depth(),
                base.max_embedded_ranges(),
                base.max_reported_memory_bytes(),
                base.syntax_stream().clone(),
                base.ir_stream().clone(),
                base.ir().clone(),
            )
            .unwrap();
            let request = fixture.request(&bounded, language, Vec::new());
            let result = execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if maximum == 15 {
                let output = result.unwrap();
                assert_eq!(
                    output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert_eq!(output.report().resources().syntax_nodes(), 15);
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::RejectedRequest(
                        rootlight_adapter_sdk::RequestError::ProviderLimit {
                            resource: ResourceKind::SyntaxNodes,
                            observed: 15,
                            limit: 14,
                        }
                    )
                );
            }
        }
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn expression_processor_keeps_unsupported_tails_on_the_native_route() {
    for expression in [
        "value + &H1&^",
        "value + #1/2/2000#",
        "value + 1.5!#",
        "value + 1E+",
        "value + value%",
        "value + owner.Field",
        "value + Me",
        "Target(slot:=1) + 2",
        "Target(,1) + 2",
        "Target(ByVal value) + 2",
        "Target(1)(2) + 3",
        "Line(1) + 2",
        "(value + 1",
        "value +",
        "Not",
        "value + 1 trailing",
    ] {
        let source = format!(
            "Sub First()\nEnd Sub\nSub Run(ByVal value As Long)\nDim result As Variant\nresult = (value + 1) * 2\nresult = {expression}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let output = parse(
                &provider,
                &fixture.request(&full_limits, language, Vec::new()),
            );
            assert!(
                provider.stats().pooled_parsers > 0,
                "{language}: {expression}"
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
            for name in ["First", "Run"] {
                assert!(output.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str() == "vba.identifier.definition"
                        && bytes_at(source.as_bytes(), fact.span()) == name.as_bytes()
                }));
            }
        }
    }
}

#[test]
fn intrinsic_parameters_preserve_exact_identity_and_mandatory_retention() {
    let source = std::str::from_utf8(PARAMETER_MODULE).unwrap();
    let parameters = [
        ("ByVal [value] As Long", "[value]"),
        ("ByRef text As String", "text"),
        ("item As Long", "item"),
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(PARAMETER_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 18);
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert!(full.diagnostics().is_empty());
        let exact_limits = limits(required);
        let request = fixture.request(&exact_limits, language, Vec::new());
        let exact = parse(&provider, &request);
        for output in [&full, &exact] {
            let declarations: Vec<_> = output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.parameter.declaration")
                .collect();
            assert_eq!(declarations.len(), parameters.len());
            for ((clause, name), declaration) in parameters.iter().copied().zip(declarations) {
                let start = source.find(clause).unwrap();
                let name_start = start + clause.find(name).unwrap();
                assert_eq!(
                    bytes_at(PARAMETER_MODULE, declaration.span()),
                    clause.as_bytes()
                );
                assert_eq!(
                    declaration.span().start_byte(),
                    u64::try_from(start).unwrap()
                );
                assert_eq!(
                    declaration.span().end_byte(),
                    u64::try_from(start + clause.len()).unwrap()
                );
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && fact.parent() == Some(declaration.local_id())
                            && fact.span().start_byte() == u64::try_from(name_start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    bytes_at(PARAMETER_MODULE, definition.span()),
                    name.as_bytes()
                );
                assert_eq!(
                    definition.span().end_byte(),
                    u64::try_from(name_start + name.len()).unwrap()
                );
                assert_eq!(definition.span().file(), fixture.source.span().file());
                let parent = declaration.parent().unwrap();
                assert!(output.facts().iter().any(|fact| {
                    fact.local_id() == parent
                        && matches!(
                            fact.syntax_kind().as_str(),
                            "vba.sub_function.declaration" | "vba.function.declaration"
                        )
                        && fact.span().start_byte() <= declaration.span().start_byte()
                        && fact.span().end_byte() >= declaration.span().end_byte()
                }));
            }
        }
        assert_eq!(
            full.facts(),
            parse(
                &provider,
                &fixture.request(&full_limits, language, Vec::new())
            )
            .facts()
        );
        let stats = provider.stats();
        assert_eq!(stats.pooled_parsers, 0);
        assert_eq!(stats.checked_out_parsers, 0);
        assert_eq!(stats.cache.entries, 0);
    }
}

#[test]
fn header_parameter_trivia_never_becomes_a_fabricated_formal() {
    let source = b"Sub Empty(Rem)\nEnd Sub\nSub Accept(Rem[value], Optional other = 7)\nEnd Sub\n";
    let text = std::str::from_utf8(source).unwrap();
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        assert_eq!(request.source().source_ref(), &fixture.source);
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(source)
        );
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 15);
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert_eq!(full.report().resources().syntax_nodes(), 10);
        assert!(full.diagnostics().is_empty());
        let comments: Vec<_> = full
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.comment.comment")
            .collect();
        assert_eq!(comments.len(), 2);
        for (comment, start) in comments
            .iter()
            .zip([text.find("Rem)").unwrap(), text.find("Rem[value]").unwrap()])
        {
            assert_eq!(comment.span().start_byte(), u64::try_from(start).unwrap());
            assert_eq!(comment.span().end_byte(), u64::try_from(start + 3).unwrap());
            assert_eq!(bytes_at(source, comment.span()), b"Rem");
            assert!(!full.facts().iter().any(|fact| {
                fact.syntax_kind().as_str() == "vba.identifier.definition"
                    && fact.span() == comment.span()
            }));
        }
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        assert_eq!(exact.facts().len(), required);
        for output in [&full, &exact] {
            let parameters: Vec<_> = output
                .facts()
                .iter()
                .filter(|fact| {
                    matches!(
                        fact.syntax_kind().as_str(),
                        "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
                    )
                })
                .collect();
            assert_eq!(parameters.len(), 2);
            for (parameter, clause, name) in [
                (parameters[0], "[value]", "[value]"),
                (parameters[1], "Optional other = 7", "other"),
            ] {
                let start = text.find(clause).unwrap();
                assert_eq!(parameter.span().start_byte(), u64::try_from(start).unwrap());
                assert_eq!(
                    parameter.span().end_byte(),
                    u64::try_from(start + clause.len()).unwrap()
                );
                assert_eq!(bytes_at(source, parameter.span()), clause.as_bytes());
                assert!(output.facts().iter().any(|fact| {
                    fact.parent() == Some(parameter.local_id())
                        && fact.syntax_kind().as_str() == "vba.identifier.definition"
                        && bytes_at(source, fact.span()) == name.as_bytes()
                }));
            }
            let empty = output
                .facts()
                .iter()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "vba.sub_function.declaration"
                        && bytes_at(source, fact.span()).starts_with(b"Sub Empty")
                })
                .unwrap();
            assert!(
                parameters
                    .iter()
                    .all(|parameter| parameter.parent() != Some(empty.local_id()))
            );
        }
        let short_limits = limits(required - 1);
        let request = fixture.request(&short_limits, language, Vec::new());
        assert_eq!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: required,
                limit: required - 1
            })
        );
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn omitted_types_preserve_exact_written_clauses_and_bound_source_identity() {
    let source = std::str::from_utf8(OMITTED_TYPE_MODULE).unwrap();
    let clauses = [
        ("required", "required", "vba.parameter.declaration"),
        ("ByVal typed As Long", "typed", "vba.parameter.declaration"),
        (
            "Optional ByVal count = 007",
            "count",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional [text] = \"a,\"\"b\"\":()\"",
            "[text]",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional flag = False",
            "flag",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional unset",
            "unset",
            "vba.omittable_parameter.declaration",
        ),
        ("ByRef value", "value", "vba.parameter.declaration"),
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(OMITTED_TYPE_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        assert_eq!(request.source().source_ref(), &fixture.source);
        assert_eq!(request.source().bytes(), OMITTED_TYPE_MODULE);
        assert_eq!(
            fixture.source.content_hash(),
            rootlight_ids::content_hash(OMITTED_TYPE_MODULE)
        );
        assert_eq!(
            fixture.source.content_hash(),
            fixture.snapshot.content_hash()
        );
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 26);
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert_eq!(
            full.report().coverage().covered_source_bytes(),
            OMITTED_TYPE_MODULE.len()
        );
        assert_eq!(full.report().resources().syntax_nodes(), 21);
        assert!(full.diagnostics().is_empty());
        assert_eq!(
            full.facts()
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "vba.parameter.declaration"
                        || fact.syntax_kind().as_str() == "vba.omittable_parameter.declaration"
                })
                .count(),
            clauses.len()
        );
        for fact in full.facts() {
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
            assert!(!fact.syntax_kind().as_str().contains("variant"));
            if fact.syntax_kind().as_str().ends_with(".reference") {
                assert!(
                    [
                        b"count".as_slice(),
                        b"required",
                        b"[text]",
                        b"Echo",
                        b"value"
                    ]
                    .contains(&bytes_at(OMITTED_TYPE_MODULE, fact.span()))
                );
            }
        }
        let string_start = u64::try_from(source.find("\"a,").unwrap()).unwrap();
        let strings: Vec<_> = full
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "vba.string.string"
                    && fact.span().start_byte() == string_start
            })
            .collect();
        assert_eq!(strings.len(), 1);
        assert_eq!(
            bytes_at(OMITTED_TYPE_MODULE, strings[0].span()),
            b"\"a,\"\"b\"\":()\""
        );
        for literal in ["007", "False"] {
            let start = u64::try_from(source.find(literal).unwrap()).unwrap();
            assert!(!full.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".reference")
                    && fact.span().start_byte() == start
            }));
        }
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        assert_eq!(exact.facts().len(), required);
        assert_eq!(exact.report().coverage().status(), CoverageStatus::Bounded);
        assert_eq!(exact.report().resources().syntax_nodes(), 21);
        for output in [&full, &exact] {
            for (clause, name, syntax) in clauses {
                let start = source.find(clause).unwrap();
                let declaration = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    declaration.span().end_byte(),
                    u64::try_from(start + clause.len()).unwrap()
                );
                assert_eq!(
                    bytes_at(OMITTED_TYPE_MODULE, declaration.span()),
                    clause.as_bytes()
                );
                let name_start = start + clause.find(name).unwrap();
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && fact.parent() == Some(declaration.local_id())
                            && fact.span().start_byte() == u64::try_from(name_start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    bytes_at(OMITTED_TYPE_MODULE, definition.span()),
                    name.as_bytes()
                );
                assert_eq!(
                    definition.span().end_byte(),
                    u64::try_from(name_start + name.len()).unwrap()
                );
                let owner = output
                    .facts()
                    .iter()
                    .find(|fact| Some(fact.local_id()) == declaration.parent())
                    .unwrap();
                assert!(matches!(
                    owner.syntax_kind().as_str(),
                    "vba.sub_function.declaration" | "vba.function.declaration"
                ));
                assert!(
                    owner.span().start_byte() <= declaration.span().start_byte()
                        && owner.span().end_byte() >= declaration.span().end_byte()
                );
            }
        }
        assert_eq!(full.facts(), parse(&provider, &request).facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn omitted_type_identity_retains_every_formal_at_exact_and_short_budgets() {
    for (parameters, parameter_count, optional_count, defaults, required) in [
        ("value", 1, 0, 0, 9),
        ("ByVal first, ByRef second", 2, 0, 0, 11),
        ("Optional value", 1, 1, 0, 9),
        ("Optional value = 7", 1, 1, 1, 9),
        ("required, Optional value = \"x\"", 2, 1, 1, 11),
        (
            "typed As Long, untyped, Optional count = 7, Optional flag = True",
            4,
            2,
            2,
            15,
        ),
    ] {
        let source = format!("Sub Run({parameters})\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            assert_eq!(
                provider
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap(),
                required
            );
            let full = parse(&provider, &request);
            assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
            assert_eq!(
                full.report().resources().syntax_nodes(),
                4 + parameter_count + defaults
            );
            assert!(full.diagnostics().is_empty());
            assert!(
                full.facts()
                    .iter()
                    .all(|fact| !fact.syntax_kind().as_str().ends_with(".reference"))
            );
            let exact_limits = limits(required);
            let exact = parse(
                &provider,
                &fixture.request(&exact_limits, language, Vec::new()),
            );
            assert_eq!(exact.facts().len(), required);
            for output in [&full, &exact] {
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| fact.syntax_kind().as_str()
                            == "vba.omittable_parameter.declaration")
                        .count(),
                    optional_count
                );
                let declarations: Vec<_> = output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.syntax_kind().as_str(),
                            "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
                        )
                    })
                    .collect();
                assert_eq!(declarations.len(), parameter_count);
                for declaration in declarations {
                    assert_eq!(
                        output
                            .facts()
                            .iter()
                            .filter(|fact| {
                                fact.parent() == Some(declaration.local_id())
                                    && fact.syntax_kind().as_str() == "vba.identifier.definition"
                            })
                            .count(),
                        1
                    );
                }
            }
            let short_limits = limits(required - 1);
            let request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1
                })
            );
            let cancelled = deadline();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn optional_parameters_preserve_written_modifiers_defaults_and_exact_identity() {
    let source = std::str::from_utf8(OPTIONAL_PARAMETER_MODULE).unwrap();
    let clauses = [
        ("required As Long", "required", "vba.parameter.declaration"),
        (
            "Optional ByVal count As Long = 007",
            "count",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional [text] As String = \"a,\"\"b\"\":()\"",
            "[text]",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional flag As Boolean = False",
            "flag",
            "vba.omittable_parameter.declaration",
        ),
        (
            "Optional unset As Long",
            "unset",
            "vba.omittable_parameter.declaration",
        ),
    ];
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(OPTIONAL_PARAMETER_MODULE);
        let provider = provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert_eq!(required, 18);
        let full = parse(&provider, &request);
        assert_eq!(full.report().coverage().status(), CoverageStatus::Complete);
        assert!(full.diagnostics().is_empty());
        let string_start = u64::try_from(source.find("\"a,").unwrap()).unwrap();
        let strings: Vec<_> = full
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "vba.string.string"
                    && fact.span().start_byte() == string_start
            })
            .collect();
        assert_eq!(strings.len(), 1);
        assert_eq!(
            bytes_at(OPTIONAL_PARAMETER_MODULE, strings[0].span()),
            b"\"a,\"\"b\"\":()\""
        );
        for literal in ["007", "False"] {
            let start = u64::try_from(source.find(literal).unwrap()).unwrap();
            assert!(
                !full.facts().iter().any(|fact| fact
                    .syntax_kind()
                    .as_str()
                    .ends_with(".reference")
                    && fact.span().start_byte() == start)
            );
        }
        let exact_limits = limits(required);
        let exact = parse(
            &provider,
            &fixture.request(&exact_limits, language, Vec::new()),
        );
        assert_eq!(exact.facts().len(), required);
        assert_eq!(exact.report().coverage().status(), CoverageStatus::Bounded);
        for output in [&full, &exact] {
            for (clause, name, syntax) in clauses {
                let start = source.find(clause).unwrap();
                let end = start + clause.len();
                let declaration = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == syntax
                            && fact.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .unwrap();
                assert_eq!(declaration.span().end_byte(), u64::try_from(end).unwrap());
                assert_eq!(
                    bytes_at(OPTIONAL_PARAMETER_MODULE, declaration.span()),
                    clause.as_bytes()
                );
                let name_start = start + clause.find(name).unwrap();
                let definition = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && fact.parent() == Some(declaration.local_id())
                            && fact.span().start_byte() == u64::try_from(name_start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    bytes_at(OPTIONAL_PARAMETER_MODULE, definition.span()),
                    name.as_bytes()
                );
                assert_eq!(
                    definition.span().end_byte(),
                    u64::try_from(name_start + name.len()).unwrap()
                );
                assert_eq!(definition.span().file(), fixture.source.span().file());
                let owner = output
                    .facts()
                    .iter()
                    .find(|fact| Some(fact.local_id()) == declaration.parent())
                    .unwrap();
                assert_eq!(owner.syntax_kind().as_str(), "vba.sub_function.declaration");
                assert!(
                    owner.span().start_byte() <= declaration.span().start_byte()
                        && owner.span().end_byte() >= declaration.span().end_byte()
                );
            }
        }
        assert_eq!(full.facts(), parse(&provider, &request).facts());
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn optional_parameter_identity_retains_all_formals_at_exact_and_short_budgets() {
    for (parameters, required, optional_count) in [
        ("Optional value As Long", 9, 1),
        ("Optional value As String = \"x\"", 9, 1),
        ("required As Long, Optional value As Long = 7", 11, 1),
        (
            "Optional count As Long = 7, Optional text As String = \"x\", Optional flag As Boolean = True",
            13,
            3,
        ),
    ] {
        let source = format!("Sub Run({parameters})\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            assert_eq!(
                provider
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap(),
                required
            );
            let full = parse(&provider, &request);
            let exact_limits = limits(required);
            let exact = parse(
                &provider,
                &fixture.request(&exact_limits, language, Vec::new()),
            );
            assert_eq!(exact.facts().len(), required);
            for output in [&full, &exact] {
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| fact.syntax_kind().as_str()
                            == "vba.omittable_parameter.declaration")
                        .count(),
                    optional_count
                );
                for declaration in output.facts().iter().filter(|fact| {
                    matches!(
                        fact.syntax_kind().as_str(),
                        "vba.parameter.declaration" | "vba.omittable_parameter.declaration"
                    )
                }) {
                    assert!(
                        output
                            .facts()
                            .iter()
                            .any(|fact| fact.parent() == Some(declaration.local_id())
                                && fact.syntax_kind().as_str() == "vba.identifier.definition")
                    );
                }
            }
            let short_limits = limits(required - 1);
            let request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1
                })
            );
            let cancelled = deadline();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn required_parameter_identity_fits_exact_and_rejects_one_short() {
    for (parameters, expected) in [
        ("ByVal value As Long", 9),
        ("ByVal first As Long, ByRef second As String", 11),
    ] {
        let source = format!("Sub Run({parameters})\nEnd Sub\n");
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let full_limits = limits(4096);
            let request = fixture.request(&full_limits, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, expected);
            let exact_limits = limits(required);
            let request = fixture.request(&exact_limits, language, Vec::new());
            let exact = parse(&provider, &request);
            assert_eq!(exact.facts().len(), required);
            assert_eq!(
                exact
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == "vba.parameter.declaration")
                    .count(),
                (required - 7) / 2
            );
            let short_limits = limits(required - 1);
            let request = fixture.request(&short_limits, language, Vec::new());
            assert_eq!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            let cancelled = deadline();
            assert!(cancelled.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn unsupported_tail_is_not_hidden_by_a_valid_prefix_or_included_range() {
    let source =
        b"Sub Empty()\nEnd Sub\nSub Populated(ByVal value As Long)\nvalue = #1/1/2000#\nEnd Sub\n";
    for scoped in [false, true] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let limits = limits(4096);
        let ranges = if scoped {
            vec![IncludedRange::new(
                SourceSpan::new(fixture.source.span().file(), 0, 19).unwrap(),
                LanguageId::new("vba").unwrap(),
            )]
        } else {
            Vec::new()
        };
        let request = fixture.request(&limits, "vba", ranges);
        let _required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert!(provider.stats().pooled_parsers > 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().ends_with(".definition")
                && bytes_at(source, fact.span()) == b"Empty"
        }));
        if !scoped {
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && bytes_at(source, fact.span()) == b"Populated"
            }));
        }
    }
}

#[test]
fn native_object_context_cannot_be_promoted_to_a_procedural_file() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let limits = limits(4096);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("vba").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(rootlight_ids::content_hash(b"authored-context")),
        &limits,
    )
    .unwrap()
    .with_native_vba_module(NativeVbaModuleKind::Object)
    .unwrap()
    .to_parse_request();
    let output = parse(&provider, &request);
    assert!(provider.stats().pooled_parsers > 0);
    assert!(
        output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "vba.class.module")
    );
    assert!(
        !output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "vba.file.module")
    );
}

#[test]
fn native_continuation_is_invalidated_when_an_edit_enters_the_first_party_route() {
    let native = b"Sub Run()\nDim value As Long\nvalue = #1/1/2000#\nEnd Sub\n";
    let first_party = b"Sub Run()\nEnd Sub\n";
    let mut fixture = Fixture::new(native);
    let provider = provider();
    let limits = limits(4096);
    let settings = ParserSettings::new(512).unwrap();
    let request = fixture.request(&limits, "vba", Vec::new());
    let previous = provider
        .execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let continuation = previous
        .previous()
        .expect("native parse owns a real cached tree");
    assert_eq!(
        previous.output().report().coverage().status(),
        CoverageStatus::Complete
    );
    let old_stats = provider.stats();
    fixture.rewrite(first_party);
    let request = fixture.request(&limits, "vba", Vec::new());
    let edit = SourceEdit::new(10, native.len() - b"End Sub\n".len(), "").unwrap();
    let output = provider
        .execute_with_previous(
            &request,
            Some(continuation),
            &[edit],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        output.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
    );
    assert_eq!(
        output.reuse_key().grammar_version(),
        VBA_PROCEDURAL_FRONTEND_VERSION
    );
    assert!(output.previous().is_none());
    assert_eq!(output.output().facts(), parse(&provider, &request).facts());
    assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
    assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn native_continuation_is_invalidated_by_a_richer_first_party_expression() {
    let native = b"Sub Run()\nDim value As Long\nvalue = #1/1/2000#\nEnd Sub\n";
    let first_party = b"Sub Run()\nDim value As Long\nvalue = (value + 1) * -2\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        let mut fixture = Fixture::new(native);
        let provider = provider();
        let full_limits = limits(4096);
        let settings = ParserSettings::new(512).unwrap();
        let request = fixture.request(&full_limits, language, Vec::new());
        let previous = provider
            .execute_with_previous(
                &request,
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let continuation = previous
            .previous()
            .expect("native parse retains its real tree");
        assert_eq!(
            previous.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        let old_stats = provider.stats();
        assert!(old_stats.pooled_parsers > 0);
        assert!(old_stats.cache.entries > 0);
        fixture.rewrite(first_party);
        let request = fixture.request(&full_limits, language, Vec::new());
        let edit = SourceEdit::new(
            b"Sub Run()\n".len(),
            native.len() - b"End Sub\n".len(),
            "Dim value As Long\nvalue = (value + 1) * -2\n",
        )
        .unwrap();
        let output = provider
            .execute_with_previous(
                &request,
                Some(continuation),
                &[edit],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            output.reuse_status(),
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
        assert_eq!(
            output.reuse_key().grammar_version(),
            VBA_PROCEDURAL_FRONTEND_VERSION
        );
        assert!(output.previous().is_none());
        assert_eq!(
            output.output().report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(output.output().facts(), parse(&provider, &request).facts());
        assert!(output.output().facts().iter().any(|fact| {
            fact.syntax_kind().as_str() == "vba.value_identifier.reference"
                && bytes_at(first_party, fact.span()) == b"value"
        }));
        assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
        assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn first_party_identity_preflight_matches_exact_fit_and_one_short_fact_admission() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let full_limits = limits(4096);
    let request = fixture.request(&full_limits, "vba", Vec::new());
    let required = provider
        .required_syntax_fact_count(&request, &deadline())
        .unwrap();
    assert!(required > 1);
    let exact_limits = limits(required);
    let request = fixture.request(&exact_limits, "vba", Vec::new());
    let output = parse(&provider, &request);
    for name in [b"Run".as_slice(), b"Count"] {
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().ends_with(".definition")
                && bytes_at(CLOSED_MODULE, fact.span()) == name
        }));
    }
    let short_limits = limits(required - 1);
    let request = fixture.request(&short_limits, "vba", Vec::new());
    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
    assert_eq!(provider.stats().pooled_parsers, 0);
}

#[test]
fn cancellation_does_not_trigger_native_fallback_or_leave_output_state() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let limits = limits(4096);
    let request = fixture.request(&limits, "vba", Vec::new());
    let cancelled = deadline();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(provider.stats().pooled_parsers, 0);
    assert_eq!(provider.stats().cache.entries, 0);
    assert!(!parse(&provider, &request).facts().is_empty());
}

fn procedure_exit_blank(source: &str, exits: &[&str]) -> String {
    exits.iter().fold(source.to_owned(), |source, written| {
        source.replace(
            written,
            &written
                .chars()
                .map(|character| {
                    if matches!(character, '\r' | '\n') {
                        character
                    } else {
                        ' '
                    }
                })
                .collect::<String>(),
        )
    })
}

#[test]
fn procedure_exits_preserve_later_sdk_facts_and_exact_keyword_ranges() {
    for (sub_exit, function_exit) in [
        ("Exit Sub", "Exit Function"),
        ("eXiT\tSuB", "eXiT\tFuNcTiOn"),
        ("Exit _\n Sub", "Exit _\n Function"),
    ] {
        let source = format!(
            "Attribute VB_Name = \"Authored\"\nSub Notify(Optional count As Long = 7)\n{sub_exit} ' retained exit trivia\nEnd Sub\nSub Run()\n{sub_exit}:Dim local As Long:local = local ' retained sibling\nCall Notify()\nEnd Sub\nFunction Echo(Optional value As Long = 7) As Long\n{function_exit}\nEcho = value\nEnd Function\n"
        );
        for newline in ["\n", "\r\n"] {
            let source = source.replace('\n', newline);
            let sub_exit = sub_exit.replace('\n', newline);
            let function_exit = function_exit.replace('\n', newline);
            let baseline = procedure_exit_blank(&source, &[&sub_exit, &function_exit]);
            assert_eq!(source.len(), baseline.len());
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let baseline_fixture = Fixture::new(baseline.as_bytes());
                let provider = provider();
                let budget = limits(4096);
                let request = fixture.request(&budget, language, Vec::new());
                let output = parse(&provider, &request);
                let baseline_output = parse(
                    &provider,
                    &baseline_fixture.request(&budget, language, Vec::new()),
                );
                assert_eq!(
                    output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert_eq!(
                    output.report().coverage().covered_source_bytes(),
                    source.len()
                );
                assert!(output.diagnostics().is_empty());
                let required = provider
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap();
                assert_eq!(required, 22);
                assert_eq!(
                    required,
                    provider
                        .required_syntax_fact_count(
                            &baseline_fixture.request(&budget, language, Vec::new()),
                            &deadline(),
                        )
                        .unwrap(),
                );
                let geometry = |output: &ParseOutput| {
                    output
                        .facts()
                        .iter()
                        .map(|fact| {
                            (
                                fact.local_id(),
                                fact.parent(),
                                fact.kind(),
                                fact.span().start_byte(),
                                fact.span().end_byte(),
                                fact.depth(),
                                fact.syntax_kind().as_str().to_owned(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                // Source hashes differ, but equal-width trivia cannot erase later facts.
                assert_eq!(geometry(&output), geometry(&baseline_output));
                assert_eq!(
                    fixture.source.content_hash(),
                    rootlight_ids::content_hash(source.as_bytes())
                );
                let inventory = rootlight_vba::module_inventory(
                    source.as_bytes(),
                    rootlight_vba::ModuleLimits::new(source.len(), 4096, 4096),
                    &mut || Ok::<(), ()>(()),
                )
                .unwrap();
                let run = &inventory.procedures()[1];
                let run_declaration = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "vba.sub_function.declaration"
                            && fact.span().start_byte()
                                == u64::try_from(run.declaration().start).unwrap()
                            && fact.span().end_byte()
                                == u64::try_from(run.declaration().end).unwrap()
                    })
                    .unwrap();
                let local_declaration = output
                    .facts()
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "vba.variable.declaration"
                            && bytes_at(source.as_bytes(), fact.span()) == b"local As Long"
                    })
                    .unwrap();
                assert_eq!(local_declaration.parent(), Some(run_declaration.local_id()));
                for fact in output.facts() {
                    assert_eq!(fact.span().file(), fixture.source.span().file());
                    assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
                }
                let mut exit_count = 0;
                for (procedure_index, procedure) in inventory.procedures().iter().enumerate() {
                    for statement in procedure.statements() {
                        let rootlight_vba::ModuleBodyStatement::ProcedureExit(exit) = statement
                        else {
                            continue;
                        };
                        exit_count += 1;
                        let written = if procedure_index == 2 {
                            &function_exit
                        } else {
                            &sub_exit
                        };
                        assert_eq!(&source[exit.statement()], written);
                        assert!(source[exit.exit_keyword()].eq_ignore_ascii_case("Exit"));
                        assert!(source[exit.kind_keyword()].eq_ignore_ascii_case(
                            if procedure_index == 2 {
                                "Function"
                            } else {
                                "Sub"
                            },
                        ));
                        assert_eq!(
                            exit.kind(),
                            if procedure_index == 2 {
                                rootlight_vba::ModuleProcedureExitKind::Function
                            } else {
                                rootlight_vba::ModuleProcedureExitKind::Sub
                            }
                        );
                        assert!(exit.statement().start >= procedure.body().start);
                        assert!(exit.statement().end <= procedure.body().end);
                        assert!(!output.facts().iter().any(|fact| {
                            fact.span().start_byte()
                                >= u64::try_from(exit.statement().start).unwrap()
                                && fact.span().end_byte()
                                    <= u64::try_from(exit.statement().end).unwrap()
                        }));
                    }
                }
                assert_eq!(exit_count, 3);
                for (syntax, written) in [
                    ("vba.variable.declaration", "local As Long"),
                    ("vba.application.reference", "Call Notify()"),
                    ("vba.value_identifier.reference", "value"),
                ] {
                    assert!(
                        output.facts().iter().any(|fact| {
                            fact.syntax_kind().as_str() == syntax
                                && bytes_at(source.as_bytes(), fact.span()) == written.as_bytes()
                        }),
                        "{language}: {syntax} {written}"
                    );
                }
                assert_eq!(output.facts(), parse(&provider, &request).facts());
                assert_eq!(provider.stats().pooled_parsers, 0);
                assert_eq!(provider.stats().checked_out_parsers, 0);
                assert_eq!(provider.stats().cache.entries, 0);
            }
        }
    }
}

#[test]
fn procedure_exits_charge_statement_work_without_new_required_facts() {
    let source = format!(
        "Sub Run()\n{}Dim value As Long\nvalue = 1\nEnd Sub\n",
        "Exit Sub\n".repeat(8)
    );
    for newline in ["\n", "\r\n"] {
        let source = source.replace('\n', newline);
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let provider = provider();
            let base = limits(4096);
            let request = fixture.request(&base, language, Vec::new());
            let required = provider
                .required_syntax_fact_count(&request, &deadline())
                .unwrap();
            assert_eq!(required, 9);
            let full = parse(&provider, &request);
            assert_eq!(full.report().resources().syntax_nodes(), 14);
            assert_eq!(full.facts().len(), 12);
            let baseline = procedure_exit_blank(&source, &["Exit Sub"]);
            let baseline_fixture = Fixture::new(baseline.as_bytes());
            let baseline_output = parse(
                &provider,
                &baseline_fixture.request(&base, language, Vec::new()),
            );
            assert_eq!(full.facts(), baseline_output.facts());
            for capacity in [required, full.facts().len()] {
                let bounded = limits(capacity);
                let output = parse(&provider, &fixture.request(&bounded, language, Vec::new()));
                assert_eq!(output.facts().len(), capacity);
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "vba.identifier.definition"
                        })
                        .count(),
                    2
                );
            }
            let short = limits(required - 1);
            assert_eq!(
                execute_parse(
                    &provider,
                    &fixture.request(&short, language, Vec::new()),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                })
            );
            for maximum in [14, 13] {
                let bounded = AnalysisLimits::new(
                    base.max_source_bytes(),
                    maximum,
                    base.max_syntax_depth(),
                    base.max_embedded_ranges(),
                    base.max_reported_memory_bytes(),
                    base.syntax_stream().clone(),
                    base.ir_stream().clone(),
                    base.ir().clone(),
                )
                .unwrap();
                let result = execute_parse(
                    &provider,
                    &fixture.request(&bounded, language, Vec::new()),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                );
                if maximum == 14 {
                    assert_eq!(result.unwrap().facts(), full.facts());
                } else {
                    assert_eq!(
                        result.unwrap_err(),
                        AdapterError::RejectedRequest(
                            rootlight_adapter_sdk::RequestError::ProviderLimit {
                                resource: ResourceKind::SyntaxNodes,
                                observed: 14,
                                limit: 13,
                            },
                        )
                    );
                }
            }
            assert!(matches!(
                AnalysisLimits::new(
                    base.max_source_bytes(),
                    0,
                    base.max_syntax_depth(),
                    base.max_embedded_ranges(),
                    base.max_reported_memory_bytes(),
                    base.syntax_stream().clone(),
                    base.ir_stream().clone(),
                    base.ir().clone(),
                ),
                Err(rootlight_adapter_sdk::LimitError::Zero {
                    field: "analysis.max_syntax_nodes"
                })
            ));
            let cancelled = deadline();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled,
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(parse(&provider, &request).facts(), full.facts());
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn implicit_bare_callees_preserve_later_mandatory_declarations() {
    for newline in ["\n", "\r\n"] {
        let source = "Sub First()\nEnd Sub\nSub Run()\nExitSub\nDim later As Long\nEnd Sub\n"
            .replace('\n', newline);
        let declaration_start = u64::try_from(source.find("later").unwrap()).unwrap();
        for language in ["vba", "visual-basic-classic"] {
            let fixture = Fixture::new(source.as_bytes());
            let first_party = provider();
            let native = provider();
            let budget = limits(4096);
            let request = fixture.request(&budget, language, Vec::new());
            let output = parse(&first_party, &request);
            let reference = parse(
                &native,
                &fixture.request(
                    &budget,
                    language,
                    vec![IncludedRange::new(
                        fixture.source.span(),
                        LanguageId::new(language).unwrap(),
                    )],
                ),
            );
            assert_eq!(first_party.stats().pooled_parsers, 0);
            assert_eq!(first_party.stats().checked_out_parsers, 0);
            assert_eq!(first_party.stats().cache.entries, 0);
            assert!(native.stats().pooled_parsers > 0);
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert_eq!(output.facts(), reference.facts());
            assert!(output.diagnostics().is_empty());
            assert_eq!(output.diagnostics(), reference.diagnostics());
            assert_eq!(
                first_party
                    .required_syntax_fact_count(&request, &deadline())
                    .unwrap(),
                13
            );
            assert_eq!(
                output
                    .facts()
                    .iter()
                    .filter(|fact| {
                        fact.syntax_kind().as_str() == "vba.identifier.definition"
                            && fact.span().start_byte() == declaration_start
                            && fact.span().end_byte() == declaration_start + 5
                            && bytes_at(source.as_bytes(), fact.span()) == b"later"
                    })
                    .count(),
                1
            );
        }
    }
}

#[test]
fn unsupported_procedure_exit_forms_preserve_whole_module_native_fallback() {
    for (header, terminator, body) in [
        ("Sub Run()", "End Sub", "Exit Function"),
        ("Function Run() As Long", "End Function", "Exit Sub"),
        ("Sub Run()", "End Sub", "Exit"),
        ("Sub Run()", "End Sub", "Exit Sub trailing"),
        ("Sub Run()", "End Sub", "Exit Sub()"),
        ("Sub Run()", "End Sub", "ExitSub ,"),
        ("Sub Run()", "End Sub", "Exit Property"),
        ("Sub Run()", "End Sub", "Exit Do"),
        ("Sub Run()", "End Sub", "Exit For"),
        ("Sub Run()", "End Sub", "Do\nExit Do\nLoop"),
    ] {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "Sub First()\nEnd Sub\n{header}\n{body}\nDim later As Long\n{terminator}\n"
            )
            .replace('\n', newline);
            assert!(matches!(
                rootlight_vba::module_inventory(
                    source.as_bytes(),
                    rootlight_vba::ModuleLimits::new(source.len(), 4096, 4096),
                    &mut || Ok::<(), ()>(()),
                ),
                Err(rootlight_vba::ModuleError::Unsupported { .. })
            ));
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let provider = provider();
                let budget = limits(4096);
                // Invalid fixtures test fallback routing, not native completeness.
                let output = parse(&provider, &fixture.request(&budget, language, Vec::new()));
                assert!(provider.stats().pooled_parsers > 0, "{language}: {body}");
                assert_eq!(provider.stats().checked_out_parsers, 0);
                assert!(output.facts().iter().any(|fact| {
                    fact.syntax_kind().as_str() == "vba.identifier.definition"
                        && bytes_at(source.as_bytes(), fact.span()) == b"First"
                }));
            }
        }
    }
}

fn bytes_at(source: &[u8], span: SourceSpan) -> &[u8] {
    source
        .get(usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap())
        .unwrap()
}

fn parse(provider: &TreeSitterProvider, request: &ParseRequest<'_>) -> ParseOutput {
    execute_parse(
        provider,
        request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("bounded parse commits")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn provider() -> TreeSitterProvider {
    TreeSitterProvider::new(
        RuntimeConfig::new(
            SOURCE_LIMIT,
            4096,
            128,
            32,
            64,
            1,
            4 * 1024 * 1024,
            ParserSettings::new(512).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn limits(records: usize) -> AnalysisLimits {
    let batch = BatchThresholds::new(records.min(128), 128 * 1024, 4, 4096).unwrap();
    let syntax = StreamLimits::new(
        64,
        records,
        4 * 1024 * 1024,
        16,
        64 * 1024,
        128 * 1024,
        batch,
    )
    .unwrap();
    let ir = StreamLimits::new(
        8192,
        4096,
        16 * 1024 * 1024,
        128,
        1024 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 16, 64 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        SOURCE_LIMIT,
        4096,
        128,
        32,
        8 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .unwrap()
}

struct Fixture {
    _temporary: TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let (snapshot, source) = capture(&temporary, bytes);
        Self {
            _temporary: temporary,
            snapshot,
            source,
        }
    }

    fn rewrite(&mut self, bytes: &[u8]) {
        let (snapshot, source) = capture(&self._temporary, bytes);
        self.snapshot = snapshot;
        self.source = source;
    }

    fn request<'a>(
        &'a self,
        limits: &'a AnalysisLimits,
        language: &str,
        ranges: Vec<IncludedRange>,
    ) -> ParseRequest<'a> {
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::new("utf-8").unwrap(),
            ranges,
            limits,
        )
        .unwrap()
    }
}

fn capture(temporary: &TempDir, bytes: &[u8]) -> (SourceSnapshot, SourceRef) {
    fs::write(temporary.path().join("module.bas"), bytes).unwrap();
    let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
    let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
    let snapshot = repository
        .snapshot(
            &RelativePath::parse(Path::new("module.bas")).unwrap(),
            u64::try_from(SOURCE_LIMIT).unwrap(),
        )
        .unwrap();
    let source = SourceRef::new(
        repository_id,
        "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .unwrap(),
        SourceSpan::new(
            snapshot.file(),
            0,
            u64::try_from(snapshot.content().len()).unwrap(),
        )
        .unwrap(),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
