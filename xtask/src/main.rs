//! Repository tooling for Rootlight's architecture and evidence contracts.
//!
//! `cargo xtask` keeps checks in Rust so the same behavior runs on every
//! supported developer and CI platform.

#![forbid(unsafe_code)]

mod architecture;
mod daemon_lifecycle;
mod git_metadata;
mod grammar_lock;
mod ids;
mod license;
mod mcp_vertical;
mod policy;
mod protobuf_compatibility;
mod schemas;
mod source_hygiene;

use std::{env, error::Error as _, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            if let XtaskError::DaemonLifecycle(lifecycle) = &error {
                let mut source = lifecycle.source();
                while let Some(cause) = source {
                    eprintln!("caused by: {cause}");
                    source = cause.source();
                }
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
        Some("id-vectors") => ids::print_vectors()?,
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
        Some("mcp-vertical-check") => {
            let options = mcp_vertical::Options::parse(&mut args)?;
            mcp_vertical::check(&options)?;
        }
        Some("policy-check") | Some("policy") => policy::check()?,
        Some("license-check") => license::check()?,
        Some("internal-id-check") => git_metadata_command(&mut args)?,
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

fn git_metadata_command(args: &mut impl Iterator<Item = String>) -> Result<(), XtaskError> {
    let flag = args.next().ok_or(XtaskError::MissingInternalIdMode)?;
    let value = args.next().ok_or(XtaskError::MissingInternalIdValue)?;
    match flag.as_str() {
        "--commit-msg-file" => {
            git_metadata::check_commit_msg_file(std::path::Path::new(&value))?;
        }
        "--range" => {
            let workspace_root = std::env::current_dir().map_err(XtaskError::WorkingDir)?;
            git_metadata::check_range(&workspace_root, &value)?;
        }
        "--event" => {
            git_metadata::check_event(std::path::Path::new(&value))?;
        }
        other => return Err(XtaskError::UnexpectedArgument(other.to_owned())),
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum XtaskError {
    #[error(
        "usage: cargo xtask <architecture-check|compatibility-check|daemon-lifecycle-check --bin-dir PATH|mcp-vertical-check --bin-dir PATH [--output-dir PATH]|freeze-daemon-protocol|id-vectors|generate [--check]|internal-id-check <--commit-msg-file PATH|--range REV|--event PATH>|license-check|policy-check|unsafe-check --fixture-root PATH>"
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
    #[error("internal-id-check requires --commit-msg-file, --range, or --event")]
    MissingInternalIdMode,
    #[error("internal-id-check flag requires a value")]
    MissingInternalIdValue,
    #[error("failed to determine the working directory")]
    WorkingDir(#[source] std::io::Error),
    #[error(transparent)]
    Architecture(#[from] architecture::ArchitectureError),
    #[error(transparent)]
    DaemonLifecycle(#[from] daemon_lifecycle::LifecycleError),
    #[error(transparent)]
    GitMetadata(#[from] git_metadata::GitMetadataError),
    #[error(transparent)]
    IdVectors(#[from] ids::IdVectorError),
    #[error(transparent)]
    License(#[from] license::LicenseError),
    #[error(transparent)]
    McpVertical(#[from] mcp_vertical::VerticalError),
    #[error(transparent)]
    Policy(#[from] policy::PolicyError),
    #[error(transparent)]
    Schemas(#[from] schemas::SchemaError),
}
