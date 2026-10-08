//! Cross-crate tests for capability-confined discovery reconciliation.
//!
//! The fixtures mutate only files beneath an opened repository root and verify
//! authoritative scans without supplying watcher events.

use std::{
    fs::{self, FileTimes, OpenOptions},
    time::{Duration, SystemTime},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_config::ConfigSnapshot;
use rootlight_discovery::{
    DiscoveryError, DiscoveryLimits, DiscoveryPolicy, IncrementalDiscoveryBaseline,
    IncrementalDiscoveryContext, IncrementalDiscoveryOptions, InputClass, LanguageEvidence,
    LanguageSignal, ManifestReconcileInputs, ProjectLanguageWitness,
    correlate_incremental_manifest, discover, discover_incremental,
    discover_incremental_with_progress, discover_manifest_and_reconcile_with_progress,
    discover_with_snapshots,
};
use rootlight_ids::{FactId, RepositoryId, content_hash, derive_repository};
use rootlight_incremental::{ChangeClass, FileChangeKind, ReconcileMode};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::{TempDir, tempdir_in};

fn local_tempdir() -> TempDir {
    let current = std::env::current_dir().expect("current directory is available");
    tempdir_in(current).expect("local temporary directory is available")
}

fn root(temporary: &TempDir, seed: &[u8]) -> RepositoryRoot {
    RepositoryRoot::open(derive_repository(seed).id(), temporary.path())
        .expect("fixture repository opens")
}

fn policy() -> DiscoveryPolicy {
    DiscoveryPolicy::build(Vec::new(), false).expect("empty fixture policy builds")
}

fn limits() -> DiscoveryLimits {
    DiscoveryLimits::new(1_000, 16, 1024 * 1024, 100).expect("fixture limits are valid")
}

fn context(configuration: &[u8], provider: &[u8]) -> IncrementalDiscoveryContext {
    IncrementalDiscoveryContext::new(
        content_hash(configuration),
        FactId::from_bytes([7; 20]),
        content_hash(provider),
    )
}

#[test]
fn no_op_reuse_requires_a_platform_change_token_and_audit_always_rehashes() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-no-op");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");

    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");
    assert_eq!(first.hashed_files().len(), 1);

    let no_op = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("no-op reconcile succeeds");
    #[cfg(unix)]
    assert!(no_op.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(no_op.hashed_files().len(), 1);
    assert!(no_op.changes().is_empty());
    assert_eq!(no_op.file_changes()[0].kind(), FileChangeKind::NoChange);

    let audit = discover_incremental(
        &root,
        Some(no_op.baseline()),
        context,
        &policy,
        ReconcileMode::Audit,
        limits(),
        &Cancellation::new(),
    )
    .expect("audit reconcile succeeds");
    assert_eq!(audit.hashed_files().len(), 1);
    assert!(audit.changes().is_empty());
}

#[test]
fn cold_incremental_discovery_reports_monotonic_file_and_byte_progress() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("first.rs"), b"aa").expect("first source is written");
    fs::write(temporary.path().join("second.rs"), b"bbbb").expect("second source is written");
    let root = root(&temporary, b"incremental-progress");
    let mut progress = Vec::new();

    let discovery = discover_incremental_with_progress(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy(),
        IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits()),
        &Cancellation::new(),
        |observed| progress.push(observed),
    )
    .expect("cold discovery succeeds");

    assert_eq!(discovery.hashed_files().len(), 2);
    assert_eq!(progress.len(), 2);
    assert_eq!(progress[0].files_examined, 1);
    assert_eq!(progress[1].files_examined, 2);
    assert!(progress[0].bytes_examined > 0);
    assert!(progress[1].bytes_examined >= progress[0].bytes_examined);
    assert_eq!(progress[1].bytes_examined, 6);
}

