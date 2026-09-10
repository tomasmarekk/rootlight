//! Bounded interpreter identity for extensionless source discovery.
//! `env` operands are parsed separately from interpreter arguments so unrelated
//! paths, option values and environment assignments cannot select a parser.

use super::MAX_CLASSIFICATION_BYTES;

// Source NULs are rejected before tokenization. This marker preserves an
// expansion's position without reading process state or re-tokenizing its value.
const DYNAMIC: char = '\0';

pub(super) fn language(content: &[u8]) -> Option<&'static str> {
    let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES))?;
    let end = sample.iter().position(|byte| *byte == b'\n');
    if end.is_none() && sample.len() < content.len() {
        return None;
    }
    let line = std::str::from_utf8(sample.get(..end.unwrap_or(sample.len()))?).ok()?;
    if line.contains(DYNAMIC) {
        return None;
    }
    let command = line.strip_prefix("#!")?.trim_matches([' ', '\t', '\r']);
    let (interpreter, argument) = command
        .split_once([' ', '\t'])
        .map_or((command, ""), |(name, argument)| {
            (name, argument.trim_matches([' ', '\t']))
        });
    if executable_name(interpreter) != "env" {
        return interpreter_language(interpreter);
    }
    // The kernel supplies env's remaining shebang text as one argument. Only
    // explicit split-string syntax gives quotes or whitespace shell-like meaning.
    let split = argument
        .strip_prefix("-S")
        .or_else(|| argument.strip_prefix("-vS"))
        .or_else(|| argument.strip_prefix("--split-string="));
    match split {
        Some(split) => env_language(&split_words(split)?),
        None if !argument.bytes().any(|byte| byte.is_ascii_whitespace()) => {
            interpreter_language(argument)
        }
        None => None,
    }
}

fn executable_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".exe").unwrap_or(name)
}

fn interpreter_language(path: &str) -> Option<&'static str> {
    if path.contains(DYNAMIC) {
        return None;
    }
    let name = executable_name(path);
    if name == "perl" || versioned(name, "perl5") {
        Some("perl")
    } else if name == "python" || versioned(name, "python") {
        Some("python")
    } else {
        match name {
            "node" | "nodejs" | "deno" => Some("javascript"),
            "sh" | "bash" | "dash" | "ksh" => Some("bash"),
            _ => None,
        }
    }
}

fn versioned(name: &str, prefix: &str) -> bool {
    let Some(version) = name.strip_prefix(prefix) else {
        return false;
    };
    let version = if prefix == "perl5" {
        if version.is_empty() {
            return true;
        }
        let Some(version) = version.strip_prefix('.') else {
            return false;
        };
        version
    } else {
        version
    };
    version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn env_language(words: &[String]) -> Option<&'static str> {
    let mut words = words.iter().map(String::as_str);
    let mut options = true;
    while let Some(word) = words.next() {
        if options {
            match word {
                "--" | "-" => {
                    options = false;
                    continue;
                }
                "-i"
                | "--ignore-environment"
                | "-v"
                | "--debug"
                | "--block-signal"
                | "--default-signal"
                | "--ignore-signal"
                | "--list-signal-handling" => continue,
                "-u" | "--unset" | "-C" | "--chdir" => {
                    words.next()?;
                    continue;
                }
                _ if word.starts_with("--unset=")
                    || word.starts_with("--chdir=")
                    || word.starts_with("--block-signal=")
                    || word.starts_with("--default-signal=")
                    || word.starts_with("--ignore-signal=")
                    || (word.starts_with("-u") || word.starts_with("-C"))
                        && word
                            .get(2..)
                            .is_some_and(|operand| operand.chars().any(|ch| ch != DYNAMIC)) =>
                {
                    continue;
                }
                _ if word.starts_with('-') => return None,
                _ => options = false,
            }
        }
        if word
            .split_once('=')
            .is_some_and(|(name, _)| !name.is_empty() && !name.contains(DYNAMIC))
        {
            continue;
        }
        return interpreter_language(word);
    }
    None
}

