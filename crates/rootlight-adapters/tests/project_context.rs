//! Public-boundary tests for declarative language project contexts.
//!
//! Fixtures cover every metadata family, deterministic normalization, strict
//! schema admission, hard limits, and fail-closed coverage declarations.

use rootlight_adapters::{
    MAX_PROJECT_CONTEXT_BYTES, ProjectContext, ProjectContextCoverageStatus,
    ProjectContextLanguage, ProjectContextMetadata,
};
use serde_json::{Value, json};

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn cxx_context_normalizes_order_and_derives_stable_identity() {
    let first = cxx_manifest(
        json!(["include/z", "include/a"]),
        json!(["DEBUG=1", "API=2"]),
    );
    let second = cxx_manifest(
        json!(["include/a", "include/z"]),
        json!(["API=2", "DEBUG=1"]),
    );

    let first = decode(&first);
    let second = decode(&second);

    assert_eq!(first.identity(), second.identity());
    assert_eq!(first.language(), ProjectContextLanguage::Cpp);
    assert_eq!(first.target(), "native-library");
    assert_eq!(
        first.coverage().status(),
        ProjectContextCoverageStatus::Complete
    );
    assert_eq!(
        first
            .files()
            .iter()
            .map(|file| file.path())
            .collect::<Vec<_>>(),
        vec!["include/api.hpp", "src/api.cpp"]
    );
    let ProjectContextMetadata::Cxx(metadata) = first.metadata() else {
        panic!("C++ input must retain C++ metadata");
    };
    assert_eq!(metadata.include_paths(), ["include/a", "include/z"]);
    assert_eq!(metadata.defines(), ["API=2", "DEBUG=1"]);
    assert_eq!(metadata.macros()[0].source(), "compile_commands.json");
}

#[test]
fn jvm_dotnet_and_php_contexts_retain_language_specific_evidence() {
    let jvm = decode(&json!({
        "schema_version": 1,
        "language": "kotlin",
        "target": "server",
        "files": [{"path": "src/main/kotlin/App.kt", "generated_from": null}],
        "metadata": {
            "kind": "jvm",
            "project_model_digest": DIGEST,
            "targets": [":server"],
            "source_sets": ["main"],
            "classpath_entries": ["libs/runtime.jar"],
            "generated_roots": ["build/generated"],
            "framework_routes": [{"value": "GET /health", "source": "routes.conf"}]
        },
        "coverage": {"status": "complete", "skips": []}
    }));
    let ProjectContextMetadata::Jvm(jvm_metadata) = jvm.metadata() else {
        panic!("Kotlin input must retain JVM metadata");
    };
    assert_eq!(jvm_metadata.targets(), [":server"]);
    assert_eq!(jvm_metadata.framework_routes()[0].value(), "GET /health");

    let dotnet = decode(&json!({
        "schema_version": 1,
        "language": "csharp",
        "target": "Api",
        "files": [{"path": "Api/Controller.cs", "generated_from": null}],
        "metadata": {
            "kind": "dotnet",
            "project_model_digest": DIGEST,
            "projects": ["Api/Api.csproj"],
            "target_frameworks": ["net9.0"],
            "partial_types": [{"value": "ApiController", "source": "Api/Controller.cs"}],
            "async_symbols": [{"value": "GetAsync", "source": "Api/Controller.cs"}],
            "delegates": [],
            "linq_expressions": [],
            "routes": [{"value": "GET /api", "source": "Api/Controller.cs"}]
        },
        "coverage": {"status": "complete", "skips": []}
    }));
    let ProjectContextMetadata::Dotnet(dotnet_metadata) = dotnet.metadata() else {
        panic!("C# input must retain .NET metadata");
    };
    assert_eq!(dotnet_metadata.projects(), ["Api/Api.csproj"]);
    assert_eq!(dotnet_metadata.async_symbols()[0].value(), "GetAsync");

    let php = decode(&json!({
        "schema_version": 1,
        "language": "php",
        "target": "web",
        "files": [{"path": "src/Controller.php", "generated_from": null}],
        "metadata": {
            "kind": "php",
            "composer_lock_digest": DIGEST,
            "autoload_roots": ["src"],
            "namespaces": ["App"],
            "traits": [{"value": "AuthorizesRequests", "source": "src/Controller.php"}],
            "dynamic_calls": [{"value": "handler candidates", "source": "routes.php"}],
            "routes": [{"value": "GET /", "source": "routes.php"}]
        },
        "coverage": {
            "status": "bounded",
            "skips": [{"code": "runtime_registration", "detail": "runtime hooks were not executed"}]
        }
    }));
    let ProjectContextMetadata::Php(php_metadata) = php.metadata() else {
        panic!("PHP input must retain PHP metadata");
    };
    assert_eq!(php_metadata.autoload_roots(), ["src"]);
    assert_eq!(php.coverage().skips()[0].code(), "runtime_registration");
}

