//! Discovers direct Dart dependency candidates for bounded project refinement.
//! This scanner only schedules admitted files together; the native adapter
//! remains responsible for directive validity, namespace filters and binding.

use super::{
    Cancellation, FirstSliceProjectAnalysisError, PROJECT_ADAPTER_PARTITION_CONTEXT_WORK,
    ProjectIncludeSource, exact_project_include, relative_include_path,
};
use std::collections::BTreeMap;

pub(super) fn for_each_import(
    source: &str,
    max_imports: usize,
    cancellation: &Cancellation,
    mut visit: impl FnMut(&str) -> Result<bool, FirstSliceProjectAnalysisError>,
) -> Result<usize, FirstSliceProjectAnalysisError> {
    let mut lexer = Lexer {
        remaining: source,
        cancellation,
    };
    let mut count = 0usize;
    let mut import = false;
    while count < max_imports.min(PROJECT_ADAPTER_PARTITION_CONTEXT_WORK) {
        let Some(token) = lexer.next()? else { break };
        if import && let Token::String(uri, raw) = token {
            // URI evaluation and package roots cannot be guessed from filenames.
            if !uri.is_empty()
                && !uri.starts_with('/')
                && !uri.contains([':', '\\', '?', '#', '%', '\r', '\n'])
                && (raw || !uri.contains('$'))
            {
                count = count.saturating_add(1);
                if !visit(uri)? {
                    break;
                }
            }
        }
        import = matches!(token, Token::Word("import"));
    }
    Ok(count)
}

pub(super) fn scan_top_level_partitions<'a, T: ProjectIncludeSource>(
    inputs: &[&'a T],
    partitions: &BTreeMap<&str, usize>,
    remaining_scan_bytes: &mut usize,
    remaining_imports: &mut usize,
    cancellation: &Cancellation,
    mut visit: impl FnMut(&'a T, &'a T) -> Result<bool, FirstSliceProjectAnalysisError>,
) -> Result<(), FirstSliceProjectAnalysisError> {
    if inputs
        .windows(2)
        .any(|pair| pair[0].include_path() >= pair[1].include_path())
        || inputs
            .iter()
            .any(|input| !partitions.contains_key(input.include_path()))
    {
        return Err(FirstSliceProjectAnalysisError::Analysis);
    }
    for consumer in inputs.iter().take(PROJECT_ADAPTER_PARTITION_CONTEXT_WORK) {
        check(cancellation)?;
        if *remaining_imports == 0 {
            break;
        }
        let Some(remaining) = remaining_scan_bytes.checked_sub(consumer.include_source().len())
        else {
            *remaining_scan_bytes = 0;
            break;
        };
        *remaining_scan_bytes = remaining;
        let Ok(source) = std::str::from_utf8(consumer.include_source()) else {
            continue;
        };
        let mut keep_scanning = true;
        let observed = for_each_import(source, *remaining_imports, cancellation, |uri| {
            let Some(path) = relative_include_path(consumer.include_path(), uri) else {
                return Ok(true);
            };
            let Some(provider) = exact_project_include(inputs, &path)
                .filter(|provider| provider.include_path() != consumer.include_path())
            else {
                return Ok(true);
            };
            // Adaptive analysis already recovers dependencies within each primary
            // partition. Replaying them here repeats native parsing and IR merges.
            if partitions.get(consumer.include_path()) != partitions.get(provider.include_path()) {
                keep_scanning = visit(consumer, provider)?;
            }
            Ok(keep_scanning)
        })?;
        *remaining_imports = remaining_imports.saturating_sub(observed);
        if !keep_scanning {
            break;
        }
    }
    Ok(())
}

fn check(cancellation: &Cancellation) -> Result<(), FirstSliceProjectAnalysisError> {
    cancellation
        .check()
        .map_err(|error| FirstSliceProjectAnalysisError::Cancelled(error.reason()))
}

