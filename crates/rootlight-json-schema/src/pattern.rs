//! Bounded pattern matching for the JSON Schema regular-expression subset.
//!
//! The compiler rejects constructs outside this subset instead of silently
//! weakening a contract. Matching is bounded by the candidate string rather
//! than an untrusted quantifier.

use std::collections::BTreeSet;

#[derive(Debug)]
pub(super) struct Pattern {
    alternatives: Vec<Vec<Piece>>,
    anchored_start: bool,
    anchored_end: bool,
}

#[derive(Debug)]
struct Piece {
    atom: Atom,
    minimum: usize,
    maximum: Option<usize>,
}

#[derive(Debug)]
enum Atom {
    Character(char),
    Class(Vec<(char, char)>),
    Group(Vec<Vec<Piece>>),
}

impl Pattern {
    pub(super) fn compile(source: &str) -> Result<Self, String> {
        let mut parser = Parser {
            input: source.chars().collect(),
            offset: 0,
        };
        let anchored_start = parser.take('^');
        let alternatives = parser.alternatives()?;
        if alternatives.len() != 1 {
            return Err("top-level pattern alternatives require an explicit group".to_owned());
        }
        let anchored_end = parser.take('$');
        if parser.peek().is_some() {
            return Err(format!(
                "unsupported pattern syntax at character {}",
                parser.offset
            ));
        }
        Ok(Self {
            alternatives,
            anchored_start,
            anchored_end,
        })
    }

    pub(super) fn matches(&self, candidate: &str) -> bool {
        let chars: Vec<char> = candidate.chars().collect();
        let starts = if self.anchored_start {
            0..1
        } else {
            0..chars.len().saturating_add(1)
        };
        starts.into_iter().any(|start| {
            evaluate(&self.alternatives, &chars, start)
                .into_iter()
                .any(|end| !self.anchored_end || end == chars.len())
        })
    }
}

fn evaluate(alternatives: &[Vec<Piece>], chars: &[char], start: usize) -> BTreeSet<usize> {
    let mut matches = BTreeSet::new();
    for sequence in alternatives {
        let mut positions = BTreeSet::from([start]);
        for piece in sequence {
            let mut next_positions = BTreeSet::new();
            for position in positions {
                next_positions.extend(evaluate_piece(piece, chars, position));
            }
            positions = next_positions;
            if positions.is_empty() {
                break;
            }
        }
        matches.extend(positions);
    }
    matches
}

fn evaluate_piece(piece: &Piece, chars: &[char], start: usize) -> BTreeSet<usize> {
    let mut result = BTreeSet::new();
    let mut current = BTreeSet::from([start]);
    if piece.minimum == 0 {
        result.insert(start);
    }
    let limit = piece.maximum.unwrap_or(chars.len()).min(chars.len());
    for repetition in 1..=limit {
        let mut next = BTreeSet::new();
        for position in current {
            next.extend(match_atom(&piece.atom, chars, position));
        }
        if next.is_empty() {
            break;
        }
        if repetition >= piece.minimum {
            result.extend(next.iter().copied());
        }
        current = next;
    }
    result
}

fn match_atom(atom: &Atom, chars: &[char], start: usize) -> BTreeSet<usize> {
    match atom {
        Atom::Character(expected) => chars
            .get(start)
            .filter(|actual| *actual == expected)
            .map_or_else(BTreeSet::new, |_| BTreeSet::from([start + 1])),
        Atom::Class(ranges) => chars
            .get(start)
            .filter(|actual| {
                ranges
                    .iter()
                    .any(|(first, last)| first <= *actual && *actual <= last)
            })
            .map_or_else(BTreeSet::new, |_| BTreeSet::from([start + 1])),
        Atom::Group(alternatives) => evaluate(alternatives, chars, start),
    }
}

