INSERT INTO generation_meta (
    singleton, contract_major, contract_minor, ir_major, ir_minor,
    repository_id, generation_id, parent_generation_id,
    manifest_hash, configuration_hash, provider_set_hash,
    file_count, entity_count, occurrence_count, relation_count,
    provenance_count, source_mapping_count, coverage_count,
    skipped_region_count, diagnostic_count, extension_count,
    source_ref_count, stored_row_count, text_bytes, sealed
) VALUES (
    1, 1, 1, 1, 1,
    X'D9CECF1CEADC8E76209537A20DE12B3B',
    X'9BB264BC4C2EE16F223DCDB22E2570847BD70E40',
    NULL,
    X'0D197521D9A48760576DA1C87DFD828A60D397EA1E16F92A5B7C44B4F4060CE1',
    X'6F9B6927228AD98444162E1A202914BF3EA3A88328C16D1B116F95051444187F',
    X'52293A361D6471BB4775CE4028F9C2E3E53C47950E5A3ED706960AB5F253A2B8',
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1
);

INSERT INTO identity_registry(kind, identity) VALUES (
    'repository',
    X'D9CECF1CEADC8E76209537A20DE12B3B'
);

INSERT INTO application_meta(key, value) VALUES (
    'document_hash',
    X'1F979F62F0CC529E0E48A41A817335B09345D058037768412FAE6291C03E0EB3'
);