#[test]
fn deep_language_contexts_bind_build_selection_and_generated_origins() {
    let rust = decode(&json!({
        "schema_version": 1,
        "language": "rust",
        "target": "rootlight-service",
        "files": [{
            "path": "target/generated/schema.rs",
            "generated_from": "proto/schema.proto",
            "origin_mappings": [{
                "generated_start_byte": 0,
                "generated_end_byte": 24,
                "origin_path": "proto/schema.proto",
                "origin_start_byte": 8,
                "origin_end_byte": 32,
                "transformation": "prost",
                "generator_digest": DIGEST
            }]
        }],
        "metadata": {
            "kind": "rust",
            "cargo_metadata_digest": DIGEST,
            "cargo_lock_digest": DIGEST,
            "workspace_members": ["rootlight-service"],
            "targets": ["lib"],
            "source_roots": ["crates/rootlight-service/src"],
            "edition": "2024",
            "target_triple": "x86_64-pc-windows-msvc",
            "enabled_features": ["default"],
            "cfgs": ["windows"],
            "compiler_version": "rustc 1.97.1",
            "precise_index_digest": null
        },
        "coverage": {"status": "complete", "skips": []}
    }));
    let ProjectContextMetadata::Rust(rust_metadata) = rust.metadata() else {
        panic!("Rust input must retain Cargo metadata");
    };
    assert_eq!(rust_metadata.edition(), "2024");
    assert_eq!(
        rust.files()[0].origin_mappings()[0].origin_path(),
        "proto/schema.proto"
    );

    let typescript = decode(&json!({
        "schema_version": 1,
        "language": "typescript",
        "target": "web",
        "files": [{"path": "src/index.ts", "generated_from": null}],
        "metadata": {
            "kind": "typescript",
            "config_digest": DIGEST,
            "package_lock_digest": DIGEST,
            "project_references": ["packages/shared/tsconfig.json"],
            "source_roots": ["src"],
            "type_roots": ["types"],
            "path_mappings": [{"value": "@shared/*=packages/shared/*", "source": "tsconfig.json"}],
            "module_resolution": "bundler",
            "language_target": "es2024",
            "jsx_mode": null,
            "semantic_frontend_version": "typescript 6.0",
            "precise_index_digest": null
        },
        "coverage": {"status": "complete", "skips": []}
    }));
    let ProjectContextMetadata::Typescript(ts_metadata) = typescript.metadata() else {
        panic!("TypeScript input must retain project-graph metadata");
    };
    assert_eq!(ts_metadata.module_resolution(), "bundler");

    let python = decode(&json!({
        "schema_version": 1,
        "language": "python",
        "target": "api",
        "files": [{"path": "src/api/__init__.py", "generated_from": null}],
        "metadata": {
            "kind": "python",
            "pyproject_digest": DIGEST,
            "lock_digest": null,
            "package_roots": ["src"],
            "import_paths": ["src"],
            "namespace_packages": ["api"],
            "stub_roots": ["typings"],
            "type_checker": "pyright",
            "semantic_frontend_version": null,
            "precise_index_digest": null
        },
        "coverage": {
            "status": "bounded",
            "skips": [{"code": "dynamic_imports", "detail": "runtime imports were not executed"}]
        }
    }));
    let ProjectContextMetadata::Python(python_metadata) = python.metadata() else {
        panic!("Python input must retain package metadata");
    };
    assert_eq!(python_metadata.package_roots(), ["src"]);

    let go = decode(&json!({
        "schema_version": 1,
        "language": "go",
        "target": "server",
        "files": [{"path": "cmd/server/main.go", "generated_from": null}],
        "metadata": {
            "kind": "go",
            "go_mod_digest": DIGEST,
            "go_work_digest": null,
            "go_sum_digest": DIGEST,
            "modules": ["example.test/rootlight"],
            "packages": ["example.test/rootlight/cmd/server"],
            "replacements": [],
            "build_tags": ["production"],
            "goos": "windows",
            "goarch": "amd64",
            "vendor_mode": false,
            "cgo_enabled": false,
            "semantic_frontend_version": "gopls 0.20",
            "precise_index_digest": null
        },
        "coverage": {"status": "complete", "skips": []}
    }));
    let ProjectContextMetadata::Go(go_metadata) = go.metadata() else {
        panic!("Go input must retain module metadata");
    };
    assert_eq!(go_metadata.goos(), "windows");
    assert_eq!(go_metadata.goarch(), "amd64");
}