struct Parser {
    input: Vec<char>,
    offset: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.input.get(self.offset).copied()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.offset += 1;
        Some(value)
    }

    fn take(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn alternatives(&mut self) -> Result<Vec<Vec<Piece>>, String> {
        let mut alternatives = Vec::new();
        loop {
            alternatives.push(self.sequence()?);
            if !self.take('|') {
                break;
            }
        }
        Ok(alternatives)
    }

    fn sequence(&mut self) -> Result<Vec<Piece>, String> {
        let mut sequence = Vec::new();
        while let Some(character) = self.peek() {
            if matches!(character, '|' | ')' | '$') {
                break;
            }
            let atom = match self.next() {
                Some('(') => {
                    let alternatives = self.alternatives()?;
                    if !self.take(')') {
                        return Err("unclosed pattern group".to_owned());
                    }
                    if alternatives.iter().any(Vec::is_empty) {
                        return Err("empty pattern group alternative".to_owned());
                    }
                    Atom::Group(alternatives)
                }
                Some('[') => Atom::Class(self.class()?),
                Some('\\') => {
                    let escaped = self
                        .next()
                        .ok_or_else(|| "trailing pattern escape".to_owned())?;
                    if !matches!(
                        escaped,
                        '\\' | '.'
                            | '^'
                            | '$'
                            | '['
                            | ']'
                            | '('
                            | ')'
                            | '{'
                            | '}'
                            | '|'
                            | '+'
                            | '*'
                            | '?'
                            | '-'
                    ) {
                        return Err(format!("unsupported pattern escape \\{escaped}"));
                    }
                    Atom::Character(escaped)
                }
                Some('.') => return Err("wildcard pattern atoms are unsupported".to_owned()),
                Some(character @ ('*' | '+' | '?' | '{' | '}' | '^')) => {
                    return Err(format!("unsupported pattern token {character}"));
                }
                Some(character) => Atom::Character(character),
                None => return Err("missing pattern atom".to_owned()),
            };
            let (minimum, maximum) = self.quantifier()?;
            sequence.push(Piece {
                atom,
                minimum,
                maximum,
            });
        }
        Ok(sequence)
    }

    fn class(&mut self) -> Result<Vec<(char, char)>, String> {
        if self.peek() == Some('^') {
            return Err("negated character classes are unsupported".to_owned());
        }
        let mut ranges = Vec::new();
        while self.peek() != Some(']') {
            let first = self
                .next()
                .ok_or_else(|| "unclosed character class".to_owned())?;
            let first = if first == '\\' {
                let escaped = self
                    .next()
                    .ok_or_else(|| "trailing class escape".to_owned())?;
                if !matches!(escaped, '\\' | '-' | ']') {
                    return Err(format!("unsupported class escape \\{escaped}"));
                }
                escaped
            } else {
                first
            };
            if self.peek() == Some('-') && self.input.get(self.offset + 1) != Some(&']') {
                self.next();
                let last = self
                    .next()
                    .ok_or_else(|| "unclosed class range".to_owned())?;
                if first > last {
                    return Err("reversed character class range".to_owned());
                }
                ranges.push((first, last));
            } else {
                ranges.push((first, first));
            }
        }
        self.next();
        if ranges.is_empty() {
            return Err("empty character class".to_owned());
        }
        Ok(ranges)
    }

    fn quantifier(&mut self) -> Result<(usize, Option<usize>), String> {
        if self.take('+') {
            return Ok((1, None));
        }
        if self.take('*') {
            return Ok((0, None));
        }
        if self.take('?') {
            return Ok((0, Some(1)));
        }
        if !self.take('{') {
            return Ok((1, Some(1)));
        }
        let minimum = self.number()?;
        let maximum = if self.take(',') {
            if self.peek() == Some('}') {
                None
            } else {
                Some(self.number()?)
            }
        } else {
            Some(minimum)
        };
        if !self.take('}') || maximum.is_some_and(|max| max < minimum) {
            return Err("invalid pattern repetition".to_owned());
        }
        Ok((minimum, maximum))
    }

    fn number(&mut self) -> Result<usize, String> {
        let mut value = None::<usize>;
        while let Some(digit) = self.peek().and_then(|character| character.to_digit(10)) {
            self.next();
            value = Some(
                value
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(digit as usize))
                    .ok_or_else(|| "pattern repetition is too large".to_owned())?,
            );
        }
        value.ok_or_else(|| "missing pattern repetition".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    #[test]
    fn matches_contract_identifiers_and_rejects_near_misses() {
        let pattern = Pattern::compile("^sym1_[a-z2-7]{39}$").unwrap();
        assert!(pattern.matches(&format!("sym1_{}", "a".repeat(39))));
        assert!(!pattern.matches(&format!("sym1_{}", "a".repeat(38))));
        assert!(!pattern.matches(&format!("sym1_{}", "1".repeat(39))));
    }

    #[test]
    fn matches_numeric_alternatives_without_exponential_repetition() {
        let pattern = Pattern::compile("^1\\.(0|[1-9][0-9]{0,3}|[1-5][0-9]{4})$").unwrap();
        for valid in ["1.0", "1.3", "1.9999", "1.59999"] {
            assert!(pattern.matches(valid), "{valid}");
        }
        for invalid in ["1.00", "1.60000", "2.1", "1.999999"] {
            assert!(!pattern.matches(invalid), "{invalid}");
        }
    }

    #[test]
    fn rejects_unclosed_and_unsupported_patterns() {
        for invalid in [
            "[abc", "(a|b", "(a|)", "a{3,2}", "a{", "a\\", "[^x]", "a.b", "\\d", "^a|b$",
        ] {
            assert!(Pattern::compile(invalid).is_err(), "{invalid}");
        }
    }
}
