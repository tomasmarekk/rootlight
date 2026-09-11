//! Public retrieval-contract evidence across real MCP and daemon processes.
//!
//! The fixtures exercise advertised retrieval capabilities and verify stable
//! rejection for schema-visible options outside the accepted surface.

mod process_support;

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use rootlight_ids::SymbolId;
use rootlight_mcp_contract::accounting::estimate_tokens;
use serde_json::{Map, Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const RETRIEVAL_SOURCE: &str = "\
pub fn matrix_target_alpha(value: usize) -> usize {
    matrix_target_beta(value).saturating_add(1)
}

pub fn matrix_target_beta(value: usize) -> usize {
    matrix_target_gamma(value).saturating_mul(2)
}

pub fn matrix_target_gamma(value: usize) -> usize {
    value.saturating_sub(1)
}

pub fn matrix_target_delta(value: usize) -> usize {
    value.saturating_add(4)
}

pub fn matrix_target_epsilon(value: usize) -> usize {
    value.saturating_add(5)
}

pub fn matrix_target_zeta(value: usize) -> usize {
    value.saturating_add(6)
}

pub fn matrix_target_eta(value: usize) -> usize {
    value.saturating_add(7)
}

pub fn matrix_target_theta(value: usize) -> usize {
    value.saturating_add(8)
}

pub fn matrix_target_iota(value: usize) -> usize {
    value.saturating_add(9)
}

pub fn matrix_target_kappa(value: usize) -> usize {
    value.saturating_add(10)
}

pub fn matrix_target_lambda(value: usize) -> usize {
    value.saturating_add(11)
}

pub fn matrix_target_mu(value: usize) -> usize {
    value.saturating_add(12)
}
";
const SCOPED_PAGE_SOURCE: &str = "\
pub fn scope_page_candidate() -> usize {
    1
}
";

#[test]
fn yaml_scalar_alias_keys_cross_real_process_boundaries() {
    data_properties_cross_process_boundaries(
        "yaml",
        "data.yaml",
        "key: 101\nitems: [null, {&entry key: 303}, {\"k\\u0065y\": 202}, {*entry : 404}]\n'': 5\n' ': 6\n",
        &[("key", 4), (r#"str:"""#, 1), (r#"str:" ""#, 1)],
    );
}

#[test]
fn stylesheet_entities_cross_real_process_boundaries() {
    let source = ".highlight { color: red; }\n@keyframes pulse { from { opacity: 0; } to { opacity: 1; } }\n";
    source_entities_cross_process_boundaries(
        "css",
        "style.css",
        source,
        &[(".highlight", "style_rule", 1), ("pulse", "keyframes", 1)],
    );
}

#[test]
fn powershell_declarations_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "powershell",
        "catalog.psm1",
        "function Read-Entry { param([string]$Name); return $Name }\nclass Cache { [string]$Label; [string] Read([int]$slot) { return $this.Label } }\n",
        &[
            ("Read-Entry", "function", 1),
            ("Cache", "type", 1),
            ("Read", "method", 1),
        ],
    );
}

#[test]
fn powershell_commented_signatures_cross_real_process_boundaries() {
    let source = format!(
        "function Read-Entry {{ <# {} #> [CmdletBinding()] param([string]$Name = 'literal # text'); $Name }}",
        "documentation ".repeat(400)
    );
    source_entities_with_signatures_cross_process_boundaries(
        "powershell",
        "catalog.psm1",
        &source,
        &[("Read-Entry", "function", 1)],
        &[(
            "Read-Entry",
            "function Read-Entry { [CmdletBinding()] param([string]$Name = 'literal # text')",
        )],
    );
}

#[test]
fn powershell_empty_blocks_preserve_exact_retrieval_across_processes() {
    source_entities_cross_process_boundaries(
        "powershell",
        "actions.ps1",
        "$action = {}\nInvoke-Check Write-Entry {<# no action #>}\nfunction Read-Entry { return 1 }\n",
        &[("$action", "variable", 1), ("Read-Entry", "function", 1)],
    );
}

#[test]
fn powershell_numeric_values_preserve_exact_retrieval_across_processes() {
    source_entities_cross_process_boundaries(
        "powershell",
        "values.ps1",
        "$size = 2kB\n$rate = 2E+2MB\n$mask = 0X2LPb\nfunction Read-Size { return $size }\n",
        &[
            ("$size", "variable", 1),
            ("$rate", "variable", 1),
            ("$mask", "variable", 1),
            ("Read-Size", "function", 1),
        ],
    );
}

#[test]
fn powershell_method_blocks_preserve_exact_retrieval_across_processes() {
    source_entities_cross_process_boundaries(
        "powershell",
        "callbacks.ps1",
        "$selected = $items.Where{ $_ }\n$mapped = $items.ForEach{}\n$read = [Item]::Read{ param($entry); $entry }\n$custom = $items.Apply{ function Read-Entry { return 1 } }\n",
        &[
            ("$selected", "variable", 1),
            ("$mapped", "variable", 1),
            ("$read", "variable", 1),
            ("$custom", "variable", 1),
            ("Read-Entry", "function", 1),
        ],
    );
}

#[test]
fn powershell_composite_arguments_preserve_exact_retrieval_across_processes() {
    source_entities_cross_process_boundaries(
        "powershell",
        "arguments.ps1",
        "function Read-Entry { param($name); return $name }\nRead-Entry name=\"$($name)\" $env:root\\Cache\\Data pre$(Read-Value)post\nWrite-Output \"$($items | Select-Entry Name, Description | Out-String)\"\n",
        &[("Read-Entry", "function", 1)],
    );
}

#[test]
fn powershell_string_hashes_preserve_exact_retrieval_across_processes() {
    source_entities_cross_process_boundaries(
        "powershell",
        "strings.ps1",
        "$text = \"`r`n## Heading`r`n\"\nInvoke-Entry name=\"$value# function Hidden {}\"\n$text = @\"\n$other# function Hidden {}\n\"@\nfunction Visible { return $text }\n",
        &[("Visible", "function", 1)],
    );
}

#[test]
fn powershell_nested_literals_cross_real_process_boundaries() {
    let nested = (0..8).fold("{ param($leaf) $leaf }".to_owned(), |value, _| {
        format!("@{{ Entry = {value} }}")
    });
    let source = format!("Invoke-Entry {{ $value = {nested} }}\n");
    source_entities_cross_process_boundaries(
        "powershell",
        "nested.ps1",
        &source,
        &[("<anonymous>", "function", 2), ("Entry", "field", 8)],
    );
}

#[test]
fn powershell_anonymous_blocks_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "powershell",
        "blocks.ps1",
        "function Outer { param($root); Invoke-Entry { param($value) { param($inner) $inner + $value } } }\n",
        &[("<anonymous>", "function", 2), ("Outer", "function", 1)],
    );
}

#[test]
fn powershell_assignment_targets_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "powershell",
        "bindings.ps1",
        "[int]$number = 1\n$first, [string]$second = 2, 'text'\n",
        &[
            ("$number", "variable", 1),
            ("$first", "variable", 1),
            ("$second", "variable", 1),
        ],
    );
}

#[test]
fn powershell_data_properties_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "powershell",
        "settings.psd1",
        "@{ Title = 'text'; 'quoted key' = 2; Nested = @{ Title = 'inner' } }\n",
        &[
            ("Title", "field", 2),
            ("quoted key", "field", 1),
            ("Nested", "field", 1),
        ],
    );
}

#[test]
fn markup_entities_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "html",
        "view.html",
        "<main><item key='one' key='two'>first</item><item key='three'>second</item><script>const embedded = '<fake />';</script></main>\n",
        &[
            ("item", "markup_element", 2),
            ("key", "markup_attribute", 3),
        ],
    );
}

#[test]
fn embedded_javascript_crosses_process_boundaries_with_host_sources() {
    source_entities_with_signatures_cross_process_boundaries(
        "javascript",
        "view.html",
        "<main>é\r\n<script>function welcome(name) { return name; } welcome('a');</script><script type='module'>function welcome(name) { return name + '!'; }</script></main>",
        &[("welcome", "function", 2)],
        &[("welcome", "(name)")],
    );
}

#[test]
fn embedded_css_crosses_process_boundaries_with_host_sources() {
    source_entities_cross_process_boundaries(
        "css",
        "view.html",
        "<main>é\r\n<style>.card { color: red; }</style><style>.card { color: blue; }</style></main>",
        &[(".card", "style_rule", 2)],
    );
}

#[test]
fn markup_analysis_gaps_preserve_scoped_exact_source_access() {
    let source = "<main><script>const embedded = '<fake />';</script><noscript><b>conditional</b></noscript><svg><title>foreign</title></svg></main>\n";
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some(("view.html", source)), FixtureLayout::Data);
    for (ordinal, (query, mode, language, path, partial)) in [
        ("embedded", "lexical", "html", "view.html", true),
        ("view.html", "path", "html", "view.html", true),
        ("matrix_target_alpha", "exact", "rust", "src/lib.rs", false),
    ]
    .into_iter()
    .enumerate()
    {
        let located = fixture.standalone(
            &format!("markup-gap-{ordinal}"),
            "code.locate",
            json!({"query": query, "search_modes": [mode], "languages": [language],
                "scope": {"paths": [path]}, "response_profile": "evidence"}),
        );
        assert_success(&located, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        assert_eq!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "coverage_unsupported"),
            partial,
            "{output:#}"
        );
        let matches = output["data"]["matches"].as_array().unwrap();
        assert!(!matches.is_empty(), "{output:#}");
        if !partial {
            continue;
        }
        for (hit_ordinal, hit) in matches.iter().enumerate() {
            assert_eq!(hit["path"], path);
            let reference = &hit["source_ref"];
            let read = fixture.standalone(
                &format!("markup-gap-read-{ordinal}-{hit_ordinal}"),
                "source.read",
                json!({"references": [{"source_ref": reference}], "context_lines_before": 0,
                    "context_lines_after": 0, "response_profile": "evidence"}),
            );
            assert_success(&read, "source.read");
            let output = &read["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            let chunk = &output["data"]["chunks"][0];
            for field in ["repository", "generation", "content_hash", "span"] {
                assert_eq!(chunk["source_ref"][field], reference[field]);
            }
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
            assert_eq!(chunk["content"].as_str(), source.get(start..end));
        }
    }
    let absent = fixture.standalone(
        "markup-literal-is-not-an-element",
        "code.locate",
        json!({"query": "fake", "search_modes": ["exact"], "languages": ["html"],
            "scope": {"paths": ["view.html"]}, "response_profile": "evidence"}),
    );
    assert_success(&absent, "code.locate");
    let matches = absent["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .unwrap();
    // The literal remains an exact file-text hit, never an authored element.
    assert_eq!(matches.len(), 1, "{absent:#}");
    assert_eq!(matches[0]["kind"], "file");
    assert!(matches[0]["symbol_id"].is_null());
    assert_eq!(matches[0]["path"], "view.html");
    fixture.finish();
}

#[test]
fn markdown_document_entities_cross_real_process_boundaries() {
    for path in ["guide.md", "guide.markdown"] {
        source_entities_cross_process_boundaries(
            "markdown",
            path,
            "# Overview\r\n\r\nRead [the reference][manual].\r\n\r\n[manual]: https://example.test/guide\r\n\r\n## Details\r\n\r\nSource-backed documentation.\r\n",
            &[
                ("Overview", "document_section", 1),
                ("Details", "document_section", 1),
                ("[manual]", "link_definition", 1),
            ],
        );
    }
}

#[test]
fn sql_database_objects_cross_process_boundaries_with_exact_source_entities() {
    source_entities_cross_process_boundaries(
        "sql",
        "schema.sql",
        "CREATE TABLE app.account (id INT);\nCREATE TABLE app.account (id TEXT);\nCREATE VIEW app.active AS SELECT id FROM app.account;\n",
        &[
            ("app.account", "database_object", 2),
            ("app.active", "database_object", 1),
        ],
    );
}

#[test]
fn solidity_declaration_kinds_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "solidity",
        "vault.sol",
        "contract Vault { event Changed(uint value); event Changed(address value); error Rejected(uint code); modifier allowed(uint minimum) { require(minimum > 0); _; } }",
        &[
            ("Changed", "event", 2),
            ("Rejected", "error_declaration", 1),
            ("allowed", "modifier", 1),
        ],
    );
}

#[test]
fn dart_constructors_and_accessors_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "dart",
        "store.dart",
        "class Store { final int value; Store(this.value); Store /* outer /* nested */ owner */ . named(this.value); int read(int input) => input; int get size => value; set size(int next) {} int operator /(int divisor) => value; }\nenum Phase { open, closed }",
        &[
            ("Store", "type", 1),
            ("Store.named", "method", 1),
            ("read", "method", 1),
            ("store.dart::Store::read", "method", 1),
            ("size", "method", 2),
            ("/", "method", 1),
            ("open", "constant", 1),
        ],
    );
}

#[test]
fn scala_companions_overloads_and_written_names_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "scala",
        "store.scala",
        "package outer\npackage inner\npackage object utility { def format(value: Int): String = value.toString }\nclass Entry(val count: Int)\nobject Entry { def read(value: Int) = value; def read(value: String) = value; def `odd name` = 1; def / = 2 }\nenum Color { case Red, Blue }",
        &[
            ("Entry", "type", 1),
            ("store.scala::outer::inner::Entry", "type", 1),
            ("utility", "module", 1),
            ("store.scala::outer::inner::utility::format", "function", 1),
            ("outer", "module", 1),
            ("inner", "module", 1),
            ("Entry", "module", 1),
            ("read", "function", 2),
            ("odd name", "function", 1),
            ("/", "function", 1),
            ("Red", "constant", 1),
            ("Blue", "constant", 1),
        ],
    );
}

#[test]
fn r_source_owners_cross_process_boundaries_without_claiming_runtime_bindings() {
    source_entities_cross_process_boundaries(
        "r",
        "analysis.R",
        "identity <- function(value) { value }\nidentity <- function(value) { value + 1 }\nlapply(values, function(value) value)\n",
        // The public retrieval taxonomy groups IR parameters under `variable`.
        // The durable service test separately asserts their exact Parameter kind.
        &[
            ("identity", "function", 2),
            ("<anonymous>", "function", 1),
            ("value", "variable", 3),
        ],
    );
}

