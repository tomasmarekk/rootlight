//! Join opt-in component telemetry to actual native admission and source recovery.
//! Existing private calibration supplies a genuine limit; no refusal is injected.
use super::*;

fn assert_components(
    profile: &FirstSliceAnalysisProfile,
) -> crate::projection_profile::Observation {
    let observed = profile
        .last_fresh_projection_admission
        .expect("actual final admission reached");
    assert_eq!(
        observed.scope,
        "computed_conservative_modeled_bytes_not_rss_or_allocation_measurement"
    );
    assert_eq!(observed.model, "3*(D+L)+W+M+A+I");
    assert_eq!(
        observed.stage,
        "fresh_final_projection_admission_not_cold_reload"
    );
    assert!(observed.document_bytes > 0);
    assert!(observed.lexical_coverage_bytes > 0);
    assert!(observed.workspace_bytes > 0);
    assert!(observed.snapshot_metadata_bytes > 0);
    assert_eq!(
        observed.total_bytes,
        observed
            .document_bytes
            .checked_add(observed.lexical_coverage_bytes)
            .unwrap()
            .checked_mul(3)
            .unwrap()
            .checked_add(observed.workspace_bytes)
            .unwrap()
            .checked_add(observed.snapshot_metadata_bytes)
            .unwrap()
            .checked_add(observed.structural_cache_bytes)
            .unwrap()
            .checked_add(observed.explain_index_bytes)
            .unwrap()
    );
    let encoded = serde_json::to_value(profile).unwrap();
    assert_eq!(
        encoded["last_fresh_projection_admission"]["snapshot_metadata_bytes"],
        observed.snapshot_metadata_bytes
    );
    assert!(
        !encoded["last_fresh_projection_admission"]
            .to_string()
            .contains("member_")
    );
    observed
}

#[test]
fn actual_fresh_projection_profile_includes_metadata_and_noop_has_no_new_projection() {
    let (_storage, root, paths) = write_members(&[
        SourceMember {
            path: "one.rs".to_owned(),
            language: "rust",
            bytes: b"pub fn retained_one() {}\n".to_vec(),
            generated: false,
        },
        SourceMember {
            path: "two.rs".to_owned(),
            language: "rust",
            bytes: b"pub fn retained_two() {}\n".to_vec(),
            generated: false,
        },
    ]);
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let (receipt, profile) = capture_first_slice_analysis_profile(|| {
        service
            .index_repository(root.path(), &cancellation)
            .unwrap()
    });
    let observed = assert_components(&profile);
    assert_eq!(
        observed.ceiling_bytes,
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
    );
    assert!(observed.total_bytes <= observed.ceiling_bytes);
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    for name in ["retained_one", "retained_two"] {
        assert!(
            lease
                .generation()
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    drop(lease);
    let (noop, empty) = capture_first_slice_analysis_profile(|| {
        service
            .index_repository(root.path(), &cancellation)
            .unwrap()
    });
    assert_eq!(noop.generation, receipt.generation);
    assert!(empty.last_fresh_projection_admission.is_none());
    assert_no_staging_or_reservations(&service, &paths);
}

#[test]
fn genuine_fresh_projection_refusal_keeps_component_profile_and_exact_sources() {
    let measured = measured();
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    with_ceiling(measured.ceiling, || {
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let ((receipt, (_, peak)), profile) = capture_first_slice_analysis_profile(|| {
            capture(|| {
                service
                    .index_repository(root.path(), &cancellation)
                    .unwrap()
            })
        });
        let observed = assert_components(&profile);
        assert_eq!(observed.total_bytes, peak);
        assert_eq!(observed.ceiling_bytes, measured.ceiling);
        assert!(observed.total_bytes > observed.ceiling_bytes);
        let lease = service
            .generation_lease(receipt.generation, &cancellation)
            .unwrap();
        let document = lease.generation().document();
        assert!(
            document.entities.is_empty()
                && document.occurrences.is_empty()
                && document.relations.is_empty()
        );
        assert!(document.diagnostics.iter().any(|diagnostic| diagnostic.message.contains(&format!(
            "fresh projection capacity (full projection peak) requires {} modeled bytes above limit {}",
            observed.total_bytes, observed.ceiling_bytes))));
        drop(lease);
        assert_sources(
            &service,
            &receipt,
            true,
            false,
            "profile-after-genuine-refusal",
        );
        assert_no_staging_or_reservations(&service, &paths);
    });
}

// A real fresh admission joins the two independently collected numeric views.
// The no-op must not reuse a previous projection's observation.

#[test]
fn fresh_record_sizes_join_admission_bytes_and_noop_stays_unknown() {
    let (_storage, root, paths) = write_members(&[SourceMember {
        path: "sized.rs".to_owned(),
        language: "rust",
        bytes: b"pub fn sized_member() {}\n".to_vec(),
        generated: false,
    }]);
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let (receipt, profile) = capture_first_slice_analysis_profile(|| {
        service
            .index_repository(root.path(), &cancellation)
            .unwrap()
    });
    let model = assert_components(&profile);
    let encoded = serde_json::to_value(&profile).unwrap();
    let observed = &encoded["last_fresh_projection_records"];
    assert_eq!(
        observed["document_bytes"].as_u64(),
        Some(model.document_bytes)
    );
    assert_eq!(
        observed["stage"],
        "fresh_final_projection_serialization_before_admission"
    );
    assert_eq!(
        observed["scope"],
        "complete_serialized_logical_json_bytes_not_rss_heap_or_temporary_lifetimes"
    );
    assert_eq!(
        observed["serialized_record_bytes"].as_u64().unwrap()
            + observed["envelope_bytes"].as_u64().unwrap(),
        model.document_bytes
    );
    assert!(observed["records"].as_u64().unwrap() > 0);
    let categories = observed["categories"].as_array().unwrap();
    assert_eq!(
        categories
            .iter()
            .map(|value| value["category"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "files",
            "entities",
            "occurrences",
            "relations",
            "provenance",
            "source_mappings",
            "coverage_records",
            "skipped_regions",
            "diagnostics",
            "extensions"
        ]
    );
    assert!(categories[0]["records"].as_u64().unwrap() > 0);
    assert!(categories[1]["records"].as_u64().unwrap() > 0);
    assert!(!observed.to_string().contains("sized_member"));
    assert!(!observed.to_string().contains("sized.rs"));
    let lease = service
        .generation_lease(receipt.generation, &cancellation)
        .unwrap();
    assert!(
        lease
            .generation()
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "sized_member")
    );
    drop(lease);
    let (noop, profile) = capture_first_slice_analysis_profile(|| {
        service
            .index_repository(root.path(), &cancellation)
            .unwrap()
    });
    assert_eq!(noop.generation, receipt.generation);
    assert!(profile.last_fresh_projection_admission.is_none());
    assert!(profile.last_fresh_projection_records.is_none());
    assert_no_staging_or_reservations(&service, &paths);
}