#[test]
fn clean_manifest_reconcile_matches_independent_correlation() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("first.rs"), b"pub fn first() {}\n")
        .expect("first fixture source is written");
    fs::write(temporary.path().join("second.rs"), b"pub fn second() {}\n")
        .expect("second fixture source is written");
    let root = root(&temporary, b"manifest-reconcile");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let policy = policy();
    let cancellation = Cancellation::new();
    let mut progress = Vec::new();

    let (manifest, combined) = discover_manifest_and_reconcile_with_progress(
        &root,
        &config,
        ManifestReconcileInputs::new(None, context),
        &policy,
        limits(),
        &cancellation,
        |observed| progress.push(observed),
    )
    .expect("combined cold discovery succeeds");
    let independently_observed = discover_incremental_with_progress(
        &root,
        None,
        context,
        &policy,
        IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits())
            .without_hashed_snapshot_retention(),
        &cancellation,
        |_| {},
    )
    .expect("independent cold reconcile succeeds");
    let independent_manifest = discover(&root, &config, &policy, limits(), &cancellation)
        .expect("independent clean manifest succeeds");
    let independently_correlated = correlate_incremental_manifest(
        &independently_observed,
        None,
        context,
        &independent_manifest,
        limits(),
        &cancellation,
    )
    .expect("independent cold observations correlate");

    assert_eq!(manifest, independent_manifest);
    assert_eq!(combined, independently_correlated);
    assert_eq!(progress.len(), 2);
    assert_eq!(progress.last().map(|item| item.files_examined), Some(2));

    let (no_op_manifest, no_op) = discover_manifest_and_reconcile_with_progress(
        &root,
        &config,
        ManifestReconcileInputs::new(Some(combined.baseline()), context),
        &policy,
        limits(),
        &cancellation,
        |_| {},
    )
    .expect("combined no-op discovery succeeds");
    let no_op_observed = discover_incremental_with_progress(
        &root,
        Some(combined.baseline()),
        context,
        &policy,
        IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits())
            .without_hashed_snapshot_retention(),
        &cancellation,
        |_| {},
    )
    .expect("independent no-op reconcile succeeds");
    let independent_no_op_manifest = discover(&root, &config, &policy, limits(), &cancellation)
        .expect("independent no-op manifest succeeds");
    let independently_correlated_no_op = correlate_incremental_manifest(
        &no_op_observed,
        Some(combined.baseline()),
        context,
        &independent_no_op_manifest,
        limits(),
        &cancellation,
    )
    .expect("independent no-op observations correlate");

    assert_eq!(no_op_manifest, independent_no_op_manifest);
    assert_eq!(no_op, independently_correlated_no_op);
    assert!(no_op.changes().is_empty());

    fs::write(
        temporary.path().join("first.rs"),
        b"pub fn changed() { dbg!(); }\n",
    )
    .expect("fixture source mutation succeeds");
    let (changed_manifest, changed) = discover_manifest_and_reconcile_with_progress(
        &root,
        &config,
        ManifestReconcileInputs::new(Some(no_op.baseline()), context),
        &policy,
        limits(),
        &cancellation,
        |_| {},
    )
    .expect("combined changed discovery succeeds");
    let changed_observed = discover_incremental_with_progress(
        &root,
        Some(no_op.baseline()),
        context,
        &policy,
        IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits())
            .without_hashed_snapshot_retention(),
        &cancellation,
        |_| {},
    )
    .expect("independent changed reconcile succeeds");
    let independent_changed_manifest = discover(&root, &config, &policy, limits(), &cancellation)
        .expect("independent changed manifest succeeds");
    let independently_correlated_change = correlate_incremental_manifest(
        &changed_observed,
        Some(no_op.baseline()),
        context,
        &independent_changed_manifest,
        limits(),
        &cancellation,
    )
    .expect("independent changed observations correlate");

    assert_eq!(changed_manifest, independent_changed_manifest);
    assert_eq!(changed, independently_correlated_change);
    assert!(!changed.changes().is_empty());
}

#[test]
fn clean_manifest_reconcile_does_not_promote_excluded_sources() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join(".gitignore"), b"ignored.rs\n")
        .expect("ignore fixture is written");
    fs::write(
        temporary.path().join("visible.rs"),
        b"pub fn visible() {}\n",
    )
    .expect("visible fixture is written");
    fs::write(
        temporary.path().join("ignored.rs"),
        b"pub fn ignored() {}\n",
    )
    .expect("ignored fixture is written");
    fs::write(temporary.path().join("binary.rs"), [0, 1, 2, 3]).expect("binary fixture is written");
    let root = root(&temporary, b"manifest-reconcile-exclusions");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );

    let (manifest, discovery) = discover_manifest_and_reconcile_with_progress(
        &root,
        &config,
        ManifestReconcileInputs::new(None, context),
        &policy(),
        limits(),
        &Cancellation::new(),
        |_| {},
    )
    .expect("combined discovery succeeds");
    let ignored = root.file_id(
        &RelativePath::parse(std::path::Path::new("ignored.rs"))
            .expect("ignored fixture path is valid"),
    );
    let binary = root.file_id(
        &RelativePath::parse(std::path::Path::new("binary.rs"))
            .expect("binary fixture path is valid"),
    );
    let baseline_files = discovery
        .baseline()
        .metadata()
        .files()
        .map(|file| file.descriptor().file())
        .collect::<Vec<_>>();

    assert!(!manifest.inputs.iter().any(|input| input.file == ignored));
    assert!(!manifest.inputs.iter().any(|input| input.file == binary));
    assert!(!baseline_files.contains(&ignored));
    assert!(!baseline_files.contains(&binary));
    assert_eq!(baseline_files.len(), manifest.inputs.len());
}