#[test]
fn r_decoded_names_reach_exact_mcp_queries_with_written_source_spelling() {
    source_entities_cross_process_boundaries(
        "r",
        "names.R",
        "`with spaces` <- function(value) { value }\n\"with\\x20spaces\" <- function(value) { value + 1 }\n\"\\u03bb\" <- function(value) { value }\n",
        &[("with spaces", "function", 2), ("λ", "function", 1)],
    );
}

#[test]
fn sql_return_headers_reach_mcp_explanations_without_body_text() {
    let header = "CREATE FUNCTION app.rows(\nvalue INT\n)\nRETURNS TABLE (id INT) LANGUAGE SQL";
    let source = format!("{header} AS $$ SELECT value; $$;\n");
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some(("schema.sql", &source)), FixtureLayout::Data);
    let located = fixture.standalone("sql-header-locate", "code.locate", json!({
        "query": "app.rows", "search_modes": ["exact"], "languages": ["sql"], "response_profile": "evidence"
    }));
    assert_success(&located, "code.locate");
    let matches = located["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .unwrap();
    let found = matches
        .iter()
        .find(|item| item["kind"] == "function")
        .unwrap();
    let symbol = found["symbol_id"].clone();
    let arguments = json!({"symbol_ids": [symbol], "response_profile": "evidence"});
    let explained = fixture.standalone("sql-header-explain", "symbol.explain", arguments.clone());
    let batched = fixture.batch("sql-header-batch", "symbol.explain", arguments, "evidence");
    assert_standalone_batch_parity(&explained, &batched, "symbol.explain");
    let output = &explained["result"]["structuredContent"];
    assert_common_read_contract(output, &fixture.repository_id);
    assert_eq!(output["data"]["symbols"][0]["signature"], header);
    let limited = fixture.standalone(
        "sql-header-source-budget",
        "symbol.explain",
        json!({
            "symbol_ids": [found["symbol_id"]], "response_profile": "evidence",
            "budget": {"max_source_bytes": 1}
        }),
    );
    assert_success(&limited, "symbol.explain");
    let limited = &limited["result"]["structuredContent"];
    assert_eq!(limited["completeness"]["state"], "truncated");
    assert!(
        limited["completeness"]["limiting_resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["kind"] == "source_bytes")
    );
    assert!(limited["data"]["symbols"][0]["signature"].is_null());
    let read = fixture.standalone(
        "sql-header-source",
        "source.read",
        json!({
            "references": [{"source_ref": found["source_ref"]}], "response_profile": "evidence"
        }),
    );
    assert_success(&read, "source.read");
    let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
    let reference = &found["source_ref"];
    for field in ["repository", "generation", "content_hash", "span"] {
        assert_eq!(chunk["source_ref"][field], reference[field]);
    }
    let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
    let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
    assert_eq!(chunk["content"].as_str(), source.get(start..end));
    fixture.finish();
}

#[test]
fn objective_c_selectors_and_properties_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "objective-c",
        "selectors.m",
        include_str!("../../../tests/fixtures/objective-c/selectors.m"),
        &[
            ("::", "method", 1),
            ("perform::", "method", 1),
            ("value", "method", 2),
        ],
    );
    source_entities_cross_process_boundaries(
        "objective-c",
        "declarations.m",
        include_str!("../../../tests/fixtures/objective-c/declarations.m"),
        &[("value", "property", 1), ("add:to:", "method", 2)],
    );
}

#[test]
fn matlab_lexical_relationships_cross_mcp_with_exact_read_sources() {
    assert_lexical_relationship_sources(
        "matlab",
        "inspect.m",
        "function result = inspect(input)\nlocal = input;\nlocal = 2;\nresult = local;\ndisp(result);\nend\n",
        &[
            ("input", "variable", "input"),
            ("local", "variable", "local"),
            ("result", "variable", "result"),
        ],
    );
}

#[test]
fn matlab_definitions_and_headers_cross_real_process_boundaries() {
    source_entities_with_signatures_cross_process_boundaries(
        "matlab",
        "measure.m",
        "function result = measure(value)\nresult = value + 1;\nend\n",
        &[("measure", "function", 1), ("value", "parameter", 1)],
        &[("measure", "function result = measure(value)")],
    );
    source_entities_with_signatures_cross_process_boundaries(
        "matlab",
        "Meter.m",
        include_str!("../../../tests/native-grammars/fixtures/matlab/Meter.m"),
        &[("Value", "property", 1), ("get.Value", "method", 1)],
        &[("get.Value", "function result = get.Value(obj)")],
    );
}

#[test]
fn perl_definitions_and_headers_cross_real_process_boundaries() {
    source_entities_with_coverage_cross_process_boundaries(
        "perl",
        "measure.pm",
        "package Measure; sub adjust ($value) { my $result = $value + 1; return $result; }\n",
        &[
            ("adjust", "function", 1),
            ("$value", "parameter", 1),
            ("$result", "variable", 1),
        ],
        &[("adjust", "sub adjust ($value)")],
        Some(false),
    );
    source_entities_with_signatures_cross_process_boundaries(
        "perl",
        "counter.pl",
        "class Counter { field $value; method read ($offset = 1) { $value + $offset } }\n",
        &[
            ("read", "method", 1),
            ("$value", "field", 1),
            ("$offset", "parameter", 1),
        ],
        &[("read", "method read ($offset = 1)")],
    );
}

