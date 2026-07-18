//! Bounded daemon-independent intent plans for the first secure query slice.
//!
//! Plans pin one immutable generation and compose normalized IR, lexical
//! retrieval, and capability-confined source reads without exposing SQL or an
//! MCP transport contract.

#![forbid(unsafe_code)]

mod model;
mod projection;
mod service;
mod store;

pub use model::{
    CodeLocatePlan, CodeLocateResult, LocateHit, LocateMode, PlanEstimate, PlanExplanation,
    PlanKind, QueryBudget, QueryError, QueryOperator, QueryResource, QueryResponse, QueryUsage,
    RepositoryDataTrust, SourceChunkResult, SourceReadPlan, SourceReadQueryResult,
    SymbolExplainPlan, SymbolExplainResult,
};
pub use projection::project_lexical_documents;
pub use service::QueryService;
pub use store::GenerationSet;
