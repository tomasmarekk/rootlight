//! Exact-source in-process Tier B semantic holdout generator.
//!
//! The versioned five-language corpus runs through the shipping Tree-sitter
//! provider and whole-project analyzer without executing repository code.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write as _},
    process::ExitCode,
};

use rootlight_bench::{build_project_semantic_holdout, encode_project_semantic_holdout_envelope};

const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(encoded) => {
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&encoded)
                .and_then(|()| stdout.write_all(b"\n"))
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                eprintln!("error: semantic evidence could not be written");
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<Vec<u8>, &'static str> {
    let source_revision = parse_arguments(arguments)?;
    let evidence =
        build_project_semantic_holdout().map_err(|_| "production semantic holdout is invalid")?;
    encode_project_semantic_holdout_envelope(&evidence, &source_revision)
        .map_err(|_| "semantic evidence envelope is invalid")
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<String, &'static str> {
    let mut arguments = arguments.into_iter();
    let flag = next_argument(&mut arguments)?.ok_or("semantic evidence arguments are invalid")?;
    if flag != OsStr::new("--source-revision") {
        return Err("semantic evidence arguments are invalid");
    }
    let source_revision = next_argument(&mut arguments)?
        .and_then(|value| value.into_string().ok())
        .ok_or("semantic evidence arguments are invalid")?;
    if next_argument(&mut arguments)?.is_some() {
        return Err("semantic evidence arguments are invalid");
    }
    Ok(source_revision)
}

fn next_argument<I>(arguments: &mut I) -> Result<Option<OsString>, &'static str>
where
    I: Iterator<Item = OsString>,
{
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
        return Err("semantic evidence arguments are invalid");
    }
    Ok(Some(argument))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn arguments_reject_missing_extra_and_oversized_values() {
        assert_eq!(
            parse_arguments(
                ["--source-revision", REVISION]
                    .into_iter()
                    .map(OsString::from)
            )
            .expect("canonical arguments are accepted"),
            REVISION
        );
        assert!(parse_arguments(std::iter::empty()).is_err());
        assert!(
            parse_arguments(
                ["--source-revision", REVISION, "--extra"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
        assert!(
            parse_arguments([
                OsString::from("--source-revision"),
                OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1)),
            ])
            .is_err()
        );
    }

    #[test]
    fn production_holdout_is_source_bound_and_deterministic() {
        let first = run(["--source-revision", REVISION]
            .into_iter()
            .map(OsString::from))
        .expect("production holdout encodes");
        let repeated = run(["--source-revision", REVISION]
            .into_iter()
            .map(OsString::from))
        .expect("production holdout repeats");
        assert_eq!(first, repeated);

        let value: serde_json::Value =
            serde_json::from_slice(&first).expect("semantic envelope decodes");
        assert_eq!(
            value["schema"],
            "rootlight.project-semantic-holdout-envelope/1"
        );
        assert_eq!(value["source_revision"], REVISION);
        assert_eq!(
            value["evidence"]["schema"],
            "rootlight.project-semantic-holdout/2"
        );
        assert_eq!(
            value["evidence"]["languages"].as_array().map(Vec::len),
            Some(5)
        );
    }
}
