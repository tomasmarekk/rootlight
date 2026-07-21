//! Transport-neutral agent-domain orchestration for Rootlight.
//!
//! This crate owns the planning and shaping behavior that document 07 assigns
//! to the agent boundary: context-pack optimization, and (in later slices)
//! batch orchestration, advanced-query normalization, and response shaping.
//! Application binaries compose these services and stay thin: protocol framing,
//! schema validation, exposure-profile authorization, and IPC composition.
//!
//! To keep the boundary honest, this crate must not depend on application
//! crates, the stdio transport, or JSON-RPC server internals, and its types
//! carry no request IDs or transport lifecycle.

pub mod context_pack;