#[test]
fn paired_language_families_accept_each_declared_language() {
    let mut c = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    c["language"] = json!("c");
    assert_eq!(decode(&c).language(), ProjectContextLanguage::C);

    let java = json!({
        "schema_version": 1,
        "language": "java",
        "target": "application",
        "files": [{"path": "src/main/java/App.java", "generated_from": null}],
        "metadata": {
            "kind": "jvm",
            "project_model_digest": DIGEST,
            "targets": [":application"],
            "source_sets": ["main"],
            "classpath_entries": [],
            "generated_roots": [],
            "framework_routes": []
        },
        "coverage": {"status": "complete", "skips": []}
    });
    assert_eq!(decode(&java).language(), ProjectContextLanguage::Java);
}

#[test]
fn context_rejects_unknown_fields_family_mismatch_and_ambiguous_coverage() {
    let mut unknown = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    unknown
        .as_object_mut()
        .expect("fixture is an object")
        .insert("unexpected".to_owned(), json!(true));
    assert!(ProjectContext::decode_json(&encode(&unknown)).is_err());

    let mut mismatch = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    mismatch["language"] = json!("php");
    assert!(ProjectContext::decode_json(&encode(&mismatch)).is_err());

    let mut contradictory = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    contradictory["coverage"] = json!({
        "status": "complete",
        "skips": [{"code": "missing_flags", "detail": "flags were unavailable"}]
    });
    assert!(ProjectContext::decode_json(&encode(&contradictory)).is_err());

    let mut unexplained = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    unexplained["coverage"] = json!({"status": "unknown", "skips": []});
    assert!(ProjectContext::decode_json(&encode(&unexplained)).is_err());
}

#[test]
fn context_rejects_duplicate_values_invalid_paths_and_oversized_input() {
    let duplicate = cxx_manifest(json!(["include", "include"]), json!(["DEBUG=1"]));
    assert!(ProjectContext::decode_json(&encode(&duplicate)).is_err());

    let mut invalid_path = cxx_manifest(json!(["include"]), json!(["DEBUG=1"]));
    invalid_path["files"][0]["path"] = json!("../outside.cpp");
    assert!(ProjectContext::decode_json(&encode(&invalid_path)).is_err());

    let oversized = vec![b' '; MAX_PROJECT_CONTEXT_BYTES + 1];
    assert!(ProjectContext::decode_json(&oversized).is_err());
}

fn cxx_manifest(include_paths: Value, defines: Value) -> Value {
    json!({
        "schema_version": 1,
        "language": "cpp",
        "target": "native-library",
        "files": [
            {"path": "src/api.cpp", "generated_from": null},
            {"path": "include/api.hpp", "generated_from": null}
        ],
        "metadata": {
            "kind": "cxx",
            "compile_database_digest": DIGEST,
            "working_directory": "build",
            "include_paths": include_paths,
            "defines": defines,
            "conditional_symbols": ["_WIN32"],
            "macros": [{"value": "API_EXPORT", "source": "compile_commands.json"}]
        },
        "coverage": {"status": "complete", "skips": []}
    })
}

fn decode(value: &Value) -> ProjectContext {
    ProjectContext::decode_json(&encode(value)).expect("fixture project context is valid")
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("fixture JSON serializes")
}