#[derive(Clone, Copy)]
enum Token<'a> {
    Word(&'a str),
    String(&'a str, bool),
    Other,
}

struct Lexer<'a> {
    remaining: &'a str,
    cancellation: &'a Cancellation,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Result<Option<Token<'a>>, FirstSliceProjectAnalysisError> {
        loop {
            check(self.cancellation)?;
            self.remaining = self.remaining.trim_start();
            if let Some(comment) = self.remaining.strip_prefix("//") {
                self.remaining = comment
                    .get(comment.find(['\r', '\n']).unwrap_or(comment.len())..)
                    .unwrap_or("");
            } else if let Some(comment) = self.remaining.strip_prefix("/*") {
                self.remaining = comment;
                let mut depth = 1usize;
                while depth > 0 {
                    check(self.cancellation)?;
                    if let Some(tail) = self.remaining.strip_prefix("/*") {
                        depth = depth
                            .checked_add(1)
                            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
                        self.remaining = tail;
                    } else if let Some(tail) = self.remaining.strip_prefix("*/") {
                        depth -= 1;
                        self.remaining = tail;
                    } else if let Some(c) = self.remaining.chars().next() {
                        self.remaining = self.remaining.get(c.len_utf8()..).unwrap_or("");
                    } else {
                        return Ok(None);
                    }
                }
            } else {
                break;
            }
        }
        let Some(first) = self.remaining.chars().next() else {
            return Ok(None);
        };
        let raw = self.remaining.starts_with("r'") || self.remaining.starts_with("r\"");
        let quoted = if raw {
            self.remaining.get(1..).unwrap_or("")
        } else {
            self.remaining
        };
        if matches!(quoted.as_bytes().first(), Some(b'\'' | b'"')) {
            let triple = quoted.starts_with("'''") || quoted.starts_with("\"\"\"");
            let width = if triple { 3 } else { 1 };
            let delimiter = quoted
                .get(..width)
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let body = quoted
                .get(width..)
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            let mut tail = body;
            loop {
                check(self.cancellation)?;
                if let Some(rest) = tail.strip_prefix(delimiter) {
                    let uri = body
                        .get(..body.len() - tail.len())
                        .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
                    self.remaining = rest;
                    return Ok(Some(if triple {
                        Token::Other
                    } else {
                        Token::String(uri, raw)
                    }));
                }
                let Some(c) = tail.chars().next() else {
                    self.remaining = "";
                    return Ok(None);
                };
                tail = tail.get(c.len_utf8()..).unwrap_or("");
                if c == '\\'
                    && !raw
                    && let Some(escaped) = tail.chars().next()
                {
                    tail = tail.get(escaped.len_utf8()..).unwrap_or("");
                }
            }
        }
        if first.is_alphabetic() || matches!(first, '_' | '$') {
            let end = self
                .remaining
                .char_indices()
                .find_map(|(offset, c)| {
                    (!(c.is_alphanumeric() || matches!(c, '_' | '$'))).then_some(offset)
                })
                .unwrap_or(self.remaining.len());
            let word = self
                .remaining
                .get(..end)
                .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
            self.remaining = self.remaining.get(end..).unwrap_or("");
            Ok(Some(Token::Word(word)))
        } else {
            self.remaining = self.remaining.get(first.len_utf8()..).unwrap_or("");
            Ok(Some(Token::Other))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Input(&'static str, &'static [u8]);

    impl ProjectIncludeSource for Input {
        fn include_path(&self) -> &str {
            self.0
        }
        fn include_source(&self) -> &[u8] {
            self.1
        }
    }

    #[test]
    fn top_level_dependencies_skip_work_owned_by_primary_partitions() {
        let inputs = [
            Input("other/provider.dart", b"void selected() {}"),
            Input(
                "src/consumer.dart",
                b"import './provider.dart'; import '../../escape.dart'; import 'absent.dart';",
            ),
            Input("src/provider.dart", b"void selected() {}"),
        ];
        let refs = inputs.iter().collect::<Vec<_>>();
        for cross in [false, true] {
            let partitions = BTreeMap::from([
                ("other/provider.dart", 0),
                ("src/consumer.dart", 0),
                ("src/provider.dart", usize::from(cross)),
            ]);
            let mut bytes = 4096;
            let mut imports = 8;
            let mut pairs = Vec::new();
            scan_top_level_partitions(
                &refs,
                &partitions,
                &mut bytes,
                &mut imports,
                &Cancellation::new(),
                |consumer, provider| {
                    pairs.push((
                        consumer.include_path().to_owned(),
                        provider.include_path().to_owned(),
                    ));
                    Ok(true)
                },
            )
            .unwrap();
            assert_eq!(
                pairs.len(),
                usize::from(cross),
                "primary partitions already own same-partition dependency recovery"
            );
            if let Some(pair) = pairs.first() {
                assert_eq!(
                    pair,
                    &(
                        "src/consumer.dart".to_owned(),
                        "src/provider.dart".to_owned()
                    )
                );
            }
            assert_eq!(imports, 5);
            assert_eq!(
                bytes,
                4096 - inputs.iter().map(|input| input.1.len()).sum::<usize>()
            );
        }
    }

    #[test]
    fn dependency_discovery_respects_cancellation_and_byte_budget() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(super::super::CancellationReason::Shutdown));
        assert!(matches!(
            for_each_import("import 'a.dart';", 1, &cancellation, |_| Ok(true)),
            Err(FirstSliceProjectAnalysisError::Cancelled(_))
        ));
        let input = Input("entry.dart", b"import 'other.dart';");
        let mut bytes = 1;
        let mut imports = 8;
        scan_top_level_partitions(
            &[&input],
            &BTreeMap::from([("entry.dart", 0)]),
            &mut bytes,
            &mut imports,
            &Cancellation::new(),
            |_, _| panic!("unadmitted bytes must not schedule work"),
        )
        .unwrap();
        assert_eq!(bytes, 0);
        assert_eq!(imports, 8);
    }

    #[test]
    fn candidates_ignore_comments_strings_and_unsupported_uris() {
        let source = r#"
// import 'comment.dart';
/* outer /* import 'nested.dart'; */ comment */
const sample = "import 'string.dart';";
const multiline = '''import 'multiline.dart';''';
const raw = r"import 'raw.dart';";
import /* trivia */
 'library.dart' as api show run;
import r'../other.dart' hide hidden;
import 'package:external/api.dart';
import 'dart:io';
import '$variable.dart';
import 'escaped\x2edart';
import '/absolute.dart';
import 'encoded%20path.dart';
"#;
        let mut uris = Vec::new();
        let count = for_each_import(source, 20, &Cancellation::new(), |uri| {
            uris.push(uri.to_owned());
            Ok(true)
        })
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(uris, ["library.dart", "../other.dart"]);
    }

    #[test]
    fn candidate_budget_and_visitor_stop_bound_work() {
        for limit in [0, 1, 2] {
            let mut seen = 0;
            assert_eq!(
                for_each_import(
                    "import 'a.dart'; import 'b.dart';",
                    limit,
                    &Cancellation::new(),
                    |_| {
                        seen += 1;
                        Ok(true)
                    }
                )
                .unwrap(),
                limit
            );
            assert_eq!(seen, limit);
        }
        assert_eq!(
            for_each_import(
                "import 'a.dart'; import 'b.dart';",
                8,
                &Cancellation::new(),
                |_| Ok(false)
            )
            .unwrap(),
            1
        );
    }
}
