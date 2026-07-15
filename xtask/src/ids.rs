//! Emits canonical stable-ID vectors for independent process comparisons.

use rootlight_ids::{
    FileIdentity, GenerationIdentity, SymbolIdentity, content_hash, derive_fact, derive_file,
    derive_generation, derive_repository, derive_symbol,
};
use serde::Serialize;

pub(crate) fn print_vectors() -> Result<(), IdVectorError> {
    let repository = derive_repository(b"01914f58-0bd1-7f65-b52c-73aebd98c4a1");
    let manifest_hash = content_hash(b"canonical manifest fixture");
    let config_hash = content_hash(b"canonical config fixture");
    let provider_set_hash = content_hash(b"canonical provider fixture");
    let generation = derive_generation(GenerationIdentity {
        repository: repository.id(),
        parent: None,
        manifest_hash,
        config_hash,
        provider_set_hash,
        format_version: 1,
    });
    let symbol = derive_symbol(SymbolIdentity {
        repository: repository.id(),
        language: "rust",
        semantic_kind: "function",
        container_identity: b"rootlight_ids",
        declared_identity: "derive_repository",
        signature_discriminator: b"fn(&[u8])",
        build_context_discriminator: b"default",
    });
    let file = derive_file(FileIdentity {
        repository: repository.id(),
        path_identity: b"crates/rootlight-ids/src/lib.rs",
    });
    let fact = derive_fact("declares", b"rootlight_ids::derive_repository");

    let vectors = Vectors {
        schema_version: "1.0",
        repository_id: repository.id().to_string(),
        repository_digest: hex(repository.digest().as_bytes()),
        generation_id: generation.id().to_string(),
        generation_digest: hex(generation.digest().as_bytes()),
        symbol_id: symbol.id().to_string(),
        symbol_digest: hex(symbol.digest().as_bytes()),
        file_id: file.id().to_string(),
        file_digest: hex(file.digest().as_bytes()),
        fact_id: fact.id().to_string(),
        fact_digest: hex(fact.digest().as_bytes()),
        content_hash: manifest_hash.to_string(),
    };
    let output = serde_json::to_string_pretty(&vectors).map_err(IdVectorError::Serialize)?;
    println!("{output}");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[derive(Debug, Serialize)]
struct Vectors {
    schema_version: &'static str,
    repository_id: String,
    repository_digest: String,
    generation_id: String,
    generation_digest: String,
    symbol_id: String,
    symbol_digest: String,
    file_id: String,
    file_digest: String,
    fact_id: String,
    fact_digest: String,
    content_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IdVectorError {
    #[error("failed to serialize stable ID vectors")]
    Serialize(#[source] serde_json::Error),
}