#[test]
fn fsscript_routes_follow_incremental_add_rename_remove_and_no_op() {
    let temporary = local_tempdir();
    let script = "\u{feff}// Unicode source preservation: λ\r\nprintfn \"ready\"\r\n".as_bytes();
    let signature = b"val value: int\r\n";
    let unit = b"// project language evidence\r\n";
    let project = b"<Project><ItemGroup><Compile Include='Unit.fs'/><Compile Include='Script.fsscript'/><Compile Include='ignored.fsscript'/><Compile Include='Missing.fsscript'/></ItemGroup></Project>";
    for (path, source) in [
        ("Script.fsx", script),
        ("Ambiguous.fs", script),
        ("Contract.fsi", signature.as_slice()),
        ("Unit.fs", unit.as_slice()),
        ("App.fsproj", project.as_slice()),
        ("ignored.fsscript", script),
        ("binary.fsscript", b"\0binary".as_slice()),
        (".gitignore", b"ignored.fsscript\n".as_slice()),
    ] {
        fs::write(temporary.path().join(path), source).expect("source fixture is written");
    }
    let root = root(&temporary, b"fsscript-incremental-routes");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let observe = |parent: Option<&IncrementalDiscoveryBaseline>, script_path: Option<&str>| {
        let observed = discover_manifest_and_reconcile_with_progress(
            &root,
            &config,
            ManifestReconcileInputs::new(parent, context),
            &policy,
            limits(),
            &cancellation,
            |_| {},
        )
        .expect("script changes reconcile");
        let clean = discover(&root, &config, &policy, limits(), &cancellation)
            .expect("independent clean discovery succeeds");
        let (retained, snapshots) = discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            Default::default(),
            &cancellation,
        )
        .expect("fresh source snapshots are retained")
        .into_parts();
        assert_eq!(observed.0, clean);
        assert_eq!(observed.0, retained);
        let manifest = &observed.0;
        assert!(manifest.coverage.complete);
        assert!(observed.1.is_complete());
        assert_eq!(
            manifest.inputs.len(),
            observed.1.baseline().metadata().len()
        );
        for path in ["ignored.fsscript", "binary.fsscript", "Missing.fsscript"] {
            assert!(!manifest.inputs.iter().any(|input| input.path == path));
            let file = root.file_id(
                &RelativePath::parse(std::path::Path::new(path)).expect("fixture path is valid"),
            );
            assert!(
                !observed
                    .1
                    .baseline()
                    .metadata()
                    .files()
                    .any(|entry| entry.descriptor().file() == file)
            );
        }
        let project_input = manifest
            .inputs
            .iter()
            .find(|input| input.path == "App.fsproj")
            .unwrap();
        let member = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Unit.fs")
            .unwrap();
        assert_eq!(
            member.language_signals,
            [LanguageSignal {
                language: "fsharp".to_owned(),
                evidence: LanguageEvidence::Project,
            }]
        );
        assert_eq!(
            member.project_language_witnesses,
            [ProjectLanguageWitness {
                language: "fsharp".to_owned(),
                project_file: project_input.file,
                project_path: project_input.path.clone(),
                project_content_hash: content_hash(project),
            }]
        );
        for (path, source, language) in [
            ("Script.fsx", script, Some("fsharp")),
            ("Ambiguous.fs", script, None),
            (
                "Contract.fsi",
                signature.as_slice(),
                Some("fsharp-signature"),
            ),
        ]
        .into_iter()
        .chain(script_path.map(|path| (path, script, Some("fsharp"))))
        {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.class, InputClass::Source);
            assert_eq!(input.encoding, "utf-8");
            assert_eq!(input.bytes, u64::try_from(source.len()).unwrap());
            assert_eq!(input.content_hash, content_hash(source));
            assert_eq!(snapshots[&input.file].content(), source);
            assert_eq!(
                input.file,
                root.file_id(&RelativePath::parse(std::path::Path::new(path)).unwrap())
            );
            assert!(input.project_language_witnesses.is_empty());
            assert_eq!(
                input.language_signals,
                language
                    .into_iter()
                    .map(|language| LanguageSignal {
                        language: language.to_owned(),
                        evidence: LanguageEvidence::Extension,
                    })
                    .collect::<Vec<_>>()
            );
        }
        observed
    };
    let (initial_manifest, initial) = observe(None, None);
    let script_path = temporary.path().join("Script.fsscript");
    fs::write(&script_path, script).expect("script alias is added");
    let (added_manifest, added) = observe(Some(initial.baseline()), Some("Script.fsscript"));
    let added_file = added_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Script.fsscript")
        .unwrap()
        .file;
    assert!(
        added
            .file_changes()
            .iter()
            .any(|change| change.kind() == FileChangeKind::Added
                && change.current_file() == Some(added_file))
    );
    let (no_op_manifest, no_op) = observe(Some(added.baseline()), Some("Script.fsscript"));
    assert_eq!(no_op_manifest, added_manifest);
    assert!(no_op.changes().is_empty());

    let renamed_path = temporary.path().join("Renamed.FSSCRIPT");
    fs::rename(&script_path, &renamed_path).expect("script alias is renamed");
    let (renamed_manifest, renamed) = observe(Some(no_op.baseline()), Some("Renamed.FSSCRIPT"));
    let renamed_file = renamed_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Renamed.FSSCRIPT")
        .unwrap()
        .file;
    assert_ne!(renamed_file, added_file);
    assert!(
        !renamed_manifest
            .inputs
            .iter()
            .any(|input| input.file == added_file)
    );
    assert!(renamed.file_changes().iter().any(|change| {
        change.kind() == FileChangeKind::Moved
            && change.previous_file() == Some(added_file)
            && change.current_file() == Some(renamed_file)
            && change.previous_hash() == Some(content_hash(script))
            && change.current_hash() == Some(content_hash(script))
    }));

    let short_path = temporary.path().join("Renamed.fsx");
    fs::rename(&renamed_path, &short_path).expect("script alias changes to the short suffix");
    let (short_manifest, short) = observe(Some(renamed.baseline()), Some("Renamed.fsx"));
    let short_file = short_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Renamed.fsx")
        .unwrap()
        .file;
    assert_ne!(short_file, renamed_file);
    assert!(
        short
            .file_changes()
            .iter()
            .any(|change| change.kind() == FileChangeKind::Moved
                && change.previous_file() == Some(renamed_file)
                && change.current_file() == Some(short_file))
    );

    fs::remove_file(&short_path).expect("script alias is removed");
    let (removed_manifest, removed) = observe(Some(short.baseline()), None);
    assert_eq!(removed_manifest, initial_manifest);
    assert!(
        removed
            .file_changes()
            .iter()
            .any(|change| change.kind() == FileChangeKind::Deleted
                && change.previous_file() == Some(short_file))
    );
}

