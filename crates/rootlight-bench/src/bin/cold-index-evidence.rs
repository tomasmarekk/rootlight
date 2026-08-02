//! Independent verifier for release-candidate cold-index evidence.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
    process::ExitCode,
};

use rootlight_bench::{
    ColdIndexEvidence, cold_index_corpus_sha256, decode_cold_index_evidence,
    load_cold_index_corpus, verify_cold_index_evidence, verify_cold_index_evidence_set,
};

const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), &'static str> {
    let options = parse_arguments(std::env::args_os().skip(1))?;
    let corpus_bytes =
        fs::read(&options.corpus).map_err(|_| "cold-index corpus could not be read")?;
    let corpus =
        load_cold_index_corpus(&options.corpus).map_err(|_| "cold-index corpus is invalid")?;
    let corpus_sha256 = cold_index_corpus_sha256(&corpus_bytes);
    match options.mode {
        Mode::Verify(evidence) => {
            let encoded =
                fs::read(evidence).map_err(|_| "cold-index evidence could not be read")?;
            let evidence = decode_cold_index_evidence(&encoded)
                .map_err(|_| "cold-index evidence is invalid")?;
            verify_cold_index_evidence(
                &corpus,
                &corpus_sha256,
                &evidence,
                &options.source_revision,
                &options.candidate_sha256,
            )
            .map_err(|_| "cold-index evidence did not satisfy release policy")
        }
        Mode::VerifySet(directory) => {
            let evidence = read_evidence_set(&directory)?;
            verify_cold_index_evidence_set(
                &corpus,
                &corpus_sha256,
                &evidence,
                &options.source_revision,
                &options.candidate_sha256,
            )
            .map_err(|_| "cold-index evidence set did not satisfy release policy")
        }
    }
}

fn read_evidence_set(directory: &PathBuf) -> Result<Vec<ColdIndexEvidence>, &'static str> {
    let mut paths = fs::read_dir(directory)
        .map_err(|_| "cold-index evidence directory could not be read")?
        .map(|entry| {
            let entry = entry.map_err(|_| "cold-index evidence directory is invalid")?;
            let file_type = entry
                .file_type()
                .map_err(|_| "cold-index evidence directory is invalid")?;
            if !file_type.is_file()
                || entry.path().extension().and_then(OsStr::to_str) != Some("json")
            {
                return Err("cold-index evidence directory contains an unexpected entry");
            }
            Ok(entry.path())
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let encoded = fs::read(path).map_err(|_| "cold-index evidence could not be read")?;
            decode_cold_index_evidence(&encoded).map_err(|_| "cold-index evidence is invalid")
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Verify(PathBuf),
    VerifySet(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    corpus: PathBuf,
    source_revision: String,
    candidate_sha256: String,
    mode: Mode,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Options, &'static str> {
    let mut arguments = arguments.into_iter();
    let mut corpus = None;
    let mut source_revision = None;
    let mut candidate_sha256 = None;
    let mut mode = None;
    while let Some(flag) = next_argument(&mut arguments)? {
        let value =
            next_argument(&mut arguments)?.ok_or("cold-index evidence arguments are invalid")?;
        let slot = if flag == OsStr::new("--corpus") {
            &mut corpus
        } else if flag == OsStr::new("--source-revision") {
            &mut source_revision
        } else if flag == OsStr::new("--candidate-sha256") {
            &mut candidate_sha256
        } else if flag == OsStr::new("--verify") || flag == OsStr::new("--verify-set") {
            if mode.is_some() {
                return Err("cold-index evidence arguments are invalid");
            }
            mode = Some(if flag == OsStr::new("--verify") {
                Mode::Verify(PathBuf::from(value))
            } else {
                Mode::VerifySet(PathBuf::from(value))
            });
            continue;
        } else {
            return Err("cold-index evidence arguments are invalid");
        };
        if slot.replace(value).is_some() {
            return Err("cold-index evidence arguments are invalid");
        }
    }
    let source_revision = source_revision
        .and_then(|value| value.into_string().ok())
        .ok_or("cold-index evidence arguments are invalid")?;
    let candidate_sha256 = candidate_sha256
        .and_then(|value| value.into_string().ok())
        .ok_or("cold-index evidence arguments are invalid")?;
    Ok(Options {
        corpus: PathBuf::from(corpus.ok_or("cold-index evidence arguments are invalid")?),
        source_revision,
        candidate_sha256,
        mode: mode.ok_or("cold-index evidence arguments are invalid")?,
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
        return Err("cold-index evidence arguments are invalid");
    }
    Ok(Some(argument))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_exactly_one_verification_mode() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let digest = "a".repeat(64);
        let valid = [
            "--corpus",
            "benchmarks/cold-index-repositories.json",
            "--verify",
            "ripgrep.json",
            "--source-revision",
            revision,
            "--candidate-sha256",
            &digest,
        ];
        assert!(parse_arguments(valid.into_iter().map(OsString::from)).is_ok());
        let duplicate_mode = [
            "--corpus",
            "corpus.json",
            "--verify",
            "ripgrep.json",
            "--verify-set",
            "evidence",
            "--source-revision",
            revision,
            "--candidate-sha256",
            &digest,
        ];
        assert!(parse_arguments(duplicate_mode.into_iter().map(OsString::from)).is_err());
        assert!(parse_arguments(std::iter::empty()).is_err());
    }
}
