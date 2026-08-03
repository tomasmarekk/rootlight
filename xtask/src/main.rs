//! Repository tooling for Rootlight's architecture and validation contracts.
//!
//! `cargo xtask` keeps checks in Rust so the same behavior runs on every
//! supported developer and CI platform.

#![forbid(unsafe_code)]

mod architecture;
mod budget_conformance;
mod capability;
mod contract_matrix;
mod daemon_lifecycle;
mod datasets;
mod grammar_lock;
mod ids;
mod incident;
mod license;
mod mcp_compatibility;
mod mcp_vertical;
mod package;
mod policy;
mod protobuf_compatibility;
mod release;
mod response_profile_evidence;
mod schemas;
mod token_accounting;
mod tool_discovery;
mod update_release;

use std::{env, error::Error as _, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("architecture-check") | Some("architecture") => {
            let fixture_root = parse_fixture_root(&mut args)?;
            architecture::check(fixture_root.as_deref())?;
        }
        Some("budget-conformance-check") => {
            let options = budget_conformance::Options::parse(&mut args)?;
            budget_conformance::check(&options)?;
        }
        Some("id-vectors") => ids::print_vectors()?,
        Some("incident-tabletop") => {
            let options = incident::Options::parse(&mut args)?;
            incident::exercise(&options)?;
        }
        Some("generate") | Some("schemas") => {
            let mode = parse_generate_mode(&mut args)?;
            schemas::generate(mode)?;
        }
        Some("freeze-daemon-protocol") => schemas::freeze_daemon_protocol()?,
        Some("compatibility-check") | Some("compatibility") => schemas::check_compatibility()?,
        Some("daemon-lifecycle-check") => {
            let bin_dir = parse_required_bin_dir(&mut args)?;
            daemon_lifecycle::check(&bin_dir)?;
        }
        Some("dataset-check") => datasets::check()?,
        Some("dataset-cache") => {
            let options = datasets::CacheOptions::parse(&mut args)?;
            datasets::acquire(&options)?;
        }
        Some("mcp-vertical-check") => {
            let options = mcp_vertical::Options::parse(&mut args)?;
            mcp_vertical::check(&options)?;
        }
        Some("mcp-compatibility-check") => {
            let options = mcp_compatibility::Options::parse(&mut args)?;
            mcp_compatibility::check(&options)?;
        }
        Some("package-check") => package::check()?,
        Some("package-build") => {
            let options = package::BuildOptions::parse(&mut args)?;
            package::build(&options)?;
        }
        Some("package-smoke") => {
            let options = package::SmokeOptions::parse(&mut args)?;
            package::smoke(&options)?;
        }
        Some("package-verify") => {
            let options = package::VerifyOptions::parse(&mut args)?;
            package::verify(&options)?;
        }
        Some("release-plan") => {
            let options = release::Options::parse(&mut args)?;
            release::build(&options)?;
        }
        Some("update-release-metadata") => {
            let options = update_release::Options::parse(&mut args)?;
            update_release::build(&options)?;
        }
        Some("response-profile-check") => {
            let options = response_profile_evidence::Options::parse(&mut args)?;
            response_profile_evidence::check(&options)?;
        }
        Some("policy-check") | Some("policy") => policy::check()?,
        Some("license-check") => license::check()?,
        Some("capability-check") => {
            let options = capability::Options::parse(&mut args)?;
            capability::check(&options)?;
        }
        Some("contract-matrix") => {
            let options = contract_matrix::Options::parse(&mut args)?;
            contract_matrix::run(&options)?;
        }
        Some("tool-discovery-evidence") => {
            let options = tool_discovery::Options::parse(&mut args)?;
            tool_discovery::emit(&options)?;
        }
        Some("token-accounting-report") => {
            let options = token_accounting::Options::parse(&mut args)?;
            token_accounting::emit(&options)?;
        }
        Some("token-accounting-check") => {
            let report = token_accounting::parse_report_path(&mut args)?;
            token_accounting::check(&report)?;
        }
        Some("unsafe-check") => {
            let fixture_root = parse_required_fixture_root(&mut args)?;
            policy::check_unsafe_fixture(&fixture_root)?;
        }
        Some(command) => return Err(XtaskError::UnknownCommand(command.to_owned())),
        None => return Err(XtaskError::MissingCommand),
    }

    if let Some(unexpected) = args.next() {
        return Err(XtaskError::UnexpectedArgument(unexpected));
    }

    Ok(())
}

fn parse_generate_mode(
    args: &mut impl Iterator<Item = String>,
) -> Result<schemas::GenerateMode, XtaskError> {
    match args.next() {
        None => Ok(schemas::GenerateMode::Update),
        Some(flag) if flag == "--check" => Ok(schemas::GenerateMode::Check),
        Some(argument) => Err(XtaskError::UnexpectedArgument(argument)),
    }
}