#[test]
fn conditioned_project_witnesses_follow_incremental_declaration_changes() {
    let temporary = local_tempdir();
    let source = b"// source language supplied by the project\n";
    for path in ["Source.fs", "Other.fs"] {
        fs::write(temporary.path().join(path), source)
            .expect("ambiguous source fixture is written");
    }
    fs::create_dir(temporary.path().join("ignored")).expect("ignored directory is created");
    fs::write(temporary.path().join("ignored/Hidden.fs"), source)
        .expect("ignored source fixture is written");
    fs::write(temporary.path().join(".gitignore"), b"ignored/\n")
        .expect("ignore fixture is written");
    let root = root(&temporary, b"conditioned-project-declaration-changes");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let observe = |parent: Option<&IncrementalDiscoveryBaseline>| {
        discover_manifest_and_reconcile_with_progress(
            &root,
            &config,
            ManifestReconcileInputs::new(parent, context),
            &policy,
            limits(),
            &cancellation,
            |_| {},
        )
        .expect("project declaration changes reconcile")
    };
    let (initial_manifest, initial) = observe(None);
    for input in &initial_manifest.inputs {
        if input.path.ends_with(".fs") {
            assert!(input.language_signals.is_empty());
            assert!(input.project_language_witnesses.is_empty());
        }
    }

    let project_path = temporary.path().join("App.fsproj");
    let document = b"<Project Condition='false'><ItemGroup Condition='false'><Compile Condition='false' Include='Source.fs'/><Compile Include='ignored/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>";
    fs::write(&project_path, document).expect("conditioned project fixture is written");
    let (added_manifest, added) = observe(Some(initial.baseline()));
    let project = added_manifest
        .inputs
        .iter()
        .find(|input| input.path == "App.fsproj")
        .expect("admitted project is retained");
    let member = added_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Source.fs")
        .expect("declared source is retained");
    assert_eq!(
        member.language_signals,
        [LanguageSignal {
            language: "fsharp".to_owned(),
            evidence: LanguageEvidence::Project,
        }]
    );
    assert_eq!(
        member.project_language_witnesses,
        [ProjectLanguageWitness {
            language: "fsharp".to_owned(),
            project_file: project.file,
            project_path: project.path.clone(),
            project_content_hash: content_hash(document),
        }]
    );
    let (no_op_manifest, no_op) = observe(Some(added.baseline()));
    assert_eq!(no_op_manifest, added_manifest);
    assert!(no_op.changes().is_empty());

    let edited_document = b"<Project Condition='false'><ItemGroup Condition='false'><Compile Condition='false' Include='Other.fs'/><Compile Include='ignored/Hidden.fs'/><Compile Include='Missing.fs'/></ItemGroup></Project>";
    fs::write(&project_path, edited_document).expect("literal project declaration changes");
    let (edited_manifest, edited) = observe(Some(no_op.baseline()));
    let old_member = edited_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Source.fs")
        .expect("formerly declared source remains retained");
    assert!(old_member.language_signals.is_empty());
    assert!(old_member.project_language_witnesses.is_empty());
    let new_member = edited_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Other.fs")
        .expect("newly declared source is retained");
    assert_eq!(
        new_member.language_signals,
        [LanguageSignal {
            language: "fsharp".to_owned(),
            evidence: LanguageEvidence::Project,
        }]
    );
    assert_eq!(
        new_member.project_language_witnesses,
        [ProjectLanguageWitness {
            language: "fsharp".to_owned(),
            project_file: project.file,
            project_path: project.path.clone(),
            project_content_hash: content_hash(edited_document),
        }]
    );

    fs::remove_file(&project_path).expect("project declaration document is removed");
    let (removed_manifest, removed) = observe(Some(edited.baseline()));
    assert_eq!(removed_manifest, initial_manifest);
    for (manifest, discovery) in [
        (&initial_manifest, &initial),
        (&added_manifest, &added),
        (&no_op_manifest, &no_op),
        (&edited_manifest, &edited),
        (&removed_manifest, &removed),
    ] {
        assert!(manifest.coverage.complete);
        assert!(discovery.is_complete());
        assert_eq!(manifest.inputs.len(), discovery.baseline().metadata().len());
        assert!(
            !manifest
                .inputs
                .iter()
                .any(|input| matches!(input.path.as_str(), "ignored/Hidden.fs" | "Missing.fs"))
        );
        for input in manifest
            .inputs
            .iter()
            .filter(|input| input.path.ends_with(".fs"))
        {
            assert_eq!(input.class, InputClass::Source);
            assert_eq!(input.content_hash, content_hash(source));
            assert_eq!(
                input.bytes,
                u64::try_from(source.len()).expect("fixture length fits")
            );
        }
    }
    for discovery in [&added, &edited, &removed] {
        assert!(!discovery.changes().changes().iter().any(|change| {
            change.key() == rootlight_incremental::InputKey::FileContent(member.file)
                || change.key() == rootlight_incremental::InputKey::FileContent(new_member.file)
        }));
    }
}

