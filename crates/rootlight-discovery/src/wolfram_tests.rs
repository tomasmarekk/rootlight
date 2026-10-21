//! Wolfram routing remains separate from native analysis and ambiguous .m fallback.
//! First-expression boundaries prevent comments, literals or shared suffixes from
//! donating a package header to another language.

use super::*;
use rootlight_config::{ConfigLayer, ConfigSource};
use rootlight_ids::derive_repository;

fn content_signals(path: &str, source: &[u8]) -> Vec<String> {
    classify(&RelativePath::parse(Path::new(path)).unwrap(), source)
        .1
        .into_iter()
        .filter(|signal| signal.evidence == LanguageEvidence::Content)
        .map(|signal| signal.language)
        .collect()
}

#[test]
fn wolfram_extensions_and_aliases_advertise_only_source_fallback() {
    for path in ["kernel.wl", "scripts/start.wls", "PACKAGE.WL", "SCRIPT.WLS"] {
        assert_eq!(extension_language(path), Some("wolfram"), "{path}");
    }
    for path in ["source.m", "source.wl.bak", "source.wls.rs", "source.txt"] {
        assert_ne!(extension_language(path), Some("wolfram"), "{path}");
    }
    for name in ["wolfram", "wolfram-language", "mathematica"] {
        assert_eq!(canonical_language(name), Some("wolfram"), "{name}");
    }
    let capability = language_capabilities()
        .iter()
        .find(|capability| capability.language == "wolfram")
        .unwrap();
    assert_eq!(capability.analyzers, &["treesitter"]);
}

#[test]
fn wolfram_first_package_expression_disambiguates_shared_m_sources() {
    for source in [
        "BeginPackage[\"Example`\"];\nvalue[x_] := x;\nEndPackage[];\n",
        "(* ::Package:: *)\nPaclet[Name -> \"Example\", Version -> \"1.0\"]\n",
        "\u{feff}(* outer (* nested *) comment *)\r\nBeginPackage (* head *) [\"Example`\", {\"Other`\"}];\r\n",
        "Paclet[(* name *)Name(* rule *)->(* value *)\"Example\"]",
    ] {
        assert_eq!(
            content_signals("package.m", source.as_bytes()),
            ["wolfram"],
            "{source}"
        );
    }
    assert_eq!(
        content_signals("kernel.m", b"Get[\"Example`Kernel`\"];\n"),
        Vec::<String>::new(),
        "An arbitrary Get call needs independent repository context"
    );
}

#[test]
fn wolfram_headers_do_not_escape_comments_strings_or_known_foreign_suffixes() {
    for source in [
        "(* BeginPackage[\"Hidden`\"] *) value = 1;\n",
        "(* outer (* Paclet[Name -> \"Hidden\"] *) comment *) value = 1;\n",
        "\"BeginPackage[\\\"Hidden`\\\"]\";\n",
        "value = BeginPackage[\"Hidden`\"];\n",
        "BeginPackageExtra[\"Hidden`\"];\n",
        "PacletExtra[Name -> \"Hidden\"];\n",
        "BeginPackage[\"unterminated",
        "BeginPackage[\"Example`\"",
        "(* unfinished BeginPackage[\"Hidden`\"];\n",
        "// BeginPackage[\"Hidden`\"]\n@interface Example\n@end\n",
        "const char *text = \"BeginPackage[\\\"Hidden`\\\"]\";\n",
    ] {
        assert!(
            !content_signals("example.m", source.as_bytes())
                .iter()
                .any(|language| language == "wolfram"),
            "{source}"
        );
    }
    for path in [
        "header.h",
        "source.c",
        "source.mm",
        "source.txt",
        "config.json",
    ] {
        assert!(
            !content_signals(path, b"BeginPackage[\"Example`\"];\n")
                .iter()
                .any(|language| language == "wolfram"),
            "{path}"
        );
    }
    assert_eq!(
        content_signals("example.m", b"@interface Example\n@end\n"),
        ["objective-c"]
    );
    assert_eq!(
        content_signals("example.m", b"function y = example(x)\ny = x;\nend\n"),
        ["matlab"]
    );
}