fn parse_fixture_root(
    args: &mut impl Iterator<Item = String>,
) -> Result<Option<std::path::PathBuf>, XtaskError> {
    match args.next() {
        None => Ok(None),
        Some(flag) if flag == "--fixture-root" => args
            .next()
            .map(std::path::PathBuf::from)
            .map(Some)
            .ok_or(XtaskError::MissingFixtureRoot),
        Some(argument) => Err(XtaskError::UnexpectedArgument(argument)),
    }
}

fn parse_required_fixture_root(
    args: &mut impl Iterator<Item = String>,
) -> Result<std::path::PathBuf, XtaskError> {
    parse_fixture_root(args)?.ok_or(XtaskError::MissingFixtureRoot)
}

fn parse_required_bin_dir(
    args: &mut impl Iterator<Item = String>,
) -> Result<std::path::PathBuf, XtaskError> {
    match (args.next(), args.next()) {
        (Some(flag), Some(path)) if flag == "--bin-dir" => Ok(std::path::PathBuf::from(path)),
        (Some(argument), _) => Err(XtaskError::UnexpectedArgument(argument)),
        (None, _) => Err(XtaskError::MissingBinDir),
    }
}

#[derive(Debug, thiserror::Error)]
enum XtaskError {
    #[error(
        "usage: cargo xtask <architecture-check|budget-conformance-check [--fixture-root PATH] [--refresh] [--runtime-report PATH --cancellation-report PATH --output PATH]|capability-check [--output-dir PATH --source-revision REV]|compatibility-check|contract-matrix <--output PATH|--verify PATH> --source-revision REV|daemon-lifecycle-check --bin-dir PATH|dataset-check|dataset-cache --cache-dir PATH --output PATH --source-revision REV|incident-tabletop --output PATH --source-revision REV|mcp-compatibility-check [--fixture-root PATH] [--refresh-current]|mcp-vertical-check --bin-dir PATH [--output-dir PATH>|package-check|package-build --target TARGET --version VERSION --source-revision REV --bin-dir PATH --web-assets-dir PATH --web-notices PATH --output-dir PATH|package-smoke --baseline-archive PATH --archive PATH --source-revision REV --output PATH|package-verify --archive PATH|release-plan --channel <alpha|final> --tags PATH [--exact-version VERSION] --output PATH|update-release-metadata --archive PATH --sbom PATH --provenance PATH --license-bundle PATH --target TARGET --version VERSION --key-id ID [--private-seed PATH --public-key-hex HEX] --valid-from UNIX --expires UNIX --rollout-percentage PERCENT --catalog-schema VERSION --protocol-major VERSION --protocol-minor VERSION --output-dir PATH|response-profile-check [--fixture-root PATH] [--refresh]|freeze-daemon-protocol|id-vectors|generate [--check]|license-check|policy-check|token-accounting-report --output-dir PATH --source-revision REV|token-accounting-check --report PATH|tool-discovery-evidence --output-dir PATH --source-revision REV|unsafe-check --fixture-root PATH>"
    )]
    MissingCommand,
    #[error("unknown xtask command: {0}")]
    UnknownCommand(String),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("--fixture-root requires a path")]
    MissingFixtureRoot,
    #[error("--bin-dir requires a path")]
    MissingBinDir,
    #[error(transparent)]
    Architecture(#[from] architecture::ArchitectureError),
    #[error(transparent)]
    BudgetConformance(#[from] budget_conformance::BudgetConformanceError),
    #[error(transparent)]
    Capability(#[from] capability::CapabilityError),
    #[error(transparent)]
    ContractMatrix(#[from] contract_matrix::ContractMatrixError),
    #[error(transparent)]
    DaemonLifecycle(#[from] daemon_lifecycle::LifecycleError),
    #[error(transparent)]
    Datasets(#[from] datasets::DatasetError),
    #[error(transparent)]
    IdVectors(#[from] ids::IdVectorError),
    #[error(transparent)]
    Incident(#[from] incident::IncidentError),
    #[error(transparent)]
    License(#[from] license::LicenseError),
    #[error(transparent)]
    McpCompatibility(#[from] mcp_compatibility::CompatibilityError),
    #[error(transparent)]
    McpVertical(#[from] mcp_vertical::VerticalError),
    #[error(transparent)]
    Package(#[from] package::PackageError),
    #[error(transparent)]
    ReleaseUpdate(#[from] update_release::ReleaseUpdateError),
    #[error(transparent)]
    ReleasePlan(#[from] release::ReleasePlanError),
    #[error(transparent)]
    Policy(#[from] policy::PolicyError),
    #[error(transparent)]
    ResponseProfile(#[from] response_profile_evidence::ResponseProfileEvidenceError),
    #[error(transparent)]
    ToolDiscovery(#[from] tool_discovery::DiscoveryError),
    #[error(transparent)]
    TokenAccounting(#[from] token_accounting::TokenAccountingError),
    #[error(transparent)]
    Schemas(#[from] schemas::SchemaError),
}