#[test]
fn implicit_fsharp_content_hints_follow_incremental_source_changes() {
    let temporary = local_tempdir();
    let original = b": twice 2 * ;\n";
    let fsharp = b"open Example.Support\nlet echo (value: int) : int = value\n";
    let plain = b"let value = 1\n";
    let source_path = temporary.path().join("Source.fs");
    fs::write(&source_path, original).expect("Forth-shaped source fixture is written");
    fs::write(temporary.path().join("Plain.fs"), plain)
        .expect("unanchored source fixture is written");
    fs::write(temporary.path().join("ignored.fs"), fsharp)
        .expect("ignored source fixture is written");
    fs::write(temporary.path().join("binary.fs"), [0, 1, 2, 3])
        .expect("binary source fixture is written");
    fs::write(temporary.path().join(".gitignore"), b"ignored.fs\n")
        .expect("ignore fixture is written");
    let root = root(&temporary, b"implicit-fsharp-source-changes");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let observe = |parent: Option<&IncrementalDiscoveryBaseline>| {
        let observed = discover_manifest_and_reconcile_with_progress(
            &root,
            &config,
            ManifestReconcileInputs::new(parent, context),
            &policy,
            limits(),
            &cancellation,
            |_| {},
        )
        .expect("source content changes reconcile");
        let clean = discover(&root, &config, &policy, limits(), &cancellation)
            .expect("independent clean discovery succeeds");
        assert_eq!(observed.0, clean);
        observed
    };
    let (initial_manifest, initial) = observe(None);
    let initial_source = initial_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Source.fs")
        .expect("ambiguous source is retained");
    assert!(initial_source.language_signals.is_empty());
    assert_eq!(initial_source.content_hash, content_hash(original));

    fs::write(&source_path, fsharp).expect("headerless F# declaration replaces source bytes");
    let (changed_manifest, changed) = observe(Some(initial.baseline()));
    let changed_source = changed_manifest
        .inputs
        .iter()
        .find(|input| input.path == "Source.fs")
        .expect("headerless F# source is retained");
    assert_eq!(changed_source.file, initial_source.file);
    assert_eq!(changed_source.content_hash, content_hash(fsharp));
    assert_eq!(
        changed_source.bytes,
        u64::try_from(fsharp.len()).expect("fixture length fits")
    );
    assert_eq!(
        changed_source.language_signals,
        [LanguageSignal {
            language: "fsharp".to_owned(),
            evidence: LanguageEvidence::Content,
        }]
    );
    let (no_op_manifest, no_op) = observe(Some(changed.baseline()));
    assert_eq!(no_op_manifest, changed_manifest);
    assert!(no_op.changes().is_empty());

    fs::write(&source_path, original).expect("original source bytes are restored");
    let (reverted_manifest, reverted) = observe(Some(no_op.baseline()));
    assert_eq!(reverted_manifest, initial_manifest);
    for discovery in [&changed, &reverted] {
        assert!(discovery.changes().changes().iter().any(|change| {
            change.key() == rootlight_incremental::InputKey::FileContent(initial_source.file)
        }));
    }
    for (manifest, discovery) in [
        (&initial_manifest, &initial),
        (&changed_manifest, &changed),
        (&no_op_manifest, &no_op),
        (&reverted_manifest, &reverted),
    ] {
        assert!(manifest.coverage.complete);
        assert!(discovery.is_complete());
        assert_eq!(manifest.inputs.len(), discovery.baseline().metadata().len());
        assert!(
            !manifest
                .inputs
                .iter()
                .any(|input| matches!(input.path.as_str(), "ignored.fs" | "binary.fs"))
        );
        let peer = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Plain.fs")
            .expect("unanchored source remains retained");
        assert_eq!(peer.content_hash, content_hash(plain));
        assert!(peer.language_signals.is_empty());
        for input in manifest
            .inputs
            .iter()
            .filter(|input| input.path.ends_with(".fs"))
        {
            assert_eq!(input.class, InputClass::Source);
            assert!(input.project_language_witnesses.is_empty());
        }
        for path in ["ignored.fs", "binary.fs"] {
            let file = root.file_id(
                &RelativePath::parse(std::path::Path::new(path))
                    .expect("excluded fixture path is valid"),
            );
            assert!(
                !discovery
                    .baseline()
                    .metadata()
                    .files()
                    .any(|baseline| baseline.descriptor().file() == file)
            );
        }
    }
}

