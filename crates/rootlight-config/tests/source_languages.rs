//! Exercises explicit source-language assignments through the public configuration boundary.
//! Assignments are critical, hashed configuration rather than inferred dialect evidence.

use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};

#[test]
fn source_language_extension_is_recognized_and_hashed() {
    let contents = r#"
version = "1.3"
[extensions.source-languages]
version = "1.0"
critical = true
[extensions.source-languages.data.extensions]
bas = "vba"
[extensions.source-languages.data.paths]
"legacy/module.bas" = "vb6"
"#;
    let resolved = ConfigSnapshot::resolve(&[ConfigLayer {
        source: ConfigSource::User,
        contents,
    }]);
    assert!(resolved.is_ok(), "{resolved:?}");
    let resolved = resolved.unwrap();
    assert_eq!(
        resolved.source_language_for_path("src/MODULE.BAS"),
        Some("vba")
    );
    assert_eq!(
        resolved.source_language_for_path("legacy/module.bas"),
        Some("vb6")
    );
    assert_eq!(
        resolved.source_language_for_path("Legacy/module.bas"),
        Some("vba")
    );
    assert_eq!(resolved.source_language_for_path("../module.bas"), None);
    assert_eq!(resolved.source_language_for_path("folder.bas/file"), None);
    assert_ne!(
        resolved.hash(),
        ConfigSnapshot::resolve(&[]).unwrap().hash()
    );
}

#[test]
fn source_language_layers_replace_rules_and_keep_payloads_redacted() {
    let lower = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n";
    let upper = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'private/path.bas'='vb6'\n";
    let resolved = ConfigSnapshot::resolve(&[
        ConfigLayer {
            source: ConfigSource::User,
            contents: lower,
        },
        ConfigLayer {
            source: ConfigSource::Repository,
            contents: upper,
        },
    ])
    .unwrap();
    assert_eq!(resolved.source_language_for_path("other.bas"), None);
    assert_eq!(
        resolved.source_language_for_path("private/path.bas"),
        Some("vb6")
    );
    assert!(!format!("{resolved:?}").contains("private/path.bas"));
    assert_eq!(
        resolved.provenance()["extensions.source-languages"],
        ConfigSource::Repository
    );
    let cleared = ConfigSnapshot::resolve(&[
        ConfigLayer { source: ConfigSource::User, contents: lower },
        ConfigLayer { source: ConfigSource::Operation, contents: "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\ndata={}\n" },
    ]).unwrap();
    assert_eq!(cleared.source_language_for_path("module.bas"), None);
    assert_ne!(cleared.hash(), resolved.hash());
}

#[test]
fn invalid_source_language_rules_are_rejected() {
    for data in [
        "extensions={'.bas'='vba'}",
        "extensions={'BAS'='vba'}",
        "extensions={bas='VBA'}",
        "extensions={bas=''}",
        "extensions={bas=1}",
        "paths={'../secret'='vba'}",
        "paths={'/module.bas'='vba'}",
        "paths={'a//b'='vba'}",
        "paths={'a/./b'='vba'}",
        "paths={'C:/file'='vba'}",
        "paths={'*.bas'='vba'}",
        "paths={'a\\b'='vba'}",
        "path={file='vba'}",
    ] {
        let contents = format!(
            "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\ndata={{ {data} }}\n"
        );
        assert!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: &contents
            }])
            .is_err(),
            "{data}"
        );
    }
    for (version, critical) in [("1.0", false), ("1.1", true), ("2.0", true)] {
        let contents = format!(
            "version='1.3'\n[extensions.source-languages]\nversion='{version}'\ncritical={critical}\ndata={{}}\n"
        );
        assert!(
            ConfigSnapshot::resolve(&[ConfigLayer {
                source: ConfigSource::User,
                contents: &contents
            }])
            .is_err()
        );
    }
}
