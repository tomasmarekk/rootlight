//! Exact-run durable workspace scale measurement generator.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read as _, Write as _},
    process::ExitCode,
};

use rootlight_bench::{
    WORKSPACE_SCALE_EVIDENCE_MAX_BYTES, build_workspace_scale_evidence,
    encode_workspace_scale_evidence, verify_workspace_scale_evidence,
};

const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(Some(encoded)) => {
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&encoded)
                .and_then(|()| stdout.write_all(b"\n"))
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                eprintln!("error: workspace scale evidence could not be written");
                ExitCode::FAILURE
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Option<Vec<u8>>, &'static str> {
    let options = parse_arguments(std::env::args_os().skip(1))?;
    if options.verify {
        let mut encoded = Vec::new();
        io::stdin()
            .lock()
            .take(
                u64::try_from(WORKSPACE_SCALE_EVIDENCE_MAX_BYTES)
                    .map_err(|_| "workspace scale evidence byte limit is invalid")?
                    .saturating_add(1),
            )
            .read_to_end(&mut encoded)
            .map_err(|_| "workspace scale evidence could not be read")?;
        verify_workspace_scale_evidence(
            &encoded,
            options.repositories,
            &options.source_revision,
            &options.toolchain,
        )
        .map_err(|_| "workspace scale evidence is invalid")?;
        return Ok(None);
    }
    let evidence = build_workspace_scale_evidence(
        options.repositories,
        &options.source_revision,
        &options.toolchain,
    )
    .map_err(|_| "workspace scale evidence could not be built")?;
    let encoded = encode_workspace_scale_evidence(&evidence)
        .map_err(|_| "workspace scale evidence could not be encoded")?;
    verify_workspace_scale_evidence(
        &encoded,
        options.repositories,
        &options.source_revision,
        &options.toolchain,
    )
    .map_err(|_| "workspace scale evidence could not be verified")?;
    Ok(Some(encoded))
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    repositories: usize,
    source_revision: String,
    toolchain: String,
    verify: bool,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Options, &'static str> {
    let mut arguments = arguments.into_iter();
    let mut repositories = None;
    let mut source_revision = None;
    let mut toolchain = None;
    let mut verify = false;
    while let Some(flag) = next_argument(&mut arguments)? {
        if flag == OsStr::new("--verify") {
            if verify {
                return Err("workspace scale evidence arguments are invalid");
            }
            verify = true;
            continue;
        }
        let value = next_argument(&mut arguments)?
            .and_then(|value| value.into_string().ok())
            .ok_or("workspace scale evidence arguments are invalid")?;
        if flag == OsStr::new("--repositories") {
            let value = value
                .parse()
                .map_err(|_| "workspace scale evidence arguments are invalid")?;
            if repositories.replace(value).is_some() {
                return Err("workspace scale evidence arguments are invalid");
            }
        } else {
            let slot = if flag == OsStr::new("--source-revision") {
                &mut source_revision
            } else if flag == OsStr::new("--toolchain") {
                &mut toolchain
            } else {
                return Err("workspace scale evidence arguments are invalid");
            };
            if slot.replace(value).is_some() {
                return Err("workspace scale evidence arguments are invalid");
            }
        }
    }
    Ok(Options {
        repositories: repositories.ok_or("workspace scale evidence arguments are invalid")?,
        source_revision: source_revision.ok_or("workspace scale evidence arguments are invalid")?,
        toolchain: toolchain.ok_or("workspace scale evidence arguments are invalid")?,
        verify,
    })
}

fn next_argument<I>(arguments: &mut I) -> Result<Option<OsString>, &'static str>
where
    I: Iterator<Item = OsString>,
{
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
        return Err("workspace scale evidence arguments are invalid");
    }
    Ok(Some(argument))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_one_bounded_measurement_identity() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_arguments(
                [
                    "--repositories",
                    "100",
                    "--source-revision",
                    revision,
                    "--toolchain",
                    "rustc 1.90.0",
                ]
                .into_iter()
                .map(OsString::from),
            )
            .expect("complete arguments are valid"),
            Options {
                repositories: 100,
                source_revision: revision.to_owned(),
                toolchain: "rustc 1.90.0".to_owned(),
                verify: false,
            }
        );
        let verification = parse_arguments(
            [
                "--verify",
                "--repositories",
                "100",
                "--source-revision",
                revision,
                "--toolchain",
                "rustc 1.90.0",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .expect("verification arguments are valid");
        assert!(verification.verify);
        assert!(parse_arguments(std::iter::empty()).is_err());
        assert!(
            parse_arguments(
                ["--repositories", "invalid"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
    }
}