#[test]
fn generic_constructor_content_hints_follow_incremental_source_changes() {
    let temporary = local_tempdir();
    let original: &[u8] = b"namespace Example\n";
    let fsharp: &[u8] =
        b"namespace Example\ntype Counter<'T>(value: 'T) =\n    member _.Value = value\n";
    let ambiguous = b": twice 2 * ;\n";
    let source_path = temporary.path().join("Source.fs");
    fs::write(temporary.path().join("Ambiguous.fs"), ambiguous)
        .expect("ambiguous source fixture is written");
    fs::write(temporary.path().join("ignored.fs"), fsharp)
        .expect("ignored source fixture is written");
    fs::write(temporary.path().join("binary.fs"), [0, 1, 2, 3])
        .expect("binary source fixture is written");
    fs::write(temporary.path().join(".gitignore"), b"ignored.fs\n")
        .expect("ignore fixture is written");
    let root = root(&temporary, b"generic-constructor-source-changes");
    let source_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("Source.fs"))
            .expect("source fixture path is valid"),
    );
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let mut parent = None;
    let mut initial_manifest = None;
    let mut typed_manifest = None;
    for (phase, bytes, recognized) in [
        ("initial", original, false),
        ("typed", fsharp, true),
        ("no-op", fsharp, true),
        ("reverted", original, false),
    ] {
        if phase != "no-op" {
            fs::write(&source_path, bytes).expect("source declaration bytes are written");
        }
        let (manifest, discovery) = discover_manifest_and_reconcile_with_progress(
            &root,
            &config,
            ManifestReconcileInputs::new(parent.as_ref(), context),
            &policy,
            limits(),
            &cancellation,
            |_| {},
        )
        .expect("constructor source changes reconcile");
        let clean = discover(&root, &config, &policy, limits(), &cancellation)
            .expect("independent clean discovery succeeds");
        assert_eq!(manifest, clean, "{phase}");
        assert!(manifest.coverage.complete);
        assert!(discovery.is_complete());
        assert_eq!(manifest.inputs.len(), discovery.baseline().metadata().len());
        let source = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Source.fs")
            .expect("source remains retained");
        assert_eq!(source.file, source_file);
        assert_eq!(source.class, InputClass::Source);
        assert_eq!(source.content_hash, content_hash(bytes));
        assert_eq!(
            source.bytes,
            u64::try_from(bytes.len()).expect("fixture length fits")
        );
        assert!(source.project_language_witnesses.is_empty());
        let expected_signals = if recognized {
            vec![LanguageSignal {
                language: "fsharp".to_owned(),
                evidence: LanguageEvidence::Content,
            }]
        } else {
            Vec::new()
        };
        assert_eq!(source.language_signals, expected_signals, "{phase}");
        let peer = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Ambiguous.fs")
            .expect("ambiguous peer remains retained");
        assert_eq!(peer.content_hash, content_hash(ambiguous));
        assert!(peer.language_signals.is_empty());
        for path in ["ignored.fs", "binary.fs"] {
            let file = root.file_id(
                &RelativePath::parse(std::path::Path::new(path))
                    .expect("excluded fixture path is valid"),
            );
            assert!(!manifest.inputs.iter().any(|input| input.file == file));
            assert!(
                !discovery
                    .baseline()
                    .metadata()
                    .files()
                    .any(|baseline| baseline.descriptor().file() == file)
            );
        }
        match phase {
            "initial" => initial_manifest = Some(manifest.clone()),
            "typed" => typed_manifest = Some(manifest.clone()),
            "no-op" => {
                assert_eq!(Some(&manifest), typed_manifest.as_ref());
                assert!(discovery.changes().is_empty());
            }
            "reverted" => assert_eq!(Some(&manifest), initial_manifest.as_ref()),
            _ => unreachable!("fixture phase is known"),
        }
        if matches!(phase, "typed" | "reverted") {
            assert!(discovery.changes().changes().iter().any(|change| {
                change.key() == rootlight_incremental::InputKey::FileContent(source_file)
            }));
        }
        parent = Some(discovery.baseline().clone());
    }
}

#[test]
fn complete_scan_detects_missed_same_size_rewrite_with_clock_regression() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("clock.rs");
    fs::write(&source_path, b"aaaa").expect("fixture source is written");
    let root = root(&temporary, b"incremental-clock");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");
    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    fs::write(&source_path, b"bbbb").expect("same-size rewrite succeeds");
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("rewritten fixture opens");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)))
        .expect("fixture modification clock regresses");

    let update = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("authoritative scan detects rewrite without a watcher event");
    let file_id = root.file_id(
        &RelativePath::parse(std::path::Path::new("clock.rs")).expect("fixture path is valid"),
    );

    assert_eq!(update.hashed_files(), &[file_id]);
    assert_eq!(update.file_changes()[0].kind(), FileChangeKind::Modified);
    assert!(update.changes().changes().iter().any(|change| {
        change.class() == ChangeClass::BodyOnly
            && change.key() == rootlight_incremental::InputKey::FileContent(file_id)
    }));
}

