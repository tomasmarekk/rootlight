//! Repository tooling for Rootlight's architecture and evidence contracts.
//!
//! `cargo xtask` keeps checks in Rust so the same behavior runs on every
//! supported developer and CI platform.

#![forbid(unsafe_code)]

mod architecture;
mod ids;
mod policy;
mod protobuf_compatibility;
mod schemas;

use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
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
        Some("compatibility-check") | Some("compatibility") => schemas::check_compatibility()?,
        Some("policy-check") | Some("policy") => policy::check()?,
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

#[derive(Debug, thiserror::Error)]
enum XtaskError {
    #[error(
        "usage: cargo xtask <architecture-check|compatibility-check|id-vectors|generate [--check]|policy-check|unsafe-check --fixture-root PATH>"
    )]
    MissingCommand,
    #[error("unknown xtask command: {0}")]
    UnknownCommand(String),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("--fixture-root requires a path")]
    MissingFixtureRoot,
    #[error(transparent)]
    Architecture(#[from] architecture::ArchitectureError),
    #[error(transparent)]
    IdVectors(#[from] ids::IdVectorError),
    #[error(transparent)]
    Policy(#[from] policy::PolicyError),
    #[error(transparent)]
    Schemas(#[from] schemas::SchemaError),
}
