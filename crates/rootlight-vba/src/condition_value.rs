//! Evaluates a bounded Boolean subset of compiler expressions without a runtime.
//! Callers prove compiler-name bindings and the surrounding directive environment;
//! unknown values and unsupported VBA expressions cannot select a false branch.

use super::{logical_whitespace_len, name};

/// The evidence available after evaluating the supported Boolean subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilationConditionValue {
    /// A complete supported expression whose every operand has a proven Boolean value.
    Proven(bool),
    /// An unknown name, unsupported expression or caller-imposed resource limit.
    Unproven,
    /// Invalid operator/grouping syntax made entirely of supported tokens.
    ///
    /// This is not a general VBA validity judgement. Out-of-subset syntax takes
    /// precedence, including application-shaped parentheses after an operand.
    Malformed,
}

/// Caller-imposed limits for source names and the iterative evaluation stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilationConditionLimits {
    /// Maximum byte length of one ASCII logical name, excluding brackets.
    pub maximum_name_bytes: usize,
    /// Maximum combined live entries across the operand and operator stacks.
    ///
    /// Neither stack is preallocated from this limit or the input length.
    pub maximum_stack_entries: usize,
}

/// Evaluates a compiler expression only when all values are proven Booleans.
///
/// Supports `True`, `False`, `Not`, `And`, `Or`, `Xor`, `Eqv`, `Imp`, parentheses
/// and ASCII names. Operators and literals are case-insensitive. Bracketed names
/// are passed to `name_value` without brackets and in their original case;
/// bracketed operator/literal spellings are unsupported rather than reinterpreted.
/// The callback must supply only unambiguous, previously proven Boolean compiler
/// bindings in the applicable environment. It returns `None` for every other name.
/// Numeric, string, date, Empty/Null/Nothing and Variant coercions are not performed.
///
/// Operators follow MS-VBAL 5.6.9.1 precedence and left associativity. Every name
/// in a complete supported expression is looked up eagerly, even after an unknown
/// operand or a left Boolean value that could determine a simplified result.
/// Unknown operands propagate through every operator; no Boolean simplification
/// can turn an unproven expression into a proven result.
///
/// A cancellable, allocation-free lexical pass precedes evaluation so unsupported
/// VBA syntax is never diagnosed as malformed supported syntax. Calls and indexed
/// applications are unsupported even when their bytes use only names/parentheses.
/// Bare newlines and comments are unsupported; legal space/tab-based logical line
/// continuations are accepted. An empty expression is malformed.
/// Parsing is iterative and linear; combined live stack entries never exceed
/// `limits.maximum_stack_entries`. Exceeding either limit returns `Unproven`.
/// No syntax tree or token list is allocated, and no spelling-key API is changed.
/// The caller separately validates every `#If`/`#ElseIf` expression and directive
/// topology, including expressions in excluded blocks (MS-VBAL 3.4.2/5.6.16.2).
///
/// # Errors
/// Propagates `name_value` and cancellation/work callback errors unchanged.
pub fn compilation_condition_value<E>(
    source: &[u8],
    limits: CompilationConditionLimits,
    name_value: &mut impl FnMut(&[u8]) -> Result<Option<bool>, E>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<CompilationConditionValue, E> {
    check()?;
    if !supported_tokens(source, limits.maximum_name_bytes, check)? {
        return Ok(CompilationConditionValue::Unproven);
    }
    let mut evaluation = Evaluation {
        operators: Vec::new(),
        operands: Vec::new(),
        maximum_entries: limits.maximum_stack_entries,
    };
    let mut cursor = 0;
    let mut operand = true;
    loop {
        let token = match read_token(source, &mut cursor, limits.maximum_name_bytes, check)? {
            Read::End => break,
            Read::Unsupported => return Ok(CompilationConditionValue::Unproven),
            Read::Token(token) => token,
        };
        match token {
            Token::Open if operand => {
                if !evaluation.has_room() {
                    return Ok(CompilationConditionValue::Unproven);
                }
                evaluation.operators.push(Operator::Open);
            }
            Token::Close if !operand => {
                while evaluation
                    .operators
                    .last()
                    .is_some_and(|operator| *operator != Operator::Open)
                {
                    if !evaluation.reduce(check)? {
                        return Ok(CompilationConditionValue::Malformed);
                    }
                }
                if evaluation.operators.pop() != Some(Operator::Open) {
                    return Ok(CompilationConditionValue::Malformed);
                }
            }
            Token::Logical(Operator::Not) if operand => {
                if !evaluation.has_room() {
                    return Ok(CompilationConditionValue::Unproven);
                }
                evaluation.operators.push(Operator::Not);
            }
            Token::Logical(operator) if !operand && operator != Operator::Not => {
                while evaluation.operators.last().is_some_and(|previous| {
                    *previous != Operator::Open && previous.precedence() >= operator.precedence()
                }) {
                    if !evaluation.reduce(check)? {
                        return Ok(CompilationConditionValue::Malformed);
                    }
                }
                if !evaluation.has_room() {
                    return Ok(CompilationConditionValue::Unproven);
                }
                evaluation.operators.push(operator);
                operand = true;
            }
            Token::Literal(value) if operand => {
                if !evaluation.has_room() {
                    return Ok(CompilationConditionValue::Unproven);
                }
                evaluation.operands.push(Some(value));
                operand = false;
            }
            Token::Name(name) if operand => {
                let value = name_value(name)?;
                if !evaluation.has_room() {
                    return Ok(CompilationConditionValue::Unproven);
                }
                evaluation.operands.push(value);
                operand = false;
            }
            Token::Open | Token::Close | Token::Logical(_) | Token::Literal(_) | Token::Name(_) => {
                return Ok(CompilationConditionValue::Malformed);
            }
        }
    }
    if operand {
        return Ok(CompilationConditionValue::Malformed);
    }
    while !evaluation.operators.is_empty() {
        if !evaluation.reduce(check)? {
            return Ok(CompilationConditionValue::Malformed);
        }
    }
    if evaluation.operands.len() != 1 {
        return Ok(CompilationConditionValue::Malformed);
    }
    Ok(match evaluation.operands.pop() {
        Some(Some(value)) => CompilationConditionValue::Proven(value),
        Some(None) => CompilationConditionValue::Unproven,
        None => CompilationConditionValue::Malformed,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Open,
    Imp,
    Eqv,
    Xor,
    Or,
    And,
    Not,
}

impl Operator {
    fn precedence(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Imp => 1,
            Self::Eqv => 2,
            Self::Xor => 3,
            Self::Or => 4,
            Self::And => 5,
            Self::Not => 6,
        }
    }

    fn word(word: &[u8]) -> Option<Self> {
        [
            Self::Not,
            Self::And,
            Self::Or,
            Self::Xor,
            Self::Eqv,
            Self::Imp,
        ]
        .into_iter()
        .find(|operator| {
            word.eq_ignore_ascii_case(match operator {
                Self::Not => b"Not",
                Self::And => b"And",
                Self::Or => b"Or",
                Self::Xor => b"Xor",
                Self::Eqv => b"Eqv",
                Self::Imp => b"Imp",
                Self::Open => b"",
            })
        })
    }

    fn apply(self, left: bool, right: bool) -> Option<bool> {
        match self {
            Self::And => Some(left & right),
            Self::Or => Some(left | right),
            Self::Xor => Some(left ^ right),
            Self::Eqv => Some(left == right),
            Self::Imp => Some(!left | right),
            Self::Open | Self::Not => None,
        }
    }
}

struct Evaluation {
    operators: Vec<Operator>,
    operands: Vec<Option<bool>>,
    maximum_entries: usize,
}

impl Evaluation {
    fn has_room(&self) -> bool {
        self.operands.len() < self.maximum_entries.saturating_sub(self.operators.len())
    }

    fn reduce<E>(&mut self, check: &mut impl FnMut() -> Result<(), E>) -> Result<bool, E> {
        check()?;
        let Some(operator) = self.operators.pop() else {
            return Ok(false);
        };
        if operator == Operator::Open {
            return Ok(false);
        }
        let Some(right) = self.operands.pop() else {
            return Ok(false);
        };
        let value = if operator == Operator::Not {
            right.map(|value| !value)
        } else {
            let Some(left) = self.operands.pop() else {
                return Ok(false);
            };
            // Unknown is absent evidence, not VBA Null or a Boolean identity.
            match (left, right) {
                (Some(left), Some(right)) => operator.apply(left, right),
                (Some(_), None) | (None, Some(_)) | (None, None) => None,
            }
        };
        self.operands.push(value);
        Ok(true)
    }
}

#[derive(Debug, Clone, Copy)]
enum Token<'a> {
    Open,
    Close,
    Logical(Operator),
    Literal(bool),
    Name(&'a [u8]),
}

enum Read<'a> {
    End,
    Unsupported,
    Token(Token<'a>),
}

fn supported_tokens<E>(
    source: &[u8],
    maximum_name_bytes: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut cursor = 0;
    let mut complete_operand = false;
    loop {
        match read_token(source, &mut cursor, maximum_name_bytes, check)? {
            Read::End => return Ok(true),
            Read::Unsupported => return Ok(false),
            Read::Token(Token::Open) if complete_operand => return Ok(false),
            Read::Token(token) => {
                complete_operand =
                    matches!(token, Token::Close | Token::Literal(_) | Token::Name(_));
            }
        }
    }
}

fn read_token<'a, E>(
    source: &'a [u8],
    cursor: &mut usize,
    maximum_name_bytes: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Read<'a>, E> {
    *cursor += logical_whitespace_len(source.get(*cursor..).unwrap_or_default(), check)?;
    check()?;
    let Some(&byte) = source.get(*cursor) else {
        return Ok(Read::End);
    };
    if matches!(byte, b'(' | b')') {
        *cursor += 1;
        return Ok(Read::Token(if byte == b'(' {
            Token::Open
        } else {
            Token::Close
        }));
    }
    let bracketed = byte == b'[';
    let Some(word) = name(source, cursor, check)? else {
        return Ok(Read::Unsupported);
    };
    if word.len() > maximum_name_bytes {
        return Ok(Read::Unsupported);
    }
    let logical = Operator::word(word);
    let literal = if word.eq_ignore_ascii_case(b"True") {
        Some(true)
    } else if word.eq_ignore_ascii_case(b"False") {
        Some(false)
    } else {
        None
    };
    let variant_literal = [b"Empty".as_slice(), b"Null", b"Nothing"]
        .iter()
        .any(|literal| word.eq_ignore_ascii_case(literal));
    let unsupported_operator = [b"Mod".as_slice(), b"Like", b"Is"]
        .iter()
        .any(|operator| word.eq_ignore_ascii_case(operator));
    let unsupported_prefix = !bracketed
        && [b"New".as_slice(), b"AddressOf", b"TypeOf"]
            .iter()
            .any(|prefix| word.eq_ignore_ascii_case(prefix));
    if variant_literal
        || unsupported_operator
        || unsupported_prefix
        || (bracketed && (logical.is_some() || literal.is_some()))
    {
        return Ok(Read::Unsupported);
    }
    Ok(Read::Token(if let Some(operator) = logical {
        Token::Logical(operator)
    } else if let Some(value) = literal {
        Token::Literal(value)
    } else {
        Token::Name(word)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> CompilationConditionLimits {
        CompilationConditionLimits {
            maximum_name_bytes: 128,
            maximum_stack_entries: 8193,
        }
    }

    fn evaluate(source: &[u8]) -> CompilationConditionValue {
        compilation_condition_value(
            source,
            limits(),
            &mut |_| Ok(None),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap()
    }

    #[test]
    fn boolean_operators_follow_complete_truth_tables() {
        for (operator, expected) in [
            ("And", [true, false, false, false]),
            ("Or", [true, true, true, false]),
            ("Xor", [false, true, true, false]),
            ("Eqv", [true, false, false, true]),
            ("Imp", [true, false, true, true]),
        ] {
            for ((left, right), expected) in [
                ("True", "True"),
                ("True", "False"),
                ("False", "True"),
                ("False", "False"),
            ]
            .into_iter()
            .zip(expected)
            {
                let source = format!("{left} {operator} {right}");
                assert_eq!(
                    evaluate(source.as_bytes()),
                    CompilationConditionValue::Proven(expected),
                    "{source}"
                );
            }
        }
        assert_eq!(
            evaluate(b"Not True"),
            CompilationConditionValue::Proven(false)
        );
        assert_eq!(
            evaluate(b"Not False"),
            CompilationConditionValue::Proven(true)
        );
        assert_eq!(
            evaluate(b"Not Not True"),
            CompilationConditionValue::Proven(true)
        );
    }

    #[test]
    fn precedence_grouping_and_binary_associativity_match_vba() {
        for (source, expected) in [
            ("Not True And False", false),
            ("Not (True And False)", true),
            ("True Or False And False", true),
            ("(True Or False) And False", false),
            ("True Or True Xor True", false),
            ("False Xor True Eqv True", true),
            ("False Eqv True Imp False", true),
            ("False Imp False Imp False", false),
            ("False Imp (False Imp False)", true),
            ("((Not False))", true),
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Proven(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn logical_names_preserve_case_and_are_looked_up_eagerly() {
        let mut names = Vec::new();
        let value = compilation_condition_value(
            b"[mIxEd] And Not other Or MIXED",
            limits(),
            &mut |name| {
                names.push(name.to_vec());
                Ok(Some(!name.eq_ignore_ascii_case(b"other")))
            },
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(value, CompilationConditionValue::Proven(true));
        assert_eq!(
            names,
            [b"mIxEd".to_vec(), b"other".to_vec(), b"MIXED".to_vec()]
        );
        for source in [
            "False And Missing Or Known",
            "True Or Missing And Known",
            "Missing Imp Known",
        ] {
            let mut names = Vec::new();
            let value = compilation_condition_value(
                source.as_bytes(),
                limits(),
                &mut |name| {
                    names.push(name.to_vec());
                    Ok(name.eq_ignore_ascii_case(b"Known").then_some(true))
                },
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            assert_eq!(value, CompilationConditionValue::Unproven, "{source}");
            assert_eq!(names, [b"Missing".to_vec(), b"Known".to_vec()]);
        }
    }

    #[test]
    fn unsupported_vba_syntax_is_not_diagnosed_as_malformed() {
        for source in [
            "0",
            "-1",
            "\"True\"",
            "#1/1/2000#",
            "Empty",
            "Null",
            "Nothing",
            "CBool(0)",
            "Len(\"sample\")",
            "Name()",
            "Name(Other)",
            "(Name)(Other)",
            "Name.Member",
            "Name%",
            "True = False",
            "True And False + 1",
            "And True + 1",
            "True Like False",
            "True Mod False",
            "True Is False",
            "New Name",
            "AddressOf Callback",
            "TypeOf Name Is Object",
            "[True]",
            "[False]",
            "[Not] True",
            "True [And] False",
            "[Null]",
            "[foreign name]",
            "[open",
            "True 'comment",
            "True: False",
            "True\nAnd False",
            "True\rAnd False",
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Unproven,
                "{source:?}"
            );
        }
    }

    #[test]
    fn supported_malformed_syntax_is_distinct_from_missing_name_evidence() {
        for source in [
            "",
            " \t",
            "()",
            "(True",
            "True)",
            "True False",
            "Not",
            "And True",
            "True Or",
            "True Not False",
            "Unknown And",
            "Unknown Unknown",
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Malformed,
                "{source:?}"
            );
        }
        for source in [
            "Unknown",
            "Not Unknown",
            "False And Unknown",
            "True Or Unknown",
            "Unknown Xor True",
            "Unknown Eqv False",
            "False Imp Unknown",
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Unproven,
                "{source}"
            );
        }
    }

    #[test]
    fn exact_logical_continuations_preserve_boolean_values() {
        for source in [
            "tRuE _\r\n AnD _\n NoT fAlSe",
            "\t True\tAnd Not False ",
            "True \t_ \t\r\n And False Or True",
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Proven(true),
                "{source:?}"
            );
        }
        for source in [
            "True_\nAnd False",
            "True _\rAnd False",
            "True\r\nAnd False",
            "True _ 'comment\nAnd False",
        ] {
            assert_eq!(
                evaluate(source.as_bytes()),
                CompilationConditionValue::Unproven,
                "{source:?}"
            );
        }
    }

    #[test]
    fn combined_stack_and_name_limits_have_exact_boundaries() {
        for (source, minimum) in [
            ("True", 1),
            ("(True)", 2),
            ("((True))", 3),
            ("Not Not True", 3),
            ("True And False", 3),
        ] {
            for (maximum, expected) in [
                (minimum - 1, CompilationConditionValue::Unproven),
                (
                    minimum,
                    CompilationConditionValue::Proven(source != "True And False"),
                ),
            ] {
                let value = compilation_condition_value(
                    source.as_bytes(),
                    CompilationConditionLimits {
                        maximum_stack_entries: maximum,
                        ..limits()
                    },
                    &mut |_| Ok(None),
                    &mut || Ok::<(), ()>(()),
                )
                .unwrap();
                assert_eq!(value, expected, "{source}: limit {maximum}");
            }
        }
        let source = "Feature".repeat(16);
        for (maximum, expected) in [
            (source.len() - 1, CompilationConditionValue::Unproven),
            (source.len(), CompilationConditionValue::Proven(true)),
        ] {
            let value = compilation_condition_value(
                source.as_bytes(),
                CompilationConditionLimits {
                    maximum_name_bytes: maximum,
                    ..limits()
                },
                &mut |_| Ok(Some(true)),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn lookup_errors_are_not_hidden_by_boolean_identities() {
        for source in [b"False And Missing".as_slice(), b"True Or Missing"] {
            assert_eq!(
                compilation_condition_value(
                    source,
                    limits(),
                    &mut |_| Err("lookup failed"),
                    &mut || Ok(())
                ),
                Err("lookup failed")
            );
        }
    }

    #[test]
    fn scans_and_reductions_propagate_exact_work_errors() {
        for source in [
            " ".repeat(8192),
            "Feature".repeat(2048),
            format!("{}True{}", "(".repeat(4096), ")".repeat(4096)),
            format!("{}True", "Not ".repeat(4096)),
            format!("True{}", " Or False".repeat(4096)),
        ] {
            for fail_at in [1, 3, 6] {
                let mut calls = 0;
                let result = compilation_condition_value(
                    source.as_bytes(),
                    limits(),
                    &mut |_| Ok(Some(true)),
                    &mut || {
                        calls += 1;
                        if calls == fail_at {
                            Err(("work exhausted", fail_at))
                        } else {
                            Ok(())
                        }
                    },
                );
                assert_eq!(result, Err(("work exhausted", fail_at)));
                assert_eq!(calls, fail_at);
            }
        }
        let looked_up = std::cell::Cell::new(false);
        let result = compilation_condition_value(
            b"Feature",
            limits(),
            &mut |_| {
                looked_up.set(true);
                Ok(Some(true))
            },
            &mut || {
                if looked_up.get() {
                    Err("cancelled after lookup")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result, Err("cancelled after lookup"));
    }

    #[test]
    fn deep_and_long_supported_inputs_remain_iterative_and_bounded() {
        let source = format!("{}True{}", "(".repeat(8192), ")".repeat(8192));
        assert_eq!(
            evaluate(source.as_bytes()),
            CompilationConditionValue::Proven(true)
        );
        assert_eq!(
            compilation_condition_value(
                source.as_bytes(),
                CompilationConditionLimits {
                    maximum_stack_entries: 8192,
                    ..limits()
                },
                &mut |_| Ok(None),
                &mut || Ok::<(), ()>(())
            )
            .unwrap(),
            CompilationConditionValue::Unproven
        );
        let source = format!("True{}", " Or False".repeat(8192));
        assert_eq!(
            evaluate(source.as_bytes()),
            CompilationConditionValue::Proven(true)
        );
    }

    #[test]
    fn arbitrary_bytes_and_every_truncation_are_total() {
        for byte in 0..=u8::MAX {
            let source = [b"(True Or ".as_slice(), &[byte], b"False)"].concat();
            for end in 0..=source.len() {
                let _ = evaluate(&source[..end]);
            }
        }
    }
}