#[test]
fn replacement_rename_and_delete_produce_canonical_file_changes() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("replace.rs"), b"old!")
        .expect("replacement fixture is written");
    fs::write(temporary.path().join("move.rs"), b"same").expect("move fixture is written");
    fs::write(temporary.path().join("delete.rs"), b"gone").expect("delete fixture is written");
    let root = root(&temporary, b"incremental-file-transitions");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");
    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    let replacement = temporary.path().join("replacement.tmp");
    fs::write(&replacement, b"new!").expect("replacement bytes are written");
    fs::remove_file(temporary.path().join("replace.rs")).expect("old file is removed");
    fs::rename(&replacement, temporary.path().join("replace.rs"))
        .expect("replacement enters the repository");
    fs::rename(
        temporary.path().join("move.rs"),
        temporary.path().join("moved.rs"),
    )
    .expect("fixture file is renamed");
    fs::remove_file(temporary.path().join("delete.rs")).expect("fixture file is deleted");

    let update = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("file transitions reconcile");
    let kinds: Vec<_> = update
        .file_changes()
        .iter()
        .map(|change| change.kind())
        .collect();

    assert!(kinds.contains(&FileChangeKind::Modified));
    assert!(kinds.contains(&FileChangeKind::Moved));
    assert!(kinds.contains(&FileChangeKind::Deleted));
    assert!(!update.changes().is_empty());
}

#[test]
fn configuration_and_provider_drift_change_typed_inputs_independently() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-context");
    let policy = policy();
    let first = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    let configured = discover_incremental(
        &root,
        Some(first.baseline()),
        context(b"config-v2", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("configuration drift reconciles");
    #[cfg(unix)]
    assert!(configured.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(configured.hashed_files().len(), 1);
    assert_eq!(configured.changes().changes().len(), 1);
    assert_eq!(
        configured.changes().changes()[0].class(),
        ChangeClass::Configuration
    );

    let provider = discover_incremental(
        &root,
        Some(configured.baseline()),
        context(b"config-v2", b"provider-v2"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("provider drift reconciles");
    #[cfg(unix)]
    assert!(provider.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(provider.hashed_files().len(), 1);
    assert_eq!(provider.changes().changes().len(), 1);
    assert_eq!(
        provider.changes().changes()[0].class(),
        ChangeClass::ProviderChange
    );
}

#[test]
fn cancelled_incremental_scan_stops_before_repository_work() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-cancel");
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));

    assert!(matches!(
        discover_incremental(
            &root,
            None,
            context(b"config-v1", b"provider-v1"),
            &policy(),
            ReconcileMode::Normal,
            limits(),
            &cancellation,
        ),
        Err(rootlight_discovery::DiscoveryError::Cancelled(cancelled))
            if cancelled.reason() == CancellationReason::ClientRequest
    ));
}

#[test]
fn scoped_ignores_exclude_files_and_subtrees_before_incremental_hashing() {
    let temporary = local_tempdir();
    fs::create_dir(temporary.path().join("ignored")).expect("ignored fixture directory is created");
    fs::write(
        temporary.path().join(".gitignore"),
        b"ignored/\nignored.txt\n",
    )
    .expect("ignore fixture is written");
    fs::write(temporary.path().join("included.rs"), b"fn included() {}\n")
        .expect("included fixture is written");
    fs::write(temporary.path().join("ignored.txt"), b"ignored file")
        .expect("ignored file fixture is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(
            temporary.path().join("ignored.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("ignored file is made unreadable");
    }
    fs::write(
        temporary.path().join("ignored").join("nested.rs"),
        b"fn ignored_nested() {}\n",
    )
    .expect("ignored subtree fixture is written");
    let root = root(&temporary, b"incremental-scoped-ignore");

    let discovery = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy(),
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("ignored paths do not enter incremental hashing");
    let ignored_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("ignored.txt"))
            .expect("ignored fixture path is valid"),
    );
    let ignored_nested = root.file_id(
        &RelativePath::parse(std::path::Path::new("ignored/nested.rs"))
            .expect("ignored nested fixture path is valid"),
    );

    assert!(!discovery.hashed_files().contains(&ignored_file));
    assert!(!discovery.hashed_files().contains(&ignored_nested));
    assert_eq!(discovery.baseline().metadata().len(), 2);
}

#[test]
fn vcs_negations_cannot_reopen_default_exclusions() {
    let temporary = local_tempdir();
    fs::create_dir(temporary.path().join(".git")).expect("git fixture directory is created");
    fs::create_dir(temporary.path().join("target")).expect("target fixture directory is created");
    fs::create_dir(temporary.path().join("nested")).expect("nested fixture directory is created");
    fs::create_dir(temporary.path().join("nested").join("target"))
        .expect("nested target fixture directory is created");
    fs::write(
        temporary.path().join(".gitignore"),
        b"!.git/\n!.git/*.rs\n!target/\n!target/*.rs\n",
    )
    .expect("malicious ignore fixture is written");
    fs::write(
        temporary.path().join("nested").join(".gitignore"),
        b"!target/\n!target/*.rs\n",
    )
    .expect("nested malicious ignore fixture is written");
    fs::write(
        temporary.path().join(".git").join("leaked.rs"),
        b"git secret",
    )
    .expect("git fixture is written");
    fs::write(
        temporary.path().join("target").join("leaked.rs"),
        b"build secret",
    )
    .expect("target fixture is written");
    fs::write(
        temporary
            .path()
            .join("nested")
            .join("target")
            .join("leaked.rs"),
        b"nested build secret",
    )
    .expect("nested target fixture is written");
    fs::write(temporary.path().join("visible.rs"), b"fn visible() {}\n")
        .expect("visible fixture is written");
    let root = root(&temporary, b"default-exclusion-negation");
    let policy = policy();
    let cancellation = Cancellation::new();

    let incremental = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental discovery succeeds");
    let manifest = discover(
        &root,
        &ConfigSnapshot::resolve(&[]).expect("default config resolves"),
        &policy,
        limits(),
        &cancellation,
    )
    .expect("clean discovery succeeds");
    let git_file = root.file_id(
        &RelativePath::parse(std::path::Path::new(".git/leaked.rs"))
            .expect("git fixture path is valid"),
    );
    let target_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("target/leaked.rs"))
            .expect("target fixture path is valid"),
    );
    let nested_target_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("nested/target/leaked.rs"))
            .expect("nested target fixture path is valid"),
    );

    assert!(!incremental.hashed_files().contains(&git_file));
    assert!(!incremental.hashed_files().contains(&target_file));
    assert!(!incremental.hashed_files().contains(&nested_target_file));
    assert_eq!(
        manifest
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect::<Vec<_>>(),
        [".gitignore", "nested/.gitignore", "visible.rs"]
    );
}