#[test]
fn perl_project_package_calls_preserve_distinct_targets_and_exact_mcp_sources() {
    for with_write in [false, true] {
        let client = if with_write {
            include_str!("../../../tests/fixtures/perl-bindings/cross-file-local-write/client.pl")
        } else {
            include_str!("../../../tests/fixtures/perl-bindings/cross-file/client.pl")
        };
        let module =
            include_str!("../../../tests/fixtures/perl-bindings/cross-file/module/Measure.pm");
        let mut sources = vec![("client.pl", client), ("module/Measure.pm", module)];
        if with_write {
            sources.push((
                "module/Rebind.pm",
                include_str!(
                    "../../../tests/fixtures/perl-bindings/cross-file-local-write/module/Rebind.pm"
                ),
            ));
        }
        let mut fixture = RetrievalFixture::spawn_with_sources(&sources, FixtureLayout::Data);
        let mut symbols = Vec::new();
        for (path, written) in [
            ("module/Measure.pm", "Measure::adjust"),
            ("client.pl", "adjust"),
        ] {
            let located = fixture.standalone(
                "package-target",
                "code.locate",
                json!({"query": "adjust", "search_modes": ["exact"], "languages": ["perl"],
                "scope": {"paths": [path]}, "response_profile": "evidence"}),
            );
            assert_success(&located, "code.locate");
            let matches: Vec<_> = located["result"]["structuredContent"]["data"]["matches"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["kind"] == "function")
                .collect();
            assert_eq!(matches.len(), 1, "{located:#}");
            let symbol = matches[0]["symbol_id"].clone();
            let generation = matches[0]["source_ref"]["generation"].clone();
            symbols.push(symbol.clone());
            let arguments = json!({"symbol_ids": [symbol], "relations": ["calls"],
            "direction": "inbound", "include_candidates": false, "response_profile": "evidence"});
            let response =
                fixture.standalone("package-callers", "symbol.relationships", arguments.clone());
            assert_success(&response, "symbol.relationships");
            let batch = fixture.batch(
                "package-callers-batch",
                "symbol.relationships",
                arguments,
                "evidence",
            );
            assert_standalone_batch_parity(&response, &batch, "symbol.relationships");
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            let expected = usize::from(!with_write || path != "client.pl");
            assert_eq!(
                output["data"]["totals"]["total_edges"], expected,
                "{output:#}"
            );
            assert_eq!(output["data"]["totals"]["returned_edges"], expected);
            // A static package-storage link does not prove runtime module loading
            // or the absence of every dynamic caller.
            assert_eq!(output["data"]["totals"]["exact"], false);
            assert!(
                output["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| { warning["code"] == "negative_claims_inconclusive" })
            );
            let groups = output["data"]["groups"].as_array().unwrap();
            if expected == 0 {
                assert!(
                    groups
                        .iter()
                        .all(|group| group["items"].as_array().unwrap().is_empty())
                );
                continue;
            }
            assert_eq!(groups.len(), 1);
            let items = groups[0]["items"].as_array().unwrap();
            assert_eq!(items.len(), 1);
            let references = items[0]["source_refs"].as_array().unwrap();
            assert_eq!(references.len(), 1);
            let reference = &references[0];
            assert_eq!(reference["generation"], generation);
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
            assert_eq!(client.get(start..end), Some(written));
            let read = fixture.standalone(
                "package-call-source",
                "source.read",
                json!({"references": [{"source_ref": reference}], "context_lines_before": 0,
                "context_lines_after": 0, "response_profile": "evidence"}),
            );
            assert_success(&read, "source.read");
            let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
            assert_eq!(chunk["content"], written);
            for key in ["repository", "generation", "content_hash", "span"] {
                assert_eq!(chunk["source_ref"][key], reference[key]);
            }
        }
        assert_ne!(symbols[0], symbols[1]);
        fixture.finish();
    }
}

#[test]
fn perl_extensionless_definitions_cross_real_process_boundaries() {
    for source in [
        include_str!("../../../tests/fixtures/perl-bindings/shebang-probe"),
        include_str!("../../../tests/fixtures/perl-bindings/shebang-arguments"),
    ] {
        source_entities_with_coverage_cross_process_boundaries(
            "perl",
            "runner",
            source,
            &[("answer", "function", 1)],
            &[("answer", "sub answer")],
            Some(true),
        );
    }
}

#[test]
fn perl_lexical_relationships_cross_mcp_with_exact_read_sources() {
    assert_lexical_relationship_sources(
        "perl",
        "measure.pm",
        "package Measure; sub adjust ($value, $step = $value + 1) { my $result = $step; return $result; }\n",
        &[
            ("$value", "variable", "$value"),
            ("$step", "variable", "$step"),
            ("$result", "variable", "$result"),
        ],
    );
    assert_lexical_relationship_sources(
        "perl",
        "loop.pl",
        "package Measure; for my $item (1, 2) { my $reader = sub { return $item; }; print $reader; }\n",
        &[
            ("$item", "variable", "$item"),
            ("$reader", "variable", "$reader"),
        ],
    );
}

#[test]
fn perl_package_relationships_cross_mcp_with_exact_read_sources() {
    for written in ["$value", "$Harbor::value"] {
        assert_lexical_relationship_sources(
            "perl",
            "measure.pm",
            &format!("package Harbor; our $value = 7; package Cove; print {written};\n"),
            &[("$value", "variable", written)],
        );
    }
}

#[test]
fn matlab_local_calls_cross_mcp_with_exact_call_sources() {
    assert_relationship_sources(
        "matlab",
        "entry.m",
        "function result = entry(value)\nresult = helper(value);\ndisp(result);\nend\nfunction result = helper(value)\nresult = value;\nend\n",
        &[("helper", "function", "helper(value)")],
        "calls",
    );
}

#[test]
fn perl_function_calls_cross_mcp_with_exact_sources() {
    for source in [
        "package Harbor; sub target { 1 } { our sub target; package Cove; target(); } external();\n",
        "package Harbor; my sub target; sub target { 1 } target(); external();\n",
    ] {
        assert_relationship_sources(
            "perl",
            "calls.pm",
            source,
            &[("target", "function", "target")],
            "calls",
        );
    }
}

#[test]
fn perl_direct_coderef_calls_cross_mcp_with_both_exact_sources() {
    assert_relationship_sources_with_count(
        "perl",
        "calls.pm",
        include_str!("../../../tests/fixtures/perl-bindings/coderef_direct_bounded.pl"),
        &[("value", "function", "&value")],
        "calls",
        Some(2),
    );
}

#[test]
fn perl_variable_coderef_copies_cross_mcp_with_exact_targets_and_sources() {
    assert_relationship_sources(
        "perl",
        "calls.pm",
        include_str!("../../../tests/fixtures/perl-bindings/coderef_copy_mutation.pl"),
        &[
            ("first", "function", "$copy->()"),
            ("second", "function", "$call->()"),
        ],
        "calls",
    );
}

#[test]
fn perl_grouped_coderef_calls_cross_mcp_with_distinct_exact_sources() {
    assert_relationship_source_sites(
        "perl",
        "calls.pm",
        include_str!("../../../tests/fixtures/perl-bindings/coderef_grouped_receiver.pl"),
        &[("value", "function", "($call)->()")],
        "calls",
        Some(2),
        Some(&[
            "($call)->()",
            "(( # transparent receiver group\n    $call\n))->()",
        ]),
    );
}

#[test]
fn perl_qualified_function_calls_cross_mcp_with_exact_sources() {
    for (source, written) in [
        (
            "package Harbor; sub Cove::target { 1 } Cove::target(); external();\n",
            "Cove::target",
        ),
        (
            "package Cove; sub target; package Harbor; sub Cove::target { 1 } Cove::target(); external();\n",
            "Cove::target",
        ),
        (
            "package Harbor; sub ::target { 1 } main::target(); external();\n",
            "main::target",
        ),
        (
            "package Cove; sub target { 1 } package main::Cove; ::main::Cove::target(); external();\n",
            "::main::Cove::target",
        ),
        (
            "sub main::main::Cove::target { 1 } ::Cove::target(); external();\n",
            "::Cove::target",
        ),
    ] {
        assert_relationship_sources(
            "perl",
            "calls.pm",
            source,
            &[("target", "function", written)],
            "calls",
        );
    }
}

#[test]
fn perl_bare_function_calls_cross_mcp_with_exact_sources() {
    for source in [
        "package Harbor; sub target { 1 } my $result = target; external();\n",
        "package Harbor; use subs ('target'); my $result = target; sub target { 1 } external();\n",
        "package Harbor; sub target { 1 } my $result = target 3; external();\n",
    ] {
        assert_relationship_sources(
            "perl",
            "calls.pm",
            source,
            &[("target", "function", "target")],
            "calls",
        );
    }
}

#[test]
fn perl_imported_and_ampersand_calls_cross_mcp_with_exact_sources() {
    assert_relationship_sources(
        "perl",
        "calls.pm",
        "package Harbor; use subs qw(reverse); sub reverse { 29 } reverse('abc'); external();\n",
        &[("reverse", "function", "reverse")],
        "calls",
    );
    let source =
        "package Harbor; my sub target { 1 } &target(); my $ref = \\&target; external();\n";
    for predicate in ["calls", "references"] {
        assert_relationship_sources(
            "perl",
            "calls.pm",
            source,
            &[("target", "function", "&target")],
            predicate,
        );
    }
}

#[test]
fn matlab_script_bindings_cross_mcp_without_function_headers() {
    assert_lexical_relationship_sources(
        "matlab",
        "measure.m",
        "% @interface Phantom\nvalue = 1;\nresult = value + 1;\ndisp(result);\n",
        &[
            ("value", "variable", "value"),
            ("result", "variable", "result"),
        ],
    );
    assert_lexical_relationship_sources(
        "matlab",
        "matrix.m",
        "values = [1 2; 3 4];\nresult = values;\ndisp(result);\n",
        &[
            ("values", "variable", "values"),
            ("result", "variable", "result"),
        ],
    );
}

#[test]
fn nix_bindings_parameters_and_signatures_cross_real_process_boundaries() {
    let source = "{ system ? \"portable\" }@args: let identity = value: value; in { inherit system; result = identity args; }";
    source_entities_with_signatures_cross_process_boundaries(
        "nix",
        "module.nix",
        source,
        &[
            ("identity", "function", 1),
            ("value", "parameter", 1),
            ("args", "parameter", 1),
            ("result", "variable", 1),
        ],
        &[("identity", "value:")],
    );
}

#[test]
fn nix_lexical_relationships_cross_mcp_with_exact_generation_bound_read_sources() {
    let source = "{ system ? \"portable\" }@args: let identity = value: value; in { inherit system; result = identity args; }";
    assert_nix_lexical_relationship_sources(
        source,
        &[
            ("identity", "function", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_quoted_lexical_relationships_preserve_authored_mcp_sources() {
    let source = r#"{ system ? "portable" }@args: let "\identity" = value: value; in { inherit "\system"; result = identity args; }"#;
    assert_nix_lexical_relationship_sources(
        source,
        &[
            (r#""\identity""#, "function", "identity"),
            ("identity", "function", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_merged_rec_sets_cross_mcp_with_source_proven_added_field_reads() {
    assert_nix_lexical_relationship_sources(
        "{ system ? \"portable\" }@args: let identity = rec { part = value: value; }; identity.other = part; in { inherit system; result = [ identity args ]; }",
        &[
            ("identity", "variable", "identity"),
            ("part", "function", "part"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_literal_keys_cross_mcp_with_authored_and_decoded_search_names() {
    assert_nix_lexical_relationship_sources(
        r#"{ system ? "portable" }@args: let ${"identity"} = value: value; in { inherit system; result = identity args; }"#,
        &[
            (r#"${"identity"}"#, "function", "identity"),
            ("identity", "function", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_implicit_path_owners_cross_mcp_as_noncallable_lexical_targets() {
    assert_nix_lexical_relationship_sources(
        "{ system ? \"portable\" }@args: let identity.part = value: value; identity.other = 2; in { inherit system; result = [ identity args ]; }",
        &[
            ("identity", "variable", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_static_path_search_keeps_dotted_keys_and_original_sources_distinct() {
    let source = r#"{ "\item" = 1; a /* trivia */ . "b" = 2; "a.b" = 3; "λ😀" = 4; }"#;
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some(("module.nix", source)), FixtureLayout::Data);
    let mut symbols = std::collections::BTreeSet::new();
    for (query, written) in [
        ("item", r#""\item" = 1;"#),
        ("a.b", r#"a /* trivia */ . "b" = 2;"#),
        (r#""a.b""#, r#""a.b" = 3;"#),
        ("λ😀", r#""λ😀" = 4;"#),
    ] {
        let arguments = json!({"query": query, "search_modes": ["exact"], "languages": ["nix"], "scope": {"paths": ["module.nix"]}, "response_profile": "evidence"});
        let located = fixture.standalone("nix-static-name", "code.locate", arguments.clone());
        assert_success(&located, "code.locate");
        let batch = fixture.batch(
            "nix-static-name-batch",
            "code.locate",
            arguments,
            "evidence",
        );
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let hits: Vec<_> = located["result"]["structuredContent"]["data"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|hit| hit["kind"] == "variable")
            .collect();
        assert_eq!(hits.len(), 1, "{query}: {located:#}");
        let symbol = hits[0]["symbol_id"].as_str().unwrap();
        assert!(symbols.insert(symbol.to_owned()));
        let reference = &hits[0]["source_ref"];
        let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
        let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
        assert_eq!(source.get(start..end), Some(written));
        let explained = fixture.standalone(
            "nix-static-explain",
            "symbol.explain",
            json!({"symbol_ids": [symbol], "response_profile": "evidence"}),
        );
        assert_success(&explained, "symbol.explain");
        assert_eq!(
            explained["result"]["structuredContent"]["data"]["symbols"][0]["definition"],
            *reference
        );
        let read = fixture.standalone("nix-static-source", "source.read", json!({"references": [{"source_ref": reference}], "context_lines_before": 0, "context_lines_after": 0, "response_profile": "evidence"}));
        assert_success(&read, "source.read");
        let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
        assert_eq!(chunk["content"], written);
        for key in ["repository", "generation", "content_hash", "span"] {
            assert_eq!(chunk["source_ref"][key], reference[key]);
        }
    }
    fixture.finish();
}

#[test]
fn nix_selected_attributes_cross_mcp_with_exact_component_sources() {
    assert_nix_lexical_relationship_sources(
        r#"{ system ? "portable" }@args: let settings = { identity = value: value; }; in { inherit system; result = settings.identity args; }"#,
        &[
            ("settings", "variable", "settings"),
            ("identity", "function", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

#[test]
fn nix_inherit_from_fields_cross_mcp_as_distinct_source_and_local_symbols() {
    assert_nix_lexical_relationship_sources(
        r#"{ system ? "portable" }@args: let settings = { identity = value: value; }; inherit (settings) identity; in { inherit system; result = identity args; }"#,
        &[
            ("settings", "variable", "settings"),
            ("identity", "function", "identity"),
            ("identity", "variable", "identity"),
            ("value", "variable", "value"),
            ("args", "variable", "args"),
        ],
    );
}

fn assert_nix_lexical_relationship_sources(source: &str, names: &[(&str, &str, &str)]) {
    assert_lexical_relationship_sources("nix", "module.nix", source, names);
}

fn assert_lexical_relationship_sources(
    language: &str,
    path: &str,
    source: &str,
    names: &[(&str, &str, &str)],
) {
    assert_relationship_sources(language, path, source, names, "references");
}

fn assert_relationship_sources(
    language: &str,
    path: &str,
    source: &str,
    names: &[(&str, &str, &str)],
    relation: &str,
) {
    assert_relationship_sources_with_count(language, path, source, names, relation, None);
}

fn assert_relationship_sources_with_count(
    language: &str,
    path: &str,
    source: &str,
    names: &[(&str, &str, &str)],
    relation: &str,
    source_count: Option<usize>,
) {
    assert_relationship_source_sites(language, path, source, names, relation, source_count, None);
}

fn assert_relationship_source_sites(
    language: &str,
    path: &str,
    source: &str,
    names: &[(&str, &str, &str)],
    relation: &str,
    source_count: Option<usize>,
    written_sites: Option<&[&str]>,
) {
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some((path, source)), FixtureLayout::Data);
    for &(name, kind, written) in names {
        let located = fixture.standalone(
            &format!("lexical-binding-{name}"),
            "code.locate",
            json!({"query": name, "search_modes": ["exact"], "languages": [language],
                "scope": {"paths": [path]}, "response_profile": "evidence"}),
        );
        assert_success(&located, "code.locate");
        let matches: Vec<_> = located["result"]["structuredContent"]["data"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"] == kind)
            .collect();
        assert_eq!(matches.len(), 1, "{located:#}");
        let symbol = matches[0]["symbol_id"].clone();
        let generation = matches[0]["source_ref"]["generation"].clone();
        let arguments = json!({"symbol_ids": [symbol.clone()], "relations": [relation],
            "direction": "inbound", "include_candidates": false, "response_profile": "evidence"});
        let response = fixture.standalone(
            &format!("lexical-references-{name}"),
            "symbol.relationships",
            arguments.clone(),
        );
        assert_success(&response, "symbol.relationships");
        let batch = fixture.batch(
            &format!("lexical-references-batch-{name}"),
            "symbol.relationships",
            arguments,
            "evidence",
        );
        assert_standalone_batch_parity(&response, &batch, "symbol.relationships");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        // Source-proven edges do not make the language's incomplete relation
        // coverage exhaustive; the API must retain its lower-bound warning.
        assert_eq!(output["data"]["totals"]["exact"], false);
        assert!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["code"] == "negative_claims_inconclusive" })
        );
        let expected_edges = source_count.unwrap_or(1);
        assert_eq!(
            output["data"]["totals"]["total_edges"], expected_edges,
            "{output:#}"
        );
        assert_eq!(output["data"]["totals"]["returned_edges"], expected_edges);
        let groups = output["data"]["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["seed"], symbol);
        assert_eq!(groups[0]["relation"], relation);
        let items = groups[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), expected_edges);
        assert_eq!(groups[0]["total_count"], expected_edges);
        let mut references: Vec<_> = items
            .iter()
            .flat_map(|item| {
                let sources = item["source_refs"].as_array().unwrap();
                assert!(!sources.is_empty());
                if source_count.is_some() {
                    assert_eq!(sources.len(), 1, "each call edge needs its own source");
                }
                sources
            })
            .collect();
        if let Some(count) = source_count {
            assert_eq!(references.len(), count, "{output:#}");
            let mut starts: Vec<_> = references
                .iter()
                .map(|reference| reference["span"]["start_byte"].as_u64().unwrap())
                .collect();
            starts.sort_unstable();
            starts.dedup();
            assert_eq!(
                starts.len(),
                count,
                "each call must have distinct source evidence"
            );
        }
        if let Some(written_sites) = written_sites {
            assert_eq!(references.len(), written_sites.len());
            references.sort_by_key(|reference| reference["span"]["start_byte"].as_u64().unwrap());
        }
        for (index, reference) in references.iter().enumerate() {
            let written = written_sites.map_or(written, |sites| sites[index]);
            assert_eq!(reference["generation"], generation);
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
            assert_eq!(source.get(start..end), Some(written));
            let read = fixture.standalone(
                &format!("lexical-reference-read-{name}-{index}"),
                "source.read",
                json!({"references": [{"source_ref": reference}], "context_lines_before": 0,
                    "context_lines_after": 0, "response_profile": "evidence"}),
            );
            assert_success(&read, "source.read");
            let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
            assert_eq!(chunk["content"], written);
            for key in ["repository", "generation", "content_hash", "span"] {
                assert_eq!(chunk["source_ref"][key], reference[key]);
            }
        }
    }
    fixture.finish();
}

#[test]
fn objective_c_forward_sources_remain_non_defining_in_current_and_retained_mcp() {
    let source = include_str!("../../../tests/fixtures/objective-c/forwards.m");
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some(("forwards.m", source)), FixtureLayout::Data);
    for (name, total, declarations, ir_kind) in [
        ("Earlier", 3, 2, "class"),
        ("Later", 1, 1, "class"),
        ("Box", 2, 1, "class"),
        ("Readable", 3, 2, "protocol"),
        ("Writable", 1, 1, "protocol"),
    ] {
        let arguments = json!({"query": name, "search_modes": ["exact"], "languages": ["objective-c"],
            "scope": {"paths": ["forwards.m"]}, "max_results": 20, "response_profile": "evidence"});
        let located = fixture.standalone("forward-locate", "code.locate", arguments.clone());
        let batch = fixture.batch("forward-batch", "code.locate", arguments, "evidence");
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        let matches = output["data"]["matches"].as_array().unwrap();
        for hit in matches.iter().filter(|hit| hit["kind"] != "type") {
            assert_eq!(hit["kind"], "file");
            assert_eq!(hit["path"], "forwards.m");
            assert!(hit["symbol_id"].is_null());
        }
        let found: Vec<_> = matches.iter().filter(|hit| hit["kind"] == "type").collect();
        assert_eq!(found.len(), total, "{output:#}");
        let mut non_defining = 0;
        let scan = fixture.standalone(
            "forward-types",
            "query.advanced",
            json!({"query": {"op": "scan", "entity": "type"}}),
        );
        assert_success(&scan, "query.advanced");
        for found in found {
            assert_eq!(found["kind"], "type");
            let symbol = &found["symbol_id"];
            let reference = &found["source_ref"];
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
            let written = source.get(start..end).unwrap();
            let is_declaration = written
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.is_empty() || suffix.trim_start().starts_with('<'));
            if is_declaration {
                non_defining += 1;
            }
            assert!(
                scan["result"]["structuredContent"]["data"]["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == *symbol && row["kind"] == ir_kind)
            );
            let arguments = json!({"symbol_ids": [symbol], "response_profile": "evidence"});
            let current =
                fixture.standalone("forward-explain", "symbol.explain", arguments.clone());
            let retained =
                fixture.standalone_version("forward-retained", "symbol.explain", arguments, "1.0");
            for response in [&current, &retained] {
                assert_success(response, "symbol.explain");
                let explanation = &response["result"]["structuredContent"]["data"]["symbols"][0];
                assert_eq!(explanation["symbol_id"], *symbol);
                assert_eq!(explanation["definition"], *reference);
                let notes: Vec<_> = explanation["uncertainty"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|note| note["code"] == "definition_is_declaration")
                    .collect();
                assert_eq!(notes.len(), usize::from(is_declaration), "{explanation:#}");
                if let Some(note) = notes.first() {
                    assert_eq!(
                        note["message"],
                        "definition reference points to a non-defining declaration"
                    );
                }
            }
            let read = fixture.standalone(
                "forward-source",
                "source.read",
                json!({"references": [{"source_ref": reference}], "response_profile": "evidence"}),
            );
            assert_success(&read, "source.read");
            let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
            assert_eq!(chunk["content"], written);
            for key in ["repository", "generation", "content_hash", "span"] {
                assert_eq!(chunk["source_ref"][key], reference[key]);
            }
        }
        assert_eq!(non_defining, declarations);
    }
    fixture.finish();
}

#[test]
fn objective_c_generic_parameters_cross_real_process_boundaries() {
    let source = include_str!("../../../tests/fixtures/objective-c/generics.m");
    source_entities_cross_process_boundaries(
        "objective-c",
        "generics.m",
        source,
        &[
            ("Element", "type_parameter", 2),
            ("Key", "type_parameter", 1),
            ("Value", "type_parameter", 1),
            ("Input", "type_parameter", 1),
            ("Other", "type_parameter", 1),
            ("Item", "type_parameter", 1),
        ],
    );
    let embedded = format!(
        "# Generic examples\n\n```objective-c\n{source}\n```\n\n```objective-c\n{source}\n```\n"
    );
    source_entities_cross_process_boundaries(
        "objective-c",
        "examples.md",
        &embedded,
        &[
            ("Element", "type_parameter", 4),
            ("Key", "type_parameter", 2),
            ("Value", "type_parameter", 2),
            ("Input", "type_parameter", 2),
            ("Other", "type_parameter", 2),
            ("Item", "type_parameter", 2),
        ],
    );
}

#[test]
fn objective_c_ivars_and_parameters_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "objective-c",
        "members.m",
        include_str!("../../../tests/fixtures/objective-c/members.m"),
        &[
            ("second", "field", 1),
            ("next", "field", 1),
            ("callback", "field", 1),
            ("handler", "field", 1),
            ("slots", "field", 1),
            ("flags", "field", 1),
            ("state", "field", 1),
            ("left", "parameter", 2),
            ("right", "parameter", 2),
            ("value", "parameter", 2),
            ("extra", "parameter", 2),
            ("input", "parameter", 2),
            ("combine:with:", "method", 2),
            ("legacy:", "method", 2),
            ("from:", "method", 2),
        ],
    );
}

#[test]
fn imported_bindings_cross_real_process_boundaries() {
    source_entities_cross_process_boundaries(
        "javascript",
        "bindings.js",
        "import { externalValue as importedValue } from './dependency.js';\nexport const consume = () => importedValue;\n",
        &[("importedValue", "import", 1)],
    );
    source_entities_cross_process_boundaries(
        "typescript",
        "view.astro",
        "---\nimport { externalValue as importedValue } from './dependency.js';\n---\n<main>{importedValue}</main>\n",
        &[("importedValue", "import", 1)],
    );
}

fn source_entities_cross_process_boundaries(
    language: &str,
    path: &str,
    source: &str,
    queries: &[(&str, &str, usize)],
) {
    source_entities_with_signatures_cross_process_boundaries(language, path, source, queries, &[]);
}

fn source_entities_with_signatures_cross_process_boundaries(
    language: &str,
    path: &str,
    source: &str,
    queries: &[(&str, &str, usize)],
    signatures: &[(&str, &str)],
) {
    let unsupported = matches!(
        language,
        "sql"
            | "r"
            | "solidity"
            | "scala"
            | "dart"
            | "powershell"
            | "markdown"
            | "objective-c"
            | "nix"
            | "perl"
    )
    .then_some(true);
    source_entities_with_coverage_cross_process_boundaries(
        language,
        path,
        source,
        queries,
        signatures,
        unsupported,
    );
}

fn source_entities_with_coverage_cross_process_boundaries(
    language: &str,
    path: &str,
    source: &str,
    queries: &[(&str, &str, usize)],
    signatures: &[(&str, &str)],
    unsupported: Option<bool>,
) {
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some((path, source)), FixtureLayout::Data);
    for &(name, expected_kind, count) in queries {
        let kind = match expected_kind {
            "property" => "field",
            "parameter" => "variable",
            "type_parameter" => "type",
            other => other,
        };
        if matches!(kind, "import" | "export") {
            let located = fixture.standalone(
                &format!("binding-path-{kind}"),
                "code.locate",
                json!({"query": path, "search_modes": ["path"], "scope": {"paths": [path]},
                    "max_results": 200, "response_profile": "evidence"}),
            );
            assert_success(&located, "code.locate");
            let output = &located["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            assert!(
                output["data"]["matches"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| {
                        item["kind"] == kind
                            && item["display_name"] == name
                            && item["path"] == path
                            && item["source_ref"].is_object()
                    }),
                "path lookup preserves the authored binding: {output:#}"
            );
        }
        let arguments = json!({"query": name, "search_modes": ["exact"], "max_results": 200,
            "languages": [language], "scope": {"paths": [path]}, "response_profile": "evidence"});
        let located = fixture.standalone(
            &format!("source-locate-{kind}"),
            "code.locate",
            arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("source-batch-{kind}"),
            "code.locate",
            arguments.clone(),
            "evidence",
        );
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        assert_eq!(output["schema_version"], "1.5");
        if let Some(unsupported) = unsupported {
            assert_eq!(
                output["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| {
                        warning["code"] == "coverage_unsupported"
                            && warning["message"].as_str().is_some_and(|message| {
                                message.ends_with(&format!("language {language}"))
                            })
                    }),
                unsupported,
                "Coverage warnings describe the selected source, not a blanket language label: {output:#}"
            );
            if !unsupported {
                let language_coverage: Vec<_> = output["coverage"]["languages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|entry| entry["language"] == language)
                    .collect();
                assert_eq!(language_coverage.len(), 1);
                assert_eq!(language_coverage[0]["status"], "complete");
                // Complete structural coverage of this source is not a semantic-provider claim.
                assert_eq!(
                    output["data"]["query_interpretation"]["semantic_available"],
                    false
                );
                assert_ne!(output["generation"]["semantic_freshness"], "current");
            }
        }
        let matches: Vec<_> = output["data"]["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .filter(|item| item["kind"] == kind)
            .collect();
        assert_eq!(matches.len(), count, "{output:#}");
        let identities: std::collections::BTreeSet<_> = matches
            .iter()
            .map(|item| item["symbol_id"].as_str().expect("symbol identity"))
            .collect();
        assert_eq!(identities.len(), count);
        for (ordinal, found) in matches.into_iter().enumerate() {
            let symbol = found["symbol_id"].clone();
            let reference = found["source_ref"].clone();
            let explained = fixture.standalone(
                &format!("source-explain-{kind}-{ordinal}"),
                "symbol.explain",
                json!({"symbol_ids": [symbol.clone()], "response_profile": "evidence"}),
            );
            assert_success(&explained, "symbol.explain");
            let explanation = &explained["result"]["structuredContent"];
            assert_eq!(explanation["schema_version"], "1.6");
            assert_eq!(explanation["data"]["symbols"][0]["kind"], kind);
            assert_eq!(explanation["data"]["symbols"][0]["symbol_id"], symbol);
            if let Some((_, signature)) = signatures.iter().find(|(query, _)| *query == name) {
                assert_eq!(explanation["data"]["symbols"][0]["signature"], *signature);
            }
            if language == "r" && kind == "function" {
                assert_eq!(
                    explanation["data"]["symbols"][0]["signature"],
                    "function(value)"
                );
            }
            let read = fixture.standalone(&format!("source-read-{kind}-{ordinal}"), "source.read",
            json!({"references": [{"source_ref": reference.clone()}], "response_profile": "evidence"}));
            assert_success(&read, "source.read");
            let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
            for field in ["repository", "generation", "content_hash", "span"] {
                assert_eq!(chunk["source_ref"][field], reference[field]);
            }
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().expect("start"))
                .expect("offset");
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().expect("end"))
                .expect("offset");
            assert_eq!(chunk["content"].as_str(), source.get(start..end));
            let advanced = fixture.standalone(
                &format!("source-scan-{kind}-{ordinal}"),
                "query.advanced",
                json!({"query": {"op": "scan", "entity": kind}}),
            );
            assert_success(&advanced, "query.advanced");
            let rows = advanced["result"]["structuredContent"]["data"]["rows"]
                .as_array()
                .expect("scan rows");
            // Public selectors group kinds; advanced rows retain the exact IR
            // kind independently asserted by these source-backed fixtures.
            let row_kind = match (language, expected_kind) {
                ("r", "variable") => "parameter",
                ("powershell", "field") => "property",
                ("scala" | "dart" | "powershell", "type") => "class",
                ("scala", "module") => "namespace",
                ("dart", "method") if name == "Store.named" => "constructor",
                _ => expected_kind,
            };
            assert!(
                rows.iter().any(|row| row["id"] == symbol
                    && row["kind"] == row_kind
                    && row["path"] == path),
                "advanced scan must retain the precise source entity: {rows:#?}"
            );
        }
        let retained = fixture.standalone_version(
            &format!("source-retained-{kind}"),
            "code.locate",
            arguments,
            "1.0",
        );
        if !matches!(kind, "import" | "export")
            && matches!(
                language,
                "r" | "scala"
                    | "dart"
                    | "powershell"
                    | "javascript"
                    | "objective-c"
                    | "nix"
                    | "matlab"
                    | "perl"
            )
        {
            // These fixtures use existing IR kinds, unlike the newer data
            // kinds that correctly require the updated retrieval schema.
            assert_success(&retained, "code.locate");
            continue;
        }
        assert_eq!(retained["result"]["isError"], true);
        assert_eq!(
            retained["result"]["structuredContent"]["error"]["code"],
            "PROTOCOL_MISMATCH"
        );
    }
    fixture.finish();
}

#[test]
fn yaml_tagged_value_gaps_cross_real_process_boundaries_with_exact_sources() {
    let source = "affected: !custom secret_value\nsafe: readable\n";
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some(("data.yaml", source)), FixtureLayout::Data);
    for (index, (query, modes, path, partial)) in [
        ("affected", json!(["exact"]), "data.yaml", true),
        ("secret_value", json!(["lexical"]), "data.yaml", true),
        ("matrix_target_alpha", json!(["exact"]), "src/lib.rs", false),
    ]
    .into_iter()
    .enumerate()
    {
        let language = if partial { "yaml" } else { "rust" };
        let arguments = json!({"query": query, "search_modes": modes, "languages": [language],
            "scope": {"paths": [path]}, "response_profile": "evidence"});
        let located = fixture.standalone(
            &format!("yaml-tag-locate-{index}"),
            "code.locate",
            arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("yaml-tag-batch-{index}"),
            "code.locate",
            arguments,
            "evidence",
        );
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        assert_eq!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "coverage_unsupported"),
            partial,
            "{output:#}"
        );
        if !partial {
            continue;
        }
        let matches = output["data"]["matches"].as_array().unwrap();
        assert!(!matches.is_empty(), "{output:#}");
        for (ordinal, hit) in matches.iter().enumerate() {
            assert_eq!(hit["path"], path);
            let reference = &hit["source_ref"];
            let read = fixture.standalone(
                &format!("yaml-tag-read-{index}-{ordinal}"),
                "source.read",
                json!({"references": [{"source_ref": reference}], "context_lines_before": 0,
                    "context_lines_after": 0, "response_profile": "evidence"}),
            );
            assert_success(&read, "source.read");
            let read = &read["result"]["structuredContent"];
            assert_common_read_contract(read, &fixture.repository_id);
            assert_eq!(read["generation"], output["generation"]);
            let chunk = &read["data"]["chunks"][0];
            for field in ["content_hash", "generation", "repository", "span"] {
                assert_eq!(chunk["source_ref"][field], reference[field]);
            }
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
            assert_eq!(chunk["content"].as_str(), source.get(start..end));
        }
    }
    fixture.finish();
}

#[test]
fn json_duplicate_and_escaped_members_cross_real_process_boundaries() {
    let source = r#"{"key":101,"k\u0065y":202,"items":[{"key":303},null,{"key":404}],"":5,"\ud800":6," ":7," key ":8}"#;
    data_properties_cross_process_boundaries(
        "json",
        "data.json",
        source,
        &[
            ("key", 4),
            (r#""""#, 1),
            (r#""\ud800""#, 1),
            (r#"" ""#, 1),
            (r#"" key ""#, 1),
        ],
    );
}

#[test]
fn toml_table_arrays_and_escaped_keys_cross_real_process_boundaries() {
    let source = "key=101\nitems=[1,{key=303}]\n[[tables]]\n\"k\\x65y\"=202\n[[tables]]\nkey=404\n\"\"=5\n\" \"=7\n\" key \"=8\n";
    data_properties_cross_process_boundaries(
        "toml",
        "data.toml",
        source,
        &[
            ("key", 4),
            (r#""""#, 1),
            (r#"" ""#, 1),
            (r#"" key ""#, 1),
            ("tables", 2),
        ],
    );
}

fn data_properties_cross_process_boundaries(
    language: &str,
    path: &str,
    source: &str,
    queries: &[(&str, usize)],
) {
    let mut fixture =
        RetrievalFixture::spawn_with_layout(Some((path, source)), FixtureLayout::Data);
    for (index, &(query, expected)) in queries.iter().enumerate() {
        let arguments = json!({"query": query, "search_modes": ["exact"], "languages": [language], "response_profile": "evidence"});
        let located = fixture.standalone(
            &format!("data-locate-{index}"),
            "code.locate",
            arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("data-batch-{index}"),
            "code.locate",
            arguments,
            "evidence",
        );
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        assert!(
            output["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .all(|warning| warning["code"] != "coverage_unsupported"),
            "{output:#}"
        );
        let matches: Vec<_> = output["data"]["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .filter(|hit| hit["symbol_id"].is_string())
            .collect();
        assert_eq!(matches.len(), expected, "{output:#}");
        let mut symbols = std::collections::BTreeSet::new();
        for (member, found) in matches.into_iter().enumerate() {
            assert!(symbols.insert(found["symbol_id"].as_str().expect("symbol")));
            let explained = fixture.standalone(
                &format!("data-explain-{index}-{member}"),
                "symbol.explain",
                json!({"symbol_ids": [found["symbol_id"]], "response_profile": "evidence"}),
            );
            assert_success(&explained, "symbol.explain");
            assert_common_read_contract(
                &explained["result"]["structuredContent"],
                &fixture.repository_id,
            );
            let read = fixture.standalone(&format!("data-read-{index}-{member}"), "source.read", json!({"references": [{"symbol_id": found["symbol_id"]}], "response_profile": "evidence"}));
            assert_success(&read, "source.read");
            let read = &read["result"]["structuredContent"];
            assert_common_read_contract(read, &fixture.repository_id);
            let chunks = read["data"]["chunks"].as_array().expect("chunks");
            assert_eq!(chunks.len(), 1);
            let reference = &chunks[0]["source_ref"];
            for field in ["content_hash", "generation", "repository", "span"] {
                assert_eq!(reference[field], found["source_ref"][field]);
            }
            let start = usize::try_from(reference["span"]["start_byte"].as_u64().expect("start"))
                .expect("offset");
            let end = usize::try_from(reference["span"]["end_byte"].as_u64().expect("end"))
                .expect("offset");
            assert_eq!(chunks[0]["content"].as_str(), source.get(start..end));
        }
    }
    fixture.finish();
}

#[test]
fn bash_symbols_and_heredoc_text_cross_real_process_boundaries() {
    let source = include_str!(
        "../../../crates/rootlight-adapter-treesitter/tests/fixtures/structural/bash.sh"
    );
    let mut fixture = RetrievalFixture::spawn_with_source(Some(("commands.sh", source)));
    for name in [
        "emit", "process", "render", "ROOT", "ITEMS", "message", "pending", "item",
    ] {
        let arguments = json!({"query": name, "search_modes": ["exact"],
            "languages": ["bash"], "response_profile": "evidence"});
        let located = fixture.standalone(
            &format!("bash-locate-{name}"),
            "code.locate",
            arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("bash-batch-{name}"),
            "code.locate",
            arguments,
            "evidence",
        );
        assert_standalone_batch_parity(&located, &batch, "code.locate");
        let output = &located["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        assert!(
            output["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .all(|warning| warning["code"] != "coverage_unsupported"),
            "Bash-only query returned unsupported coverage: {output:#}"
        );
        let matches = output["data"]["matches"].as_array().expect("Bash matches");
        let found = matches
            .iter()
            .find(|value| value["symbol_id"].is_string())
            .expect("native symbol");
        let symbol = found["symbol_id"].clone();
        let reference = found["source_ref"].clone();
        assert!(reference.is_object());
        let explained = fixture.standalone(
            &format!("bash-explain-{name}"),
            "symbol.explain",
            json!({"symbol_ids": [symbol.clone()], "response_profile": "evidence"}),
        );
        assert_success(&explained, "symbol.explain");
        assert_common_read_contract(
            &explained["result"]["structuredContent"],
            &fixture.repository_id,
        );
        let read = fixture.standalone(
            &format!("bash-source-{name}"),
            "source.read",
            json!({"references": [{"symbol_id": symbol}], "response_profile": "evidence"}),
        );
        assert_success(&read, "source.read");
        let read = &read["result"]["structuredContent"];
        assert_common_read_contract(read, &fixture.repository_id);
        let chunks = read["data"]["chunks"].as_array().expect("source chunks");
        assert_eq!(chunks.len(), 1);
        let resolved = &chunks[0]["source_ref"];
        for field in ["content_hash", "generation", "repository", "span"] {
            assert_eq!(resolved[field], reference[field]);
        }
        let start = usize::try_from(resolved["span"]["start_byte"].as_u64().expect("start"))
            .expect("offset");
        let end =
            usize::try_from(resolved["span"]["end_byte"].as_u64().expect("end")).expect("offset");
        assert_eq!(chunks[0]["content"].as_str(), source.get(start..end));
    }
    let lexical = fixture.standalone(
        "bash-heredoc-lexical",
        "code.locate",
        json!({"query": "second", "search_modes": ["lexical"], "languages": ["bash"],
            "scope": {"paths": ["commands.sh"]}, "response_profile": "evidence"}),
    );
    assert_success(&lexical, "code.locate");
    let output = &lexical["result"]["structuredContent"];
    assert_common_read_contract(output, &fixture.repository_id);
    assert!(
        !output["data"]["matches"]
            .as_array()
            .expect("lexical matches")
            .is_empty()
    );
    for (id, languages, expects_toml) in [
        ("filtered-empty", json!(["perl"]), false),
        ("unfiltered-empty", json!([]), false),
    ] {
        let absent = fixture.standalone(
            id,
            "code.locate",
            json!({"query": "absent_definition_probe", "search_modes": ["exact"],
                "languages": languages, "response_profile": "evidence"}),
        );
        assert_success(&absent, "code.locate");
        let output = &absent["result"]["structuredContent"];
        assert_eq!(output["data"]["matches"], json!([]));
        let warnings = output["warnings"]
            .as_array()
            .expect("empty-result warnings");
        assert!(warnings.iter().any(|warning| {
            warning["code"] == "coverage_unsupported"
                && warning["message"]
                    .as_str()
                    .is_some_and(|message| message.ends_with("language perl"))
        }));
        assert_eq!(
            warnings
                .iter()
                .any(|warning| warning["code"] == "coverage_unsupported"
                    && warning["message"]
                        .as_str()
                        .is_some_and(|message| message.ends_with("language toml"))),
            expects_toml
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning["code"] == "coverage_stale")
        );
    }
    fixture.finish();
}

#[test]
fn locate_kind_unions_preserve_paging_and_sources_across_processes() {
    let mut fixture = RetrievalFixture::spawn_with_layout(None, FixtureLayout::Data);
    for (case, kinds, expected) in [
        ("unrestricted", None, vec!["file", "function"]),
        ("empty", Some(json!([])), vec![]),
        ("file", Some(json!(["file"])), vec!["file"]),
        ("function", Some(json!(["function"])), vec!["function"]),
        (
            "union",
            Some(json!(["function", "file"])),
            vec!["file", "function"],
        ),
    ] {
        let mut arguments = json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "languages": ["rust"],
            "scope": {"paths": ["src"]},
            "response_profile": "evidence",
            "max_results": 1
        });
        if let Some(kinds) = kinds {
            arguments["kinds"] = kinds;
        }
        let mut observed = Vec::new();
        for page in 0..3 {
            let response = fixture.standalone(
                &format!("kind-{case}-{page}"),
                "code.locate",
                arguments.clone(),
            );
            assert_success(&response, "code.locate");
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(output, &fixture.repository_id);
            let matches = output["data"]["matches"]
                .as_array()
                .expect("locate matches");
            assert_eq!(matches.len(), usize::from(!expected.is_empty()));
            for matched in matches {
                assert_eq!(matched["path"], "src/lib.rs");
                let kind = matched["kind"].as_str().expect("entity kind");
                assert!(expected.contains(&kind));
                assert_eq!(matched["symbol_id"].is_string(), kind != "file");
                observed.push(kind.to_owned());
                let reference = &matched["source_ref"];
                assert_eq!(reference["span"]["file"], matched["file_id"]);
                assert_eq!(
                    reference["repository"],
                    output["repository"]["repository_id"]
                );
                assert_eq!(
                    reference["generation"],
                    output["generation"]["generation_id"]
                );
                let read = fixture.standalone(
                    &format!("kind-source-{case}-{page}"),
                    "source.read",
                    json!({
                        "references": [{"source_ref": reference}],
                        "response_profile": "evidence"
                    }),
                );
                assert_success(&read, "source.read");
                let chunk = &read["result"]["structuredContent"]["data"]["chunks"][0];
                for field in ["repository", "generation", "content_hash", "span"] {
                    assert_eq!(chunk["source_ref"][field], reference[field]);
                }
                let start =
                    usize::try_from(reference["span"]["start_byte"].as_u64().unwrap()).unwrap();
                let end = usize::try_from(reference["span"]["end_byte"].as_u64().unwrap()).unwrap();
                assert_eq!(chunk["content"].as_str(), RETRIEVAL_SOURCE.get(start..end));
            }
            if output["next_cursor"].is_null() {
                assert_eq!(output["truncated"], false);
                break;
            }
            assert_eq!(output["truncated"], true);
            assert!(!matches.is_empty());
            arguments["cursor"] = output["next_cursor"].clone();
            if case == "union" && page == 0 {
                let mut replay = arguments.clone();
                replay["kinds"] = json!(["file"]);
                let rejected = fixture.standalone("kind-cursor-replay", "code.locate", replay);
                assert_public_error(&rejected, "INVALID_CURSOR");
            }
        }
        observed.sort();
        assert_eq!(observed, expected);
    }
    let arguments = json!({
        "query": "matrix_target_alpha", "search_modes": ["exact"],
        "kinds": ["function"], "response_profile": "compact"
    });
    let standalone = fixture.standalone("kind-standalone", "code.locate", arguments.clone());
    let batch = fixture.batch("kind-batch", "code.locate", arguments, "compact");
    assert_standalone_batch_parity(&standalone, &batch, "code.locate");
    fixture.finish();
}

#[test]
fn retrieval_contract_matrix_crosses_real_process_boundaries() {
    let mut fixture = RetrievalFixture::spawn();
    supported_profiles_preserve_standalone_and_batch_semantics(&mut fixture);
    supported_symbol_explain_projection_crosses_process_boundaries(&mut fixture);
    supported_architecture_workflows_cross_process_boundaries(&mut fixture);
    supported_language_filters_apply_across_process_boundaries(&mut fixture);
    supported_path_scope_applies_across_process_boundaries(&mut fixture);
    source_symbol_selector_resolves_the_complete_definition(&mut fixture);
    unsupported_retrieval_options_fail_with_stable_preflight_errors(&mut fixture);
    retrieval_limits_cursors_and_unresolved_ids_are_truthful(&mut fixture);
    global_source_chunks_preserve_pagination_and_exact_tail_reads(&mut fixture);
    fixture.finish();
}

fn global_source_chunks_preserve_pagination_and_exact_tail_reads(fixture: &mut RetrievalFixture) {
    let mut cursor = Value::Null;
    let mut paths = std::collections::BTreeSet::new();
    for page in 0..2 {
        let mut arguments = json!({"query": "globalheadmarker globaltailmarker",
            "search_modes": ["lexical"], "languages": ["yaml"], "max_results": 1,
            "response_profile": "evidence"});
        if page != 0 {
            arguments["cursor"] = cursor.clone();
        }
        let response = fixture.standalone(
            &format!("full-source-page-{page}"),
            "code.locate",
            arguments,
        );
        assert_success(&response, "code.locate");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        let warnings = output["warnings"].as_array().expect("coverage warnings");
        let truncated = warnings
            .iter()
            .filter(|warning| warning["code"] == "coverage_truncated")
            .map(|warning| warning["message"].as_str().expect("warning message"))
            .collect::<Vec<_>>();
        assert_eq!(
            truncated,
            [
                "a bounded limit omitted part of the available evidence affected-files 1 language yaml"
            ]
        );
        assert!(
            warnings
                .iter()
                .all(|warning| warning["code"] != "coverage_unsupported")
        );
        let matches = output["data"]["matches"].as_array().expect("source hits");
        assert_eq!(matches.len(), 1, "full-source response: {output:#}");
        let matched = &matches[0];
        assert!(matched.get("symbol_id").is_none_or(Value::is_null));
        paths.insert(matched["path"].as_str().expect("source path").to_owned());
        let mut reference = matched["source_ref"].clone();
        let end = reference["span"]["end_byte"]
            .as_u64()
            .expect("full file extent");
        assert!(end > 32 * 1024);
        reference["span"]["start_byte"] = json!(end - 17);
        reference["span"]["end_byte"] = json!(end - 1);
        reference
            .as_object_mut()
            .expect("source reference is an object")
            .remove("line_hint");
        let read = fixture.standalone(
            &format!("full-source-read-{page}"),
            "source.read",
            json!({"references": [{"source_ref": reference}], "context_lines_before": 0,
                "context_lines_after": 0, "response_profile": "evidence"}),
        );
        assert_success(&read, "source.read");
        let read = &read["result"]["structuredContent"];
        assert_common_read_contract(read, &fixture.repository_id);
        assert_eq!(read["generation"], output["generation"]);
        assert_eq!(read["data"]["chunks"][0]["content"], "globaltailmarker");
        assert_eq!(
            read["data"]["chunks"][0]["source_ref"]["content_hash"],
            reference["content_hash"]
        );
        cursor = output["next_cursor"].clone();
        assert_eq!(cursor.is_string(), page == 0);
    }
    assert_eq!(
        paths,
        std::collections::BTreeSet::from([
            "source-tail-a.yaml".to_owned(),
            "source-tail-b.yaml".to_owned()
        ])
    );
}

fn supported_architecture_workflows_cross_process_boundaries(fixture: &mut RetrievalFixture) {
    let overview = fixture.standalone(
        "architecture-overview-rich",
        "architecture.overview",
        json!({
            "scope": {"paths": ["src"]},
            "views": [
                "modules",
                "packages",
                "services",
                "data",
                "build",
                "ownership",
                "communities",
                "hotspots"
            ],
            "detail": "detailed",
            "include_edges": true,
            "response_profile": "evidence"
        }),
    );
    assert_success(&overview, "architecture.overview");
    let overview = &overview["result"]["structuredContent"];
    assert_common_read_contract(overview, &fixture.repository_id);
    assert_eq!(overview["schema_version"], "1.1");
    assert!(overview["data"]["components"].is_array());
    assert!(overview["data"]["connections"].is_array());
    assert!(
        overview["data"]
            .get("communities")
            .is_none_or(Value::is_array)
    );
    assert!(overview["data"]["views"].is_array());
    if let Some(component) = overview["data"]["components"]
        .as_array()
        .and_then(|components| components.first())
    {
        assert!(component["file_count"].is_number());
        assert!(component["source_refs"].is_array());
    }

    let retained_overview = fixture.standalone_version(
        "architecture-overview-retained",
        "architecture.overview",
        json!({
            "views": ["hotspots"],
            "include_edges": true,
            "response_profile": "evidence"
        }),
        "1.0",
    );
    assert_success(&retained_overview, "architecture.overview");
    let retained_overview = &retained_overview["result"]["structuredContent"];
    assert_eq!(retained_overview["schema_version"], "1.0");
    if let Some(component) = retained_overview["data"]["components"]
        .as_array()
        .and_then(|components| components.first())
    {
        assert!(component.get("file_count").is_none());
        assert!(component.get("source_refs").is_none());
    }
    if let Some(hotspot) = retained_overview["data"]["hotspots"]
        .as_array()
        .and_then(|hotspots| hotspots.first())
    {
        assert!(hotspot.get("ownership_signal").is_none());
        assert!(hotspot.get("test_signal").is_none());
    }

    let cycles = fixture.standalone(
        "architecture-cycles-rich",
        "architecture.cycles",
        json!({
            "scope": {"paths": ["src"]},
            "projection": {"relations": ["calls", "imports"], "level": "module"},
            "rank_by": "edge_weight",
            "min_size": 2,
            "max_cycles": 20,
            "response_profile": "evidence"
        }),
    );
    assert_success(&cycles, "architecture.cycles");
    let cycles = &cycles["result"]["structuredContent"];
    assert_common_read_contract(cycles, &fixture.repository_id);
    assert_eq!(cycles["schema_version"], "1.1");
    assert_eq!(cycles["data"]["projection"]["level"], "module");
    assert_eq!(cycles["data"]["projection"]["rank_by"], "edge_weight");
    assert!(cycles["data"]["projection"]["relations"].is_array());

    let retained_cycles = fixture.standalone_version(
        "architecture-cycles-retained",
        "architecture.cycles",
        json!({
            "projection": {"relations": ["calls"], "level": "symbol"},
            "max_cycles": 20,
            "response_profile": "evidence"
        }),
        "1.0",
    );
    assert_success(&retained_cycles, "architecture.cycles");
    let retained_cycles = &retained_cycles["result"]["structuredContent"];
    assert_eq!(retained_cycles["schema_version"], "1.0");
    assert!(retained_cycles["data"].get("projection").is_none());
    for component in retained_cycles["data"]["components"]
        .as_array()
        .expect("retained cycle components remain an array")
    {
        assert!(component.get("edge_weight").is_none());
        assert!(component.get("change_risk").is_none());
        assert!(component.get("break_cost").is_none());
    }

    let dead = fixture.standalone(
        "code-dead-rich",
        "code.dead",
        json!({
            "scope": {"paths": ["src"]},
            "entry_point_policy": {"entry_symbols": [fixture.symbols[0].clone()]},
            "include_exported": true,
            "include_tests": true,
            "response_profile": "evidence"
        }),
    );
    assert_success(&dead, "code.dead");
    let dead = &dead["result"]["structuredContent"];
    assert_common_read_contract(dead, &fixture.repository_id);
    assert_eq!(dead["schema_version"], "1.1");
    assert_eq!(dead["data"]["entry_points"]["policy"], "explicit");
    assert!(dead["data"]["entry_points"]["entry_symbols"].is_array());
    assert!(dead["data"]["coverage_caveats"].is_array());
    for candidate in dead["data"]["candidates"]
        .as_array()
        .expect("dead-code candidates remain an array")
    {
        assert!(candidate["reachability"].is_object());
        assert!(candidate["uncertainty"].is_array());
    }

    let retained_dead = fixture.standalone_version(
        "code-dead-retained",
        "code.dead",
        json!({
            "entry_point_policy": "standard",
            "include_exported": true,
            "include_tests": true,
            "response_profile": "evidence"
        }),
        "1.0",
    );
    assert_success(&retained_dead, "code.dead");
    let retained_dead = &retained_dead["result"]["structuredContent"];
    assert_eq!(retained_dead["schema_version"], "1.0");
    assert!(
        retained_dead["data"]["entry_points"]
            .get("entry_symbols")
            .is_none()
    );
    assert!(retained_dead["data"].get("coverage_caveats").is_none());
    for candidate in retained_dead["data"]["candidates"]
        .as_array()
        .expect("retained dead-code candidates remain an array")
    {
        assert!(candidate.get("reachability").is_none());
        assert!(candidate.get("uncertainty").is_none());
    }
}

fn supported_symbol_explain_projection_crosses_process_boundaries(fixture: &mut RetrievalFixture) {
    let arguments = json!({
        "symbol_ids": [fixture.symbols[0].clone()],
        "sections": [
            "signature",
            "docs",
            "containment",
            "types",
            "calls_summary",
            "references_summary",
            "history",
            "ownership",
            "diagnostics",
            "source_preview"
        ],
        "relation_sample_limit": 1,
        "source_preview_lines": 2,
        "include_provenance": "full",
        "response_profile": "evidence"
    });
    let response = fixture.standalone(
        "explain-rich-projection",
        "symbol.explain",
        arguments.clone(),
    );
    assert_success(&response, "symbol.explain");
    let output = &response["result"]["structuredContent"];
    assert_common_read_contract(output, &fixture.repository_id);
    assert_eq!(output["schema_version"], "1.6");
    let explanation = &output["data"]["symbols"][0];
    assert!(
        explanation["qualified_name"]
            .as_str()
            .is_some_and(|name| !name.is_empty())
    );
    assert!(
        explanation["signature"]
            .as_str()
            .is_some_and(|signature| signature.contains("matrix_target_alpha")),
        "rich symbol explanation returned an unexpected signature: {explanation:#}"
    );
    assert!(
        explanation["source_preview"]
            .as_str()
            .is_some_and(|preview| preview.contains("matrix_target_alpha"))
    );
    assert!(
        explanation["relation_samples"]
            .as_array()
            .is_some_and(|samples| samples.len() <= 1)
    );
    assert!(
        explanation["provenance"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(
        explanation["section_gaps"].is_array(),
        "unavailable requested evidence is disclosed as section gaps"
    );

    let retained = fixture.standalone_version(
        "explain-rich-projection-v1",
        "symbol.explain",
        arguments,
        "1.0",
    );
    assert_success(&retained, "symbol.explain");
    let retained = &retained["result"]["structuredContent"];
    assert_eq!(retained["schema_version"], "1.0");
    let retained_explanation = &retained["data"]["symbols"][0];
    for field in [
        "qualified_name",
        "container",
        "relation_samples",
        "source_preview",
        "section_gaps",
    ] {
        assert!(
            retained_explanation.get(field).is_none(),
            "retained symbol.explain output exposed 1.1 field {field}"
        );
    }
    for provenance in retained_explanation["provenance"]
        .as_array()
        .expect("retained provenance remains an array")
    {
        assert!(provenance.get("frontend_version").is_none());
        assert!(provenance.get("rule").is_none());
    }
}

fn source_symbol_selector_resolves_the_complete_definition(fixture: &mut RetrievalFixture) {
    let arguments = json!({
        "references": [{"symbol_id": fixture.symbols[0].clone()}],
        "response_profile": "evidence"
    });
    let standalone = fixture.standalone("source-symbol-selector", "source.read", arguments.clone());
    let batch = fixture.batch(
        "batch-source-symbol-selector",
        "source.read",
        arguments,
        "evidence",
    );
    assert_standalone_batch_parity(&standalone, &batch, "source.read");
    let output = &standalone["result"]["structuredContent"];
    assert_common_read_contract(output, &fixture.repository_id);
    let chunks = output["data"]["chunks"]
        .as_array()
        .expect("symbol source selector returns chunks");
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0]["content"],
        "pub fn matrix_target_alpha(value: usize) -> usize {\n    matrix_target_beta(value).saturating_add(1)\n}"
    );
    let resolved = &chunks[0]["source_ref"];
    for field in ["content_hash", "generation", "repository", "span"] {
        assert_eq!(resolved[field], fixture.source_refs[0][field]);
    }
    assert_eq!(
        resolved["line_hint"],
        json!({"start_line": 1, "end_line": 3})
    );
}

fn supported_language_filters_apply_across_process_boundaries(fixture: &mut RetrievalFixture) {
    let matching_arguments = json!({
        "query": "matrix_target_alpha",
        "search_modes": ["exact"],
        "languages": ["rust"]
    });
    let standalone = fixture.standalone(
        "locate-language-rust",
        "code.locate",
        matching_arguments.clone(),
    );
    let batch = fixture.batch(
        "batch-locate-language-rust",
        "code.locate",
        matching_arguments,
        "compact",
    );
    assert_standalone_batch_parity(&standalone, &batch, "code.locate");
    let matching = &standalone["result"]["structuredContent"];
    assert_common_read_contract(matching, &fixture.repository_id);
    process_support::assert_symbol_and_source_matches(
        matching["data"]["matches"]
            .as_array()
            .expect("matching language returns a result"),
    );

    let excluded = fixture.standalone(
        "locate-language-python",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "languages": ["python"]
        }),
    );
    assert_success(&excluded, "code.locate");
    let excluded = &excluded["result"]["structuredContent"];
    assert_common_read_contract(excluded, &fixture.repository_id);
    assert_eq!(excluded["data"]["matches"], json!([]));
    assert_eq!(excluded["truncated"], false);
}

fn supported_path_scope_applies_across_process_boundaries(fixture: &mut RetrievalFixture) {
    let matching = fixture.standalone(
        "locate-scope-src",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "scope": {"paths": ["src"]}
        }),
    );
    let matching_batch = fixture.batch(
        "batch-locate-scope-src",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "scope": {"paths": ["src"]}
        }),
        "compact",
    );
    assert_standalone_batch_parity(&matching, &matching_batch, "code.locate");
    let matching = &matching["result"]["structuredContent"];
    assert_common_read_contract(matching, &fixture.repository_id);
    process_support::assert_symbol_and_source_matches(
        matching["data"]["matches"]
            .as_array()
            .expect("matching scope returns an array"),
    );

    let excluded = fixture.standalone(
        "locate-scope-tests",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "scope": {"paths": ["tests"]}
        }),
    );
    assert_success(&excluded, "code.locate");
    let excluded = &excluded["result"]["structuredContent"];
    assert_common_read_contract(excluded, &fixture.repository_id);
    assert_eq!(excluded["data"]["matches"], json!([]));
    assert_eq!(excluded["truncated"], false);
    assert!(excluded["next_cursor"].is_null());

    let first_unfiltered = fixture.standalone(
        "locate-scope-first-unfiltered",
        "code.locate",
        json!({
            "query": "scope_page_candidate",
            "search_modes": ["exact"],
            "max_results": 1
        }),
    );
    assert_success(&first_unfiltered, "code.locate");
    let first_path = first_unfiltered["result"]["structuredContent"]["data"]["matches"][0]["path"]
        .as_str()
        .expect("the first unfiltered page contains a path");
    let late_scope = if first_path.starts_with("src/") {
        "tests"
    } else {
        "src"
    };
    let scoped_late = fixture.standalone(
        "locate-scope-late-candidate",
        "code.locate",
        json!({
            "query": "scope_page_candidate",
            "search_modes": ["exact"],
            "scope": {"paths": [late_scope]},
            "max_results": 1
        }),
    );
    assert_success(&scoped_late, "code.locate");
    let scoped_late = &scoped_late["result"]["structuredContent"];
    let late_matches = scoped_late["data"]["matches"]
        .as_array()
        .expect("the scoped page returns matches");
    assert_eq!(late_matches.len(), 1);
    let late_path = late_matches[0]["path"]
        .as_str()
        .expect("the scoped match contains a path");
    assert!(late_path.starts_with(&format!("{late_scope}/")));
    assert_ne!(late_path, first_path);
    assert_eq!(scoped_late["truncated"], true);
    let file_page = fixture.standalone(
        "locate-scope-late-source-page",
        "code.locate",
        json!({
            "query": "scope_page_candidate",
            "search_modes": ["exact"],
            "scope": {"paths": [late_scope]},
            "max_results": 1,
            "cursor": scoped_late["next_cursor"]
        }),
    );
    assert_success(&file_page, "code.locate");
    let file_page = &file_page["result"]["structuredContent"];
    let files = file_page["data"]["matches"]
        .as_array()
        .expect("source page returns matches");
    assert_eq!(files.len(), 1);
    assert!(files[0]["symbol_id"].is_null());
    assert_eq!(files[0]["kind"], "file");
    assert_eq!(files[0]["path"], late_path);
    assert_eq!(file_page["truncated"], false);
    assert!(file_page["next_cursor"].is_null());
}

fn supported_profiles_preserve_standalone_and_batch_semantics(fixture: &mut RetrievalFixture) {
    let mut locate_outputs = Map::new();
    let mut explain_outputs = Map::new();

    for profile in ["compact", "standard", "evidence"] {
        let locate_arguments = json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "response_profile": profile
        });
        let standalone = fixture.standalone(
            &format!("locate-{profile}"),
            "code.locate",
            locate_arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("batch-locate-{profile}"),
            "code.locate",
            locate_arguments,
            profile,
        );
        assert_standalone_batch_parity(&standalone, &batch, "code.locate");
        locate_outputs.insert(
            profile.to_owned(),
            standalone["result"]["structuredContent"].clone(),
        );

        let explain_arguments = json!({
            "symbol_ids": [fixture.symbols[0].clone()],
            "include_provenance": "compact",
            "response_profile": profile
        });
        let standalone = fixture.standalone(
            &format!("explain-{profile}"),
            "symbol.explain",
            explain_arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("batch-explain-{profile}"),
            "symbol.explain",
            explain_arguments,
            profile,
        );
        assert_standalone_batch_parity(&standalone, &batch, "symbol.explain");
        explain_outputs.insert(
            profile.to_owned(),
            standalone["result"]["structuredContent"].clone(),
        );
    }

    assert_profile_identity(&locate_outputs, "/data/matches/0");
    assert_profile_identity(&explain_outputs, "/data/symbols/0");

    let source_arguments = json!({
        "references": [{"source_ref": fixture.source_refs[0].clone()}],
        "include_line_numbers": true,
        "encoding": "utf8_lossless_when_valid",
        "response_profile": "evidence"
    });
    let standalone_source =
        fixture.standalone("source-evidence", "source.read", source_arguments.clone());
    let batch_source = fixture.batch(
        "batch-source-evidence",
        "source.read",
        source_arguments,
        "evidence",
    );
    assert_standalone_batch_parity(&standalone_source, &batch_source, "source.read");
    let source = &standalone_source["result"]["structuredContent"];
    assert_common_read_contract(source, &fixture.repository_id);
    assert_eq!(source["data"]["chunks"][0]["encoding"], "utf8");
    assert_eq!(
        source["data"]["chunks"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        source["data"]["chunks"][0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("matrix_target_alpha"))
    );
    assert!(source["next_cursor"].is_null());
    assert_eq!(source["completeness"]["continuation"], "not_applicable");

    let no_provenance = fixture.standalone(
        "explain-no-provenance",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone()],
            "include_provenance": "none"
        }),
    );
    assert_success(&no_provenance, "symbol.explain");
    assert_eq!(
        no_provenance["result"]["structuredContent"]["data"]["symbols"][0]["provenance"],
        json!([])
    );

    let base64_source = fixture.standalone(
        "source-base64",
        "source.read",
        json!({
            "references": [{"source_ref": fixture.source_refs[0].clone()}],
            "encoding": "bytes_base64"
        }),
    );
    assert_success(&base64_source, "source.read");
    let base64_output = &base64_source["result"]["structuredContent"];
    assert_common_read_contract(base64_output, &fixture.repository_id);
    assert_eq!(base64_output["data"]["chunks"][0]["encoding"], "base64");
    assert!(
        base64_output["data"]["chunks"][0]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty() && content.len().is_multiple_of(4))
    );

    let contextual_source = fixture.standalone(
        "source-context",
        "source.read",
        json!({
            "references": [{"source_ref": fixture.source_refs[1].clone()}],
            "context_lines_before": 1,
            "context_lines_after": 12,
            "include_line_numbers": true
        }),
    );
    assert_success(&contextual_source, "source.read");
    let contextual_output = &contextual_source["result"]["structuredContent"];
    assert_common_read_contract(contextual_output, &fixture.repository_id);
    assert!(
        contextual_output["data"]["chunks"][0]["start_byte"]
            .as_u64()
            .zip(fixture.source_refs[1]["span"]["start_byte"].as_u64())
            .is_some_and(|(expanded, selected)| expanded < selected)
    );
    assert!(
        contextual_output["data"]["chunks"][0]["end_byte"]
            .as_u64()
            .zip(fixture.source_refs[1]["span"]["end_byte"].as_u64())
            .is_some_and(|(expanded, selected)| expanded > selected)
    );

    let mut overlapping_reference = fixture.source_refs[0].clone();
    let overlapping_start = overlapping_reference["span"]["start_byte"]
        .as_u64()
        .expect("fixture source start is an unsigned byte")
        .saturating_add(1);
    overlapping_reference["span"]["start_byte"] = json!(overlapping_start);
    let merged_source = fixture.standalone(
        "source-merge",
        "source.read",
        json!({
            "references": [
                {"source_ref": fixture.source_refs[0].clone()},
                {"source_ref": overlapping_reference}
            ],
            "merge_overlaps": true
        }),
    );
    assert_success(&merged_source, "source.read");
    assert_eq!(
        merged_source["result"]["structuredContent"]["data"]["chunks"]
            .as_array()
            .expect("overlapping reads return chunks")
            .len(),
        1
    );
}

fn unsupported_retrieval_options_fail_with_stable_preflight_errors(fixture: &mut RetrievalFixture) {
    let repository = || json!({"repository_id": fixture.repository_id});
    let source_ref = fixture.source_refs[0].clone();
    let symbol = fixture.symbols[0].clone();
    let file_selector = fixture.mcp.call(
        "source-file-selector",
        "source.read",
        json!({
            "repository": repository(),
            "references": [{
                "file_id": source_ref["span"]["file"].clone(),
                "start_byte": 0,
                "end_byte": 1
            }]
        }),
    );
    assert_public_error(&file_selector, "INVALID_ARGUMENT");

    let cases = [
        (
            "locate-scope",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "scope": {"symbols": [symbol.clone()]}}),
        ),
        (
            "locate-related",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "related_to": [symbol.clone()]}),
        ),
        (
            "locate-confidence",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "min_confidence": 700}),
        ),
        (
            "locate-docs",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["docs"]}),
        ),
        (
            "locate-semantic",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["semantic"]}),
        ),
        (
            "locate-structural",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["structural"]}),
        ),
        (
            "locate-mixed-modes",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["exact", "lexical"]}),
        ),
        (
            "source-byte-lines",
            "source.read",
            json!({
                "repository": repository(),
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "encoding": "bytes_base64",
                "include_line_numbers": true
            }),
        ),
    ];

    for (id, tool, arguments) in cases {
        let response = fixture.mcp.call(id, tool, arguments);
        assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
        let error = &response["result"]["structuredContent"]["error"];
        assert_eq!(error["message"], "requested capability is unavailable");
        assert_eq!(error["retryable"], false);
    }
}

