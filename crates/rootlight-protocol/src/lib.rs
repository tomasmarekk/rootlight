//! Versioned protobuf wire contracts for local Rootlight process boundaries.
//!
//! Generated messages are checked in during TASK-01.4 so ordinary builds never
//! require network access or a protobuf compiler.

#![forbid(unsafe_code)]

/// Generated messages compiled from the checked protocol sources.
pub mod generated;

/// Canonical descriptor set for compatibility tooling and reflection-free checks.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!("../../../schemas/generated/protobuf/rootlight.desc");

/// Earliest daemon protocol accepted by the current client and server.
///
/// Protocol 1.0 remains a frozen wire-compatibility baseline, but it predates
/// authenticated operation submission and cannot satisfy the current contract.
pub const MINIMUM_PROTOCOL_MINOR: u32 = 1;
/// Latest daemon protocol implemented by the current client and server.
pub const CURRENT_PROTOCOL_MINOR: u32 = 1;
/// Current production protocol contract version.
pub const PROTOCOL_VERSION: &str = "1.1";

#[cfg(test)]
mod tests {
    use prost::Message;
    use prost_types::FileDescriptorSet;

    use super::*;

    #[test]
    fn descriptor_contains_only_versioned_protocol_packages() {
        let descriptor = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
            .expect("checked descriptor set is valid protobuf");
        let packages: Vec<_> = descriptor
            .file
            .iter()
            .filter_map(|file| file.package.as_deref())
            .collect();

        assert_eq!(
            packages,
            [
                "rootlight.common.v1",
                "rootlight.daemon.v1",
                "rootlight.adapter.v1",
            ]
        );
        assert!(descriptor.file.iter().all(|file| file.service.is_empty()));
    }

    #[test]
    fn generated_messages_round_trip_unknown_additive_fields() {
        let mut encoded = generated::common::v1::ContractVersion {
            major: 1,
            minor: CURRENT_PROTOCOL_MINOR,
        }
        .encode_to_vec();
        encoded.extend_from_slice(&[0x98, 0x06, 0x07]);

        let decoded = generated::common::v1::ContractVersion::decode(encoded.as_slice())
            .expect("unknown protobuf field is skipped");
        assert_eq!(decoded.major, 1);
        assert_eq!(decoded.minor, CURRENT_PROTOCOL_MINOR);
    }
}