#[test]
fn entry_limit_bounds_clean_and_incremental_discovery_with_the_same_prefix() {
    let temporary = local_tempdir();
    for name in ["one.rs", "two.rs", "three.rs"] {
        fs::write(temporary.path().join(name), b"fn fixture() {}\n")
            .expect("fixture source is written");
    }
    let root = root(&temporary, b"bounded-directory-enumeration");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let limits = DiscoveryLimits::new(1, 16, 1024 * 1024, 100).expect("test limits are valid");

    let manifest = discover(&root, &config, &policy(), limits, &Cancellation::new())
        .expect("clean discovery publishes a bounded prefix");
    let incremental = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy(),
        ReconcileMode::Normal,
        limits,
        &Cancellation::new(),
    )
    .expect("incremental discovery publishes the same bounded prefix");
    let one = RelativePath::parse(std::path::Path::new("one.rs"))
        .expect("retained fixture path is valid");

    assert!(!manifest.coverage.complete);
    assert!(!incremental.is_complete());
    assert_eq!(
        manifest
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect::<Vec<_>>(),
        ["one.rs"]
    );
    assert_eq!(incremental.hashed_files(), &[root.file_id(&one)]);
}

#[test]
fn clean_manifest_must_match_the_incremental_observation() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-manifest-correlation");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("incremental observation succeeds");
    let manifest = discover(&root, &config, &policy, limits(), &Cancellation::new())
        .expect("clean discovery succeeds");
    let correlated = correlate_incremental_manifest(
        &observed,
        None,
        context,
        &manifest,
        limits(),
        &Cancellation::new(),
    )
    .expect("matching observations correlate");
    assert_eq!(
        correlated.baseline().metadata().len(),
        manifest.inputs.len()
    );

    let mut wrong_repository = manifest.clone();
    wrong_repository.repository = RepositoryId::from_bytes([0x44; 16]);
    assert!(matches!(
        correlate_incremental_manifest(
            &observed,
            None,
            context,
            &wrong_repository,
            limits(),
            &Cancellation::new(),
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));

    let mut drifted = manifest;
    drifted.inputs[0].content_hash = content_hash(b"different bytes");
    assert!(matches!(
        correlate_incremental_manifest(
            &observed,
            None,
            context,
            &drifted,
            limits(),
            &Cancellation::new(),
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));
}

#[test]
fn cached_snapshot_revalidates_metadata_only_changes() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("lib.rs");
    fs::write(&source_path, b"pub fn value() {}\n").expect("fixture source is written");
    let root = root(&temporary, b"incremental-cached-metadata");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let mut observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental observation succeeds");
    let cached_snapshots = observed.take_hashed_snapshots();
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("fixture opens for metadata update");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2)))
        .expect("fixture modification time changes");

    let discovery = discover_with_snapshots(
        &root,
        &config,
        &policy,
        limits(),
        cached_snapshots,
        &cancellation,
    )
    .expect("unchanged content survives metadata revalidation");
    let (manifest, snapshots) = discovery.into_parts();
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn cached_snapshot_revalidation_rejects_same_length_rewrites() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("lib.rs");
    fs::write(&source_path, b"aaaa").expect("fixture source is written");
    let root = root(&temporary, b"incremental-cached-content");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let mut observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental observation succeeds");
    let cached_snapshots = observed.take_hashed_snapshots();
    fs::write(&source_path, b"bbbb").expect("same-length rewrite succeeds");
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("rewritten fixture opens");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(3)))
        .expect("rewrite modification time changes");

    assert!(matches!(
        discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            cached_snapshots,
            &cancellation,
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));
}
