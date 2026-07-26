//! Source-bound language and workspace fallback evidence generator.
//!
//! Generation writes canonical JSON to standard output. Verification reads the
//! artifact from standard input and independently recomputes every observation.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read as _, Write as _},
    process::ExitCode,
};

use rootlight_bench::{
    LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES, build_language_workspace_evidence,
    encode_language_workspace_evidence, verify_language_workspace_evidence,
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
                eprintln!("error: language and workspace evidence could not be written");
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
                u64::try_from(LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES)
                    .map_err(|_| "evidence byte limit is invalid")?
                    .saturating_add(1),
            )
            .read_to_end(&mut encoded)
            .map_err(|_| "language and workspace evidence could not be read")?;
        verify_language_workspace_evidence(&encoded, &options.source_revision, &options.toolchain)
            .map_err(|_| "language and workspace evidence is invalid")?;
        Ok(None)
    } else {
        let evidence =
            build_language_workspace_evidence(&options.source_revision, &options.toolchain)
                .map_err(|_| "language and workspace evidence could not be built")?;
        encode_language_workspace_evidence(&evidence)
            .map(Some)
            .map_err(|_| "language and workspace evidence could not be encoded")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    source_revision: String,
    toolchain: String,
    verify: bool,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Options, &'static str> {
    let mut arguments = arguments.into_iter();
    let mut source_revision = None;
    let mut toolchain = None;
    let mut verify = false;
    while let Some(flag) = next_argument(&mut arguments)? {
        if flag == OsStr::new("--verify") {
            if verify {
                return Err("language and workspace evidence arguments are invalid");
            }
            verify = true;
            continue;
        }
        let value = next_argument(&mut arguments)?
            .and_then(|value| value.into_string().ok())
            .ok_or("language and workspace evidence arguments are invalid")?;
        let slot = if flag == OsStr::new("--source-revision") {
            &mut source_revision
        } else if flag == OsStr::new("--toolchain") {
            &mut toolchain
        } else {
            return Err("language and workspace evidence arguments are invalid");
        };
        if slot.replace(value).is_some() {
            return Err("language and workspace evidence arguments are invalid");
        }
    }
    Ok(Options {
        source_revision: source_revision
            .ok_or("language and workspace evidence arguments are invalid")?,
        toolchain: toolchain.ok_or("language and workspace evidence arguments are invalid")?,
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
        return Err("language and workspace evidence arguments are invalid");
    }
    Ok(Some(argument))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_accept_generation_and_verification_modes() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_arguments(
                ["--source-revision", revision, "--toolchain", "rustc 1.90.0"]
                    .into_iter()
                    .map(OsString::from)
            )
            .expect("generation arguments should be valid"),
            Options {
                source_revision: revision.to_owned(),
                toolchain: "rustc 1.90.0".to_owned(),
                verify: false,
            }
        );
        assert!(
            parse_arguments(
                [
                    "--verify",
                    "--toolchain",
                    "rustc 1.90.0",
                    "--source-revision",
                    revision,
                ]
                .into_iter()
                .map(OsString::from)
            )
            .expect("verification arguments should be valid")
            .verify
        );
    }

    #[test]
    fn arguments_reject_missing_duplicate_unknown_and_oversized_values() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        assert!(parse_arguments(std::iter::empty()).is_err());
        assert!(
            parse_arguments(
                [
                    "--source-revision",
                    revision,
                    "--source-revision",
                    revision,
                    "--toolchain",
                    "rustc 1.90.0",
                ]
                .into_iter()
                .map(OsString::from)
            )
            .is_err()
        );
        assert!(parse_arguments(["--unknown", revision].into_iter().map(OsString::from)).is_err());
        assert!(
            parse_arguments([
                OsString::from("--source-revision"),
                OsString::from(revision),
                OsString::from("--toolchain"),
                OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1)),
            ])
            .is_err()
        );
    }
}