fn retrieval_limits_cursors_and_unresolved_ids_are_truthful(fixture: &mut RetrievalFixture) {
    let first = fixture.standalone(
        "locate-page-one",
        "code.locate",
        json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 1
        }),
    );
    let first_output = &first["result"]["structuredContent"];
    assert_common_read_contract(first_output, &fixture.repository_id);
    assert_eq!(first_output["truncated"], true);
    assert_eq!(first_output["completeness"]["state"], "truncated");
    assert_eq!(first_output["completeness"]["continuation"], "available");
    assert_eq!(
        first_output["completeness"]["limiting_resources"][0]["kind"],
        "results"
    );
    let cursor = first_output["next_cursor"]
        .as_str()
        .expect("truncated locate returns an authenticated cursor")
        .to_owned();

    let second = fixture.standalone(
        "locate-page-two",
        "code.locate",
        json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 1,
            "cursor": cursor.clone()
        }),
    );
    let second_output = &second["result"]["structuredContent"];
    assert_common_read_contract(second_output, &fixture.repository_id);
    assert_ne!(
        first_output["data"]["matches"][0]["symbol_id"],
        second_output["data"]["matches"][0]["symbol_id"]
    );
    assert_eq!(first_output["repository"], second_output["repository"]);
    assert_eq!(first_output["generation"], second_output["generation"]);
    assert_eq!(first_output["trust"], second_output["trust"]);

    let mismatched_cursor = fixture.standalone(
        "locate-cursor-mismatch",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["lexical"],
            "max_results": 1,
            "cursor": cursor
        }),
    );
    assert_public_error(&mismatched_cursor, "INVALID_CURSOR");

    let first_exhaustion = collect_locate_pages(fixture, "exhaustion-first");
    let second_exhaustion = collect_locate_pages(fixture, "exhaustion-second");
    assert_eq!(
        first_exhaustion, second_exhaustion,
        "multi-page locate ordering must be repeatable"
    );
    assert_eq!(first_exhaustion.len(), 13);
    assert_eq!(
        first_exhaustion
            .iter()
            .filter(|identity| identity.starts_with("file:"))
            .count(),
        1
    );
    assert_eq!(
        first_exhaustion
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        first_exhaustion.len(),
        "multi-page locate must not duplicate results"
    );

    let absent = serde_json::to_value(SymbolId::from_bytes([0xff; 20]))
        .expect("stable symbol identity serializes");
    let mixed = fixture.standalone(
        "explain-mixed",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone(), absent.clone()],
            "include_provenance": "compact",
            "response_profile": "evidence"
        }),
    );
    let mixed_output = &mixed["result"]["structuredContent"];
    assert_common_read_contract(mixed_output, &fixture.repository_id);
    assert_eq!(mixed_output["truncated"], false);
    assert_eq!(
        mixed_output["data"]["symbols"][0]["symbol_id"],
        fixture.symbols[0]
    );
    assert_eq!(mixed_output["data"]["unresolved_ids"], json!([absent]));
    assert_eq!(
        mixed_output["data"]["symbols"][0]["definition"]["repository"],
        fixture.repository_id
    );
    assert_eq!(
        mixed_output["data"]["symbols"][0]["definition"]["generation"],
        mixed_output["generation"]["generation_id"]
    );
    assert_eq!(
        mixed_output["data"]["symbols"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        mixed_output["data"]["symbols"][0]["provenance"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let limited_explain = fixture.standalone(
        "explain-result-limit",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone(), fixture.symbols[1].clone()],
            "budget": {"max_results": 2}
        }),
    );
    assert_success(&limited_explain, "symbol.explain");
    let limited_output = &limited_explain["result"]["structuredContent"];
    assert_common_read_contract(limited_output, &fixture.repository_id);
    assert_eq!(limited_output["truncated"], true);
    assert_eq!(limited_output["completeness"]["state"], "truncated");
    assert_eq!(
        limited_output["completeness"]["limiting_resources"][0]["kind"],
        "results"
    );
    assert_eq!(
        limited_output["data"]["symbols"]
            .as_array()
            .expect("bounded explain returns resolved symbols")
            .len(),
        1
    );
    assert_eq!(limited_output["data"]["unresolved_ids"], json!([]));

    for (id, tool, arguments) in [
        (
            "locate-token-budget",
            "code.locate",
            json!({
                "query": "matrix_target_alpha",
                "search_modes": ["exact"],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "explain-token-budget",
            "symbol.explain",
            json!({
                "symbol_ids": [fixture.symbols[0].clone()],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "source-token-budget",
            "source.read",
            json!({
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "source-byte-budget",
            "source.read",
            json!({
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "max_source_bytes": 1
            }),
        ),
    ] {
        let response = fixture.standalone(id, tool, arguments);
        assert_public_error(&response, "BUDGET_EXCEEDED");
        let error = &response["result"]["structuredContent"]["error"];
        assert_eq!(error["retryable"], false);
        assert!(error["repository"].is_null());
        assert!(error["generation"].is_null());
    }
}

fn collect_locate_pages(fixture: &mut RetrievalFixture, run_id: &str) -> Vec<String> {
    let mut cursor = None;
    let mut identity = None;
    let mut identities = Vec::new();
    for page_index in 0..16 {
        let mut arguments = json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 2
        });
        if let Some(cursor) = cursor.take() {
            arguments["cursor"] = json!(cursor);
        }
        let response = fixture.standalone(
            &format!("{run_id}-page-{page_index}"),
            "code.locate",
            arguments,
        );
        assert_success(&response, "code.locate");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        let observed_identity = (
            output["repository"].clone(),
            output["generation"].clone(),
            output["trust"].clone(),
        );
        if let Some(expected) = &identity {
            assert_eq!(&observed_identity, expected);
        } else {
            identity = Some(observed_identity);
        }
        let matches = output["data"]["matches"]
            .as_array()
            .expect("locate page returns matches");
        assert!(
            !matches.is_empty(),
            "locate emitted an empty intermediate page"
        );
        identities.extend(matches.iter().map(|matched| {
            if let Some(symbol) = matched["symbol_id"].as_str() {
                symbol.to_owned()
            } else {
                assert_eq!(matched["kind"], "file");
                let file = matched["file_id"]
                    .as_str()
                    .expect("source match has a file identity");
                format!("file:{file}")
            }
        }));

        let Some(next_cursor) = output["next_cursor"].as_str() else {
            assert_eq!(output["truncated"], false);
            assert_eq!(output["completeness"]["state"], "complete");
            assert_eq!(output["completeness"]["continuation"], "not_applicable");
            return identities;
        };
        assert_eq!(output["truncated"], true);
        assert_eq!(output["completeness"]["state"], "truncated");
        assert_eq!(output["completeness"]["continuation"], "available");
        cursor = Some(next_cursor.to_owned());
    }
    panic!("locate pagination did not terminate within the bounded page count");
}

fn assert_profile_identity(outputs: &Map<String, Value>, item_path: &str) {
    for field in [
        "repository",
        "generation",
        "coverage",
        "truncated",
        "completeness",
        "next_cursor",
        "trust",
    ] {
        assert_eq!(outputs["compact"][field], outputs["standard"][field]);
        assert_eq!(outputs["standard"][field], outputs["evidence"][field]);
    }
    for field in [
        "symbol_id",
        "file_id",
        "kind",
        "display_name",
        "path",
        "score",
    ] {
        let path = format!("{item_path}/{field}");
        let compact = outputs["compact"].pointer(&path);
        let standard = outputs["standard"].pointer(&path);
        let evidence = outputs["evidence"].pointer(&path);
        if compact.is_some() || standard.is_some() || evidence.is_some() {
            assert_eq!(compact, standard, "profile changed {path}");
            assert_eq!(standard, evidence, "profile changed {path}");
        }
    }
}

fn assert_standalone_batch_parity(standalone: &Value, batch: &Value, tool: &str) {
    assert_success(standalone, tool);
    assert_success(batch, "query.batch");
    let standalone = &standalone["result"]["structuredContent"];
    let batch = &batch["result"]["structuredContent"];
    let operation = &batch["data"]["operation_results"][0];

    assert_common_usage(standalone);
    assert_common_usage(batch);
    assert_eq!(
        batch["data"]["batch_status"], "ok",
        "batch did not preserve {tool}: {batch:#}"
    );
    assert_eq!(operation["tool"], tool);
    assert_eq!(
        operation["status"], "ok",
        "batch child did not preserve {tool}: {batch:#}"
    );
    assert!(operation.get("error").is_none());
    assert_eq!(operation["data"], standalone["data"]);
    assert_eq!(
        operation["truncated"], standalone["truncated"],
        "standalone: {standalone:#}\nbatch: {batch:#}"
    );
    assert_eq!(operation["next_cursor"], standalone["next_cursor"]);
    assert_eq!(
        batch["repository"]["repository_id"],
        standalone["repository"]["repository_id"]
    );
    assert_eq!(batch["generation"], standalone["generation"]);
    assert_eq!(
        batch["data"]["generation_id"],
        standalone["generation"]["generation_id"]
    );
    assert_eq!(batch["trust"], standalone["trust"]);
    assert_eq!(batch["truncated"], standalone["truncated"]);
    assert_eq!(batch["completeness"], standalone["completeness"]);
}

fn assert_common_read_contract(output: &Value, repository_id: &str) {
    assert!(
        matches!(
            output["schema_version"].as_str(),
            Some("1.0" | "1.1" | "1.2" | "1.3" | "1.4" | "1.5" | "1.6")
        ),
        "read response uses a supported additive schema version"
    );
    assert_eq!(output["repository"]["repository_id"], repository_id);
    assert!(output["generation"]["generation_id"].is_string());
    assert_ne!(
        output["generation"]["generation_id"],
        output["generation"]["parent_generation"]
    );
    assert_eq!(output["trust"], "untrusted_repository_data");
    assert!(output["coverage"]["languages"].is_array());
    assert!(output["completeness"]["state"].is_string());
    assert!(output.get("next_cursor").is_some());
    assert_common_usage(output);
}

fn assert_common_usage(output: &Value) {
    let usage = &output["usage"];
    for field in [
        "rows",
        "edges",
        "source_bytes",
        "json_bytes",
        "estimated_tokens",
        "wall_time_ms",
    ] {
        assert!(
            usage[field].as_u64().is_some(),
            "usage.{field} is an unsigned counter"
        );
    }
    assert!(
        usage["trace_id"]
            .as_str()
            .is_some_and(|trace| !trace.is_empty())
    );
    let serialized = serde_json::to_vec(output).expect("structured output serializes");
    assert_eq!(
        usage["json_bytes"].as_u64(),
        Some(u64::try_from(serialized.len()).expect("response length fits u64"))
    );
    assert_eq!(
        usage["estimated_tokens"].as_u64(),
        Some(estimate_tokens(serialized.len()))
    );
}

fn assert_success(response: &Value, tool: &str) {
    assert!(
        response.get("error").is_none(),
        "{tool} returned a JSON-RPC error: {response:#}"
    );
    assert_ne!(
        response["result"]["isError"], true,
        "{tool} returned a public error: {response:#}"
    );
    assert!(
        response["result"]["structuredContent"].is_object(),
        "{tool} did not return structured content"
    );
}

fn assert_public_error(response: &Value, expected: &str) {
    assert!(
        response.get("error").is_none(),
        "public tool failure escaped as JSON-RPC: {response:#}"
    );
    assert_eq!(
        response["result"]["isError"], true,
        "expected {expected} public error: {response:#}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"], expected,
        "unexpected process error: {response:#}"
    );
}

enum FixtureLayout {
    Full,
    Data,
}

struct RetrievalFixture {
    _root: tempfile::TempDir,
    daemon: DaemonProcess,
    mcp: McpProcess,
    repository_id: String,
    symbols: Vec<Value>,
    source_refs: Vec<Value>,
}

impl RetrievalFixture {
    fn spawn() -> Self {
        Self::spawn_with_source(None)
    }

    fn spawn_with_source(extra_source: Option<(&str, &str)>) -> Self {
        Self::spawn_with_layout(extra_source, FixtureLayout::Full)
    }

    fn spawn_with_layout(extra_source: Option<(&str, &str)>, layout: FixtureLayout) -> Self {
        Self::spawn_with_sources(extra_source.as_slice(), layout)
    }

    fn spawn_with_sources(extra_sources: &[(&str, &str)], layout: FixtureLayout) -> Self {
        let root = process_support::private_process_tempdir("rl-retrieval-");
        let repository_root = root.path().join("repository");
        fs::create_dir_all(repository_root.join("src"))
            .expect("fixture source directory is created");
        fs::create_dir_all(repository_root.join("tests"))
            .expect("fixture test directory is created");
        for (path, source) in extra_sources {
            let path = repository_root.join(path);
            fs::create_dir_all(path.parent().unwrap()).expect("extra source parent");
            fs::write(path, source).expect("extra source fixture");
        }
        fs::write(
            repository_root.join("Cargo.toml"),
            "[package]\nname = \"retrieval_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("fixture manifest is written");
        fs::write(repository_root.join("src").join("lib.rs"), RETRIEVAL_SOURCE)
            .expect("fixture source is written");
        fs::write(
            repository_root.join("src").join("scope_page.rs"),
            SCOPED_PAGE_SOURCE,
        )
        .expect("scoped source fixture is written");
        fs::write(
            repository_root.join("tests").join("scope_page.rs"),
            SCOPED_PAGE_SOURCE,
        )
        .expect("scoped test fixture is written");

        if matches!(layout, FixtureLayout::Full) {
            fs::write(repository_root.join("unsupported.pl"), "print 1;\n")
                .expect("source-fallback fixture writes");
            let terms = (0..5_000)
                .map(|index| format!("item{index:05}value{index:05}"))
                .collect::<Vec<_>>()
                .join(" ");
            let full_source = format!("# globalheadmarker\n# {terms}\n# globaltailmarker\n");
            for path in ["source-tail-a.yaml", "source-tail-b.yaml"] {
                fs::write(repository_root.join(path), &full_source)
                    .expect("tail source fixture writes");
            }
            fs::write(
                repository_root.join("omitted-word.yaml"),
                format!("# {}\n", "x".repeat(241)),
            )
            .expect("omitted-word source writes");
        }

        let state_dir = root.path().join("state");
        let runtime_dir = root.path().join("runtime");
        let daemon_binary = ensure_daemon_binary();
        let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
        daemon.wait_until_ready(&runtime_dir);
        let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
        let arguments = json!({"root": repository_root, "mode": "auto", "detached": false});
        let index = process_support::retry_transient_busy("index", |attempt_id| {
            mcp.call(attempt_id, "repo.index", arguments.clone())
        });
        assert_success(&index, "repo.index");
        let repository_id = index["result"]["structuredContent"]["data"]["repository_id"]
            .as_str()
            .expect("repo.index returns a repository identity")
            .to_owned();
        let operation_id = index["result"]["structuredContent"]["data"]["operation_id"]
            .as_str()
            .expect("repo.index returns an operation identity")
            .to_owned();
        wait_for_publication(&mut mcp, &index, &operation_id);

        let mut symbols = Vec::new();
        let mut source_refs = Vec::new();
        for (index, query) in [
            "matrix_target_alpha",
            "matrix_target_beta",
            "matrix_target_gamma",
        ]
        .into_iter()
        .enumerate()
        {
            let locate = mcp.call(
                &format!("setup-locate-{index}"),
                "code.locate",
                json!({
                    "repository": {"repository_id": repository_id},
                    "generation": "active",
                    "query": query,
                    "search_modes": ["exact"],
                    "response_profile": "evidence"
                }),
            );
            assert_success(&locate, "code.locate");
            let matched = &locate["result"]["structuredContent"]["data"]["matches"][0];
            symbols.push(matched["symbol_id"].clone());
            source_refs.push(matched["source_ref"].clone());
        }

        Self {
            _root: root,
            daemon,
            mcp,
            repository_id,
            symbols,
            source_refs,
        }
    }

    fn standalone(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        let mut arguments = arguments
            .as_object()
            .expect("retrieval arguments are objects")
            .clone();
        arguments.insert(
            "repository".to_owned(),
            json!({"repository_id": self.repository_id}),
        );
        arguments.insert("generation".to_owned(), json!("active"));
        self.mcp.call(id, tool, Value::Object(arguments))
    }

    fn standalone_version(
        &mut self,
        id: &str,
        tool: &str,
        arguments: Value,
        version: &str,
    ) -> Value {
        let mut arguments = arguments
            .as_object()
            .expect("retrieval arguments are objects")
            .clone();
        arguments.insert(
            "repository".to_owned(),
            json!({"repository_id": self.repository_id}),
        );
        arguments.insert("generation".to_owned(), json!("active"));
        self.mcp
            .call_version(id, tool, Value::Object(arguments), version)
    }

    fn batch(&mut self, id: &str, tool: &str, arguments: Value, response_profile: &str) -> Value {
        let mut arguments = arguments
            .as_object()
            .expect("batch retrieval arguments are objects")
            .clone();
        arguments.remove("response_profile");
        self.mcp.call(
            id,
            "query.batch",
            json!({
                "repository": {"repository_id": self.repository_id},
                "generation": "active",
                "response_profile": response_profile,
                "operations": [{
                    "id": "retrieval",
                    "tool": tool,
                    "arguments": arguments
                }]
            }),
        )
    }

    fn finish(mut self) {
        self.mcp.finish();
        self.daemon.finish();
    }
}

fn wait_for_publication(mcp: &mut McpProcess, index: &Value, operation_id: &str) {
    if index["result"]["structuredContent"]["data"]["state"] == "published" {
        return;
    }
    for attempt in 0..30 {
        let status = mcp.call(
            &format!("operation-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&status, "operation.status");
        match status["result"]["structuredContent"]["data"]["operation"]["state"].as_str() {
            Some("published") => return,
            Some("failed" | "cancelled") => {
                panic!("fixture indexing terminated without publication: {status:#}")
            }
            _ => {}
        }
    }
    panic!("fixture indexing did not publish within the bounded wait");
}

fn ensure_daemon_binary() -> PathBuf {
    static DAEMON_BINARY: OnceLock<PathBuf> = OnceLock::new();
    DAEMON_BINARY.get_or_init(build_daemon_binary).clone()
}

fn build_daemon_binary() -> PathBuf {
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let profile_dir = mcp_binary
        .parent()
        .expect("MCP binary has a profile directory");
    let daemon = profile_dir.join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX));
    if daemon.is_file() {
        return daemon;
    }

    let target_dir = profile_dir
        .parent()
        .expect("profile directory belongs to a Cargo target directory");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(&workspace)
        .args([
            OsStr::new("build"),
            OsStr::new("--locked"),
            OsStr::new("-p"),
            OsStr::new("rootlight-daemon"),
            OsStr::new("--bin"),
            OsStr::new("rootlight-daemon"),
            OsStr::new("--target-dir"),
        ])
        .arg(target_dir)
        .output()
        .expect("test-only daemon build starts");
    assert!(
        output.status.success(),
        "test-only daemon build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.is_file(), "daemon build did not produce {daemon:?}");
    daemon
}

struct DaemonProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    stderr_path: PathBuf,
}

impl DaemonProcess {
    fn spawn(binary: &Path, state_dir: &Path, runtime_dir: &Path) -> Self {
        let stderr_path = runtime_dir.join("retrieval-daemon.stderr");
        fs::create_dir_all(runtime_dir).expect("daemon runtime directory is available");
        let stderr = fs::File::create(&stderr_path).expect("daemon stderr file is available");
        let mut child = Command::new(binary)
            .arg("--supervised-stdio")
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("isolated daemon process starts");
        let input = child.stdin.take().expect("daemon stdin is piped");
        Self {
            child: Some(child),
            input: Some(input),
            stderr_path,
        }
    }

    fn wait_until_ready(&mut self, runtime_dir: &Path) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let discovery = runtime_dir.join("daemon.json");
        while Instant::now() < deadline {
            if discovery.is_file() {
                return;
            }
            if self
                .child
                .as_mut()
                .expect("daemon child is retained")
                .try_wait()
                .expect("daemon status is readable")
                .is_some()
            {
                let stderr = fs::read_to_string(&self.stderr_path)
                    .unwrap_or_else(|error| format!("<daemon stderr unavailable: {error}>"));
                panic!("daemon exited before publishing discovery: {stderr}");
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon did not publish discovery within the startup bound");
    }

    fn finish(&mut self) {
        self.input.take();
        let status = wait_for_exit(
            self.child.as_mut().expect("daemon child is retained"),
            SHUTDOWN_TIMEOUT,
        );
        let stderr = fs::read_to_string(&self.stderr_path)
            .unwrap_or_else(|error| format!("<daemon stderr unavailable: {error}>"));
        assert!(
            status.success(),
            "daemon process exits successfully: status={status}; stderr={stderr}"
        );
        self.child.take();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
    }
}

struct McpProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
}

impl McpProcess {
    fn spawn(state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
            .current_dir(
                state_dir
                    .parent()
                    .expect("fixture state directory has a launch root"),
            )
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP fixture process starts");
        let mut process = Self {
            input: child.stdin.take(),
            output: child.stdout.take().map(BufReader::new),
            child: Some(child),
        };
        process.write(&json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "retrieval-process", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": "developer"}
            }
        }));
        assert_eq!(process.read()["id"], "initialize");
        process.write(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));
        process
    }

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }));
        let response = self.read();
        assert_eq!(response["id"], id);
        response
    }

    fn call_version(&mut self, id: &str, tool: &str, arguments: Value, version: &str) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": arguments,
                "_meta": {"rootlight/toolContractVersion": version}
            }
        }));
        let response = self.read();
        assert_eq!(response["id"], id);
        response
    }

    fn write(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("MCP stdin is retained");
        serde_json::to_writer(&mut *input, message).expect("MCP request serializes");
        input.write_all(b"\n").expect("MCP request terminates");
        input.flush().expect("MCP request flushes");
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output
            .as_mut()
            .expect("MCP stdout is retained")
            .read_line(&mut line)
            .expect("MCP response reads");
        serde_json::from_str(&line).expect("MCP response is valid JSON")
    }

    fn finish(&mut self) {
        self.input.take();
        self.output.take();
        let child = self.child.as_mut().expect("MCP child is retained");
        let status = wait_for_exit(child, SHUTDOWN_TIMEOUT);
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("MCP stderr is piped")
            .read_to_string(&mut stderr)
            .expect("MCP stderr reads");
        assert!(status.success(), "MCP process exits successfully: {stderr}");
        assert!(stderr.is_empty(), "MCP process wrote stderr: {stderr}");
        self.child.take();
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        terminate(&mut self.child);
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("child status is readable") {
            return status;
        }
        thread::sleep(Duration::from_millis(25));
    }
    child.kill().expect("timed-out child is terminated");
    child.wait().expect("terminated child is reaped")
}

fn terminate(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