#[test]
fn wolfram_headers_respect_classification_prefix_and_encoding_validation() {
    let header = "BeginPackage[\"Example`\"];\n";
    let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
    outside.extend_from_slice(header.as_bytes());
    assert!(
        !content_signals("late.m", &outside)
            .iter()
            .any(|language| language == "wolfram")
    );
    for little_endian in [true, false] {
        let source: Vec<_> = std::iter::once(0xfeff_u16)
            .chain(header.encode_utf16())
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        let classified = encoding::classify(&source, &Cancellation::new())
            .unwrap()
            .unwrap();
        let decoded = classified.decoded_prefix.as_ref().unwrap();
        let signals = classify_validated_prefix(
            &RelativePath::parse(Path::new("encoded.m")).unwrap(),
            decoded.as_bytes(),
            classified.prefix_truncated,
            true,
            true,
        )
        .1;
        assert!(
            signals.iter().any(|signal| signal.language == "wolfram"
                && signal.evidence == LanguageEvidence::Content)
        );
    }
}

const ORIGINAL_PACKAGE: &[u8] = include_bytes!("../tests/fixtures/wolfram/OriginalPackage.m");
const ORIGINAL_PACLET: &[u8] = include_bytes!("../tests/fixtures/wolfram/OriginalPaclet.m");
const ORIGINAL_INIT: &[u8] = include_bytes!("../tests/fixtures/wolfram/OriginalInit.m");

#[test]
fn wolfram_original_package_headers_route_by_written_source() {
    assert_eq!(ORIGINAL_PACKAGE.len(), 14_040);
    assert_eq!(ORIGINAL_PACLET.len(), 339);
    assert_eq!(ORIGINAL_INIT.len(), 96);
    for original in [ORIGINAL_PACKAGE, ORIGINAL_PACLET] {
        assert_eq!(content_signals("arbitrary.m", original), ["wolfram"]);
    }
    assert!(content_signals("arbitrary.m", ORIGINAL_INIT).is_empty());
}

#[test]
fn wolfram_shared_m_context_keeps_original_snapshots_and_refuses_mixed_peers() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    for (name, bytes) in [
        ("package.m", ORIGINAL_PACKAGE),
        ("paclet.m", ORIGINAL_PACLET),
        ("init.m", ORIGINAL_INIT),
    ] {
        std::fs::write(directory.path().join(name), bytes).unwrap();
    }
    let root = RepositoryRoot::open(
        derive_repository(b"shared-m-wolfram").id(),
        directory.path(),
    )
    .unwrap();
    let config = ConfigSnapshot::resolve(&[ConfigLayer {
        source: ConfigSource::Defaults,
        contents: "version = \"1.0\"",
    }])
    .unwrap();
    let inspect = || {
        discover_with_snapshots(
            &root,
            &config,
            &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
            DiscoveryLimits::new(20, 8, 1024 * 1024, 20).unwrap(),
            BTreeMap::new(),
            &Cancellation::new(),
        )
        .unwrap()
        .into_parts()
    };
    let (manifest, snapshots) = inspect();
    assert_eq!(manifest.inputs.len(), 3);
    let init = manifest
        .inputs
        .iter()
        .find(|input| input.path == "init.m")
        .unwrap();
    assert!(
        init.language_signals
            .iter()
            .any(|signal| signal.language == "wolfram"
                && signal.evidence == LanguageEvidence::Context)
    );
    assert_eq!(snapshots[&init.file].content(), ORIGINAL_INIT);
    for (name, bytes) in [
        ("package.m", ORIGINAL_PACKAGE),
        ("paclet.m", ORIGINAL_PACLET),
    ] {
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.path == name)
            .unwrap();
        assert_eq!(snapshots[&input.file].content(), bytes);
    }
    std::fs::write(
        directory.path().join("paclet.m"),
        b"@interface Example\n@end\n",
    )
    .unwrap();
    let (manifest, _) = inspect();
    let init = manifest
        .inputs
        .iter()
        .find(|input| input.path == "init.m")
        .unwrap();
    assert!(
        !init
            .language_signals
            .iter()
            .any(|signal| signal.evidence == LanguageEvidence::Context)
    );
    std::fs::remove_file(directory.path().join("paclet.m")).unwrap();
    let (manifest, _) = inspect();
    let init = manifest
        .inputs
        .iter()
        .find(|input| input.path == "init.m")
        .unwrap();
    assert!(
        !init
            .language_signals
            .iter()
            .any(|signal| signal.evidence == LanguageEvidence::Context)
    );
}
