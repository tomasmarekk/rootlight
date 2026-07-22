//! Transport-neutral agent-domain orchestration for Rootlight.
//!
//! This crate owns the planning and shaping behavior assigned to the agent
//! boundary: context-pack optimization, batch orchestration, advanced-query
//! normalization, response policy, and response shaping.
//! Application binaries compose these services and stay thin: protocol framing,
//! schema validation, exposure-profile authorization, and IPC composition.
//!
//! To keep the boundary honest, this crate must not depend on application
//! crates, the stdio transport, or JSON-RPC server internals, and its types
//! carry no request IDs or transport lifecycle.

#![forbid(unsafe_code)]

pub mod advanced;
pub mod batch;
pub mod change;
pub mod claim_safety;
pub mod context_evidence;
pub mod context_pack;
pub mod context_pack_request;
pub mod explain;
pub mod policy;
pub mod port;
pub mod response_profile;
