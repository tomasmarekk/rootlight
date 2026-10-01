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
    ParseOutput, ParseRequest, ResourceKind, SinkError, StreamLimits, execute_parse,
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
const OPTIONAL_PARAMETER_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Accept(required As Long, Optional ByVal count As Long = 007, Optional [text] As String = \"a,\"\"b\"\":()\", Optional flag As Boolean = False, Optional unset As Long)\r\ncount = required\r\n[text] = [text]\r\nEnd Sub\r\n";
const OMITTED_TYPE_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nSub Accept(required, ByVal typed As Long, Optional ByVal count = 007, Optional [text] = \"a,\"\"b\"\":()\", Optional flag = False, Optional unset)\r\ncount = required\r\n[text] = [text]\r\nEnd Sub\r\nFunction Echo(ByRef value) As Long\r\nEcho = value\r\nEnd Function\r\n";

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
        b"Sub Empty()\nEnd Sub\nSub Populated(ByVal value As Long)\nvalue = value + 1\nEnd Sub\n";
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
    let native = b"Sub Run()\nDim value As Long\nvalue = 1 + 2\nEnd Sub\n";
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