fn split_words(input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if quote == Some('\'') {
            match ch {
                '\'' => quote = None,
                '\\' if matches!(chars.peek(), Some('\'' | '\\')) => word.push(chars.next()?),
                _ => word.push(ch),
            }
            continue;
        }
        match ch {
            '\'' if quote.is_none() => {
                quote = Some('\'');
                started = true;
            }
            '"' => {
                quote = if quote.is_none() { Some('"') } else { None };
                started = true;
            }
            '$' => {
                if chars.next()? != '{' {
                    return None;
                }
                let first = chars.next()?;
                if first != '_' && !first.is_ascii_alphabetic() {
                    return None;
                }
                loop {
                    match chars.next()? {
                        '}' => break,
                        ch if ch == '_' || ch.is_ascii_alphanumeric() => {}
                        _ => return None,
                    }
                }
                word.push(DYNAMIC);
                started = true;
            }
            '#' if quote.is_none() && !started => break,
            '\\' => {
                let escaped = chars.next()?;
                if escaped == 'c' && quote.is_none() {
                    break;
                }
                if escaped == '_' && quote.is_none() {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                    continue;
                }
                word.push(match escaped {
                    'f' => '\u{000c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'v' => '\u{000b}',
                    '_' => ' ',
                    '\\' | '#' | '$' | '"' | '\'' => escaped,
                    _ => return None,
                });
                started = true;
            }
            ch if ch.is_ascii_whitespace() && quote.is_none() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_env_operands_do_not_hide_a_static_interpreter() {
        for argument in [
            r"perl -I${SOURCE_INCLUDE}",
            r"PERL5LIB=${SOURCE_INCLUDE} perl",
            r"-u ${SOURCE_UNUSED} perl",
            r"--unset=${SOURCE_UNUSED} perl",
            r"-uPREFIX${SOURCE_UNUSED} perl",
            r"-u${SOURCE_UNUSED}SUFFIX perl",
            r"--chdir=${SOURCE_DIRECTORY} perl",
        ] {
            let source = format!("#!/usr/bin/env -S {argument}\n");
            assert_eq!(language(source.as_bytes()), Some("perl"), "{argument}");
        }
        for argument in [
            r"${SOURCE_COMMAND} -w",
            r"${SOURCE_DIRECTORY}/perl -w",
            r"per${SOURCE_SUFFIX} -w",
            r"${SOURCE_NAME}=value perl",
            r"-u${SOURCE_UNUSED} perl",
            r"perl -I${UNCLOSED",
            r"perl -I$UNBRACED",
            r"perl -I${INVALID-NAME}",
        ] {
            let source = format!("#!/usr/bin/env -S {argument}\n");
            assert_eq!(language(source.as_bytes()), None, "{argument}");
        }
        assert_eq!(language(b"#!/usr/bin/env -S perl\0\n"), None);
        assert_eq!(language(b"#!/usr/bin/perl\0\n"), None);
    }

    #[test]
    fn split_interpreters_respect_quoting_escapes_and_env_operands() {
        for argument in [
            r#"-S per"l" -w"#,
            r"-S perl\_-w",
            r"-S perl \c ignored",
            r"-S perl # ignored",
            r"-S --unset=UNUSED --chdir=/tmp --ignore-signal=PIPE perl",
            r"-S --default-signal -- perl",
            r"-vS perl -w",
        ] {
            let source = format!("#!/usr/bin/env {argument}\n");
            assert_eq!(language(source.as_bytes()), Some("perl"), "{argument}");
        }
        for argument in [
            r"-S # perl",
            r"-S -u perl",
            r"-S --help perl",
            r"-S --version perl",
            r"-S --null perl",
            r"-S perl6",
            r"-S PERL=perl printf",
            r#"-S "perl\c""#,
            r"-S per\ql",
        ] {
            let source = format!("#!/usr/bin/env {argument}\n");
            assert_eq!(language(source.as_bytes()), None, "{argument}");
        }
        assert_eq!(
            split_words(r#"'a b' "c\_d" e\_f"#),
            Some(vec![
                "a b".to_owned(),
                "c d".to_owned(),
                "e".to_owned(),
                "f".to_owned()
            ])
        );
    }
}
