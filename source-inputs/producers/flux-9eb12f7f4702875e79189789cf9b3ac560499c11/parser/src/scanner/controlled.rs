//! Result-returning wrapper around the standard Flux scanner.

use std::{collections::HashMap, str, vec::Vec};

use crate::{
    ast::Comment,
    control::{checked_source_len, Control, Stop},
};

#[rustfmt::skip]
#[allow(warnings, missing_docs)]
#[path = "scanner_generated_controlled.rs"]
mod scanner_generated;
use scanner_generated::scan;

use super::{Position, Token, TokenType};

use super::DefaultHasher;

/// Represents a Flux scanner and its state during compilation.
pub struct ControlledScanner<'a, 'c> {
    data: &'a [u8],
    control: Control<'c>,
    ps: i32,
    p: i32,
    pe: i32,
    eof: i32,
    last_newline: i32,
    cur_line: i32,
    checkpoint: i32,
    checkpoint_line: i32,
    checkpoint_last_newline: i32,
    token: TokenType,
    positions: HashMap<Position, u32, DefaultHasher>,

    /// Comments
    pub comments: Vec<Comment>,
}

impl<'a, 'c> ControlledScanner<'a, 'c> {
    /// Create a new scanner with the provided input.
    pub fn new(input: &'a str, mut control: Control<'c>) -> Result<Self, Stop> {
        control.check()?;
        let end = checked_source_len(input.len(), control.limits.source_bytes)?;
        Ok(Self {
            control,
            data: input.as_bytes(),
            ps: 0,
            p: 0,
            pe: end,
            eof: end,
            last_newline: 0,
            cur_line: 1,
            token: TokenType::Illegal,
            checkpoint: 0,
            checkpoint_line: 1,
            checkpoint_last_newline: 0,
            positions: HashMap::default(),
            comments: Vec::new(),
        })
    }

    fn _scan(&mut self, mode: i32) -> Result<Token, Stop> {
        self.control.check()?;
        if !(0..=2).contains(&mode) {
            return self.control.stop(Stop::InvalidMode);
        }
        if self.p == self.eof {
            self.control.token()?;
            return Ok(self.get_eof_token());
        }

        // Save our state in case we need to unread
        self.checkpoint = self.p;
        self.checkpoint_line = self.cur_line;
        self.checkpoint_last_newline = self.last_newline;

        let mut token_start = 0_i32;
        let mut token_start_line = 0_i32;
        let mut token_start_col = 0_i32;
        let mut token_end = 0_i32;
        let mut token_end_line = 0_i32;
        let mut token_end_col = 0_i32;

        let error = {
            scan(
                self.data,
                mode,
                &mut self.p,
                self.ps,
                self.pe,
                self.eof,
                &mut self.last_newline,
                &mut self.cur_line,
                &mut self.token,
                &mut token_start,
                &mut token_start_line,
                &mut token_start_col,
                &mut token_end,
                &mut token_end_line,
                &mut token_end_col,
                &mut self.control,
            )?
        };
        let (kind, start, end, start_pos, end_pos) = if error != 0 {
            let Some(tail) = usize::try_from(token_start)
                .ok()
                .and_then(|start| self.data.get(start..))
            else {
                return self.control.stop(Stop::InvalidPosition);
            };
            let Some(character) = str::from_utf8(tail)
                .ok()
                .and_then(|tail| tail.chars().next())
            else {
                return self.control.stop(Stop::InvalidPosition);
            };
            self.p = self.ps + token_start + character.len_utf8() as i32;
            (
                TokenType::Illegal,
                token_start,
                token_start + character.len_utf8() as i32,
                Position {
                    line: token_start_line as u32,
                    column: token_start_col as u32,
                },
                Position {
                    line: token_start_line as u32,
                    column: (token_start_col + character.len_utf8() as i32) as u32,
                },
            )
        } else if self.token == TokenType::Illegal && self.p == self.eof {
            let eof = self.get_eof_token();
            (
                TokenType::Eof,
                self.eof,
                self.eof,
                eof.start_pos,
                eof.end_pos,
            )
        } else {
            (
                self.token,
                token_start,
                token_end,
                Position {
                    line: token_start_line as u32,
                    column: token_start_col as u32,
                },
                Position {
                    line: token_end_line as u32,
                    column: token_end_col as u32,
                },
            )
        };
        if start < 0
            || end < start
            || start_pos.line == 0
            || start_pos.column == 0
            || end_pos.line == 0
            || end_pos.column == 0
        {
            return self.control.stop(Stop::InvalidPosition);
        }
        let Some(literal) = self
            .data
            .get(start as usize..end as usize)
            .and_then(|bytes| str::from_utf8(bytes).ok())
        else {
            return self.control.stop(Stop::InvalidPosition);
        };
        self.control.token()?;
        if kind == TokenType::Comment {
            self.control.comment()?;
            if self.comments.try_reserve(1).is_err() {
                return self.control.stop(Stop::Allocation);
            }
        }
        let additions = usize::from(!self.positions.contains_key(&start_pos))
            + usize::from(end_pos != start_pos && !self.positions.contains_key(&end_pos));
        if self
            .positions
            .len()
            .checked_add(additions)
            .is_none_or(|value| value > self.control.limits.positions)
        {
            return self.control.stop(Stop::Positions);
        }
        if self.positions.try_reserve(additions).is_err() {
            return self.control.stop(Stop::Allocation);
        }
        let text = copy_text(literal, &mut self.control)?;
        let t = Token {
            tok: kind,
            lit: text,
            start_offset: start as u32,
            end_offset: end as u32,
            start_pos,
            end_pos,
            comments: Vec::new(),
        };
        self.positions.insert(t.start_pos, t.start_offset);
        self.positions.insert(t.end_pos, t.end_offset);

        Ok(t)
    }

    fn get_eof_token(&self) -> Token {
        let data_len = self.data.len() as u32;
        let column = self.eof as u32 - self.last_newline as u32 + 1;
        Token {
            tok: TokenType::Eof,
            lit: String::from(""),
            start_offset: data_len,
            end_offset: data_len,
            start_pos: Position {
                line: self.cur_line as u32,
                column,
            },
            end_pos: Position {
                line: self.cur_line as u32,
                column,
            },
            comments: vec![],
        }
    }

    /// Produces the next comment token from the input.
    pub fn scan_with_comments(&mut self, mode: i32) -> Result<Token, Stop> {
        let mut token;
        loop {
            token = self._scan(mode)?;
            if token.tok != TokenType::Comment {
                break;
            }
            self.comments.push(Comment { text: token.lit });
        }
        if token.comments.try_reserve(self.comments.len()).is_err() {
            return self.control.stop(Stop::Allocation);
        }
        token.comments.append(&mut self.comments);
        Ok(token)
    }

    /// Produces the next token from the input.
    pub fn scan(&mut self) -> Result<Token, Stop> {
        self.scan_with_comments(0)
    }

    /// Produces the next token from the input accounting for regex.
    pub fn scan_with_regex(&mut self) -> Result<Token, Stop> {
        self.scan_with_comments(1)
    }

    /// Produces the next token from the input in a string expression.
    pub fn scan_string_expr(&mut self) -> Result<Token, Stop> {
        self.scan_with_comments(2)
    }

    /// `unread` will reset the [`Scanner`] to go back to the location
    /// before the last `scan_with_regex` or `scan` call. If either of the `scan_with_regex` methods
    /// returned an EOF token, a call to `unread` will not unread the discarded whitespace.
    /// This method is a no-op if called multiple times.
    pub fn unread(&mut self) {
        self.p = self.checkpoint;
        self.cur_line = self.checkpoint_line;
        self.last_newline = self.checkpoint_last_newline;
    }

    /// Get the offset of a position.
    pub fn offset(&self, pos: &Position) -> Option<u32> {
        self.positions.get(pos).copied()
    }

    pub(crate) fn control(&mut self) -> &mut Control<'c> { &mut self.control }
    pub(crate) fn fail<T>(&mut self, stop: Stop) -> Result<T, Stop> { self.control.stop(stop) }
    pub(crate) fn copy_string(&mut self, value: &str) -> Result<String, Stop> { copy_text(value, &mut self.control) }
    pub(crate) fn check(&mut self) -> Result<(), Stop> { self.control.check() }
    pub(crate) fn step(&mut self) -> Result<(), Stop> { self.control.parser_step() }
    pub(crate) fn enter(&mut self) -> Result<(), Stop> { self.control.enter() }
    pub(crate) fn leave(&mut self) -> Result<(), Stop> { self.control.leave() }
    pub(crate) fn node(&mut self) -> Result<(), Stop> { self.control.node() }
    pub(crate) fn checked_offset(&mut self, position: &Position) -> Result<u32, Stop> {
        self.control.check()?;
        match self.positions.get(position).copied() {
            Some(offset) => Ok(offset), None => self.control.stop(Stop::InvalidPosition),
        }
    }
    pub(crate) fn set_comments(&mut self, comments: &mut Vec<Comment>) -> Result<(), Stop> {
        self.control.check()?;
        if self.comments.try_reserve(comments.len()).is_err() { return self.control.stop(Stop::Allocation); }
        self.comments.append(comments);
        Ok(())
    }

    pub fn progress(&self) -> crate::control::Progress {
        self.control.progress()
    }
}

fn copy_text(value: &str, control: &mut Control<'_>) -> Result<String, Stop> {
    control.check()?;
    let mut result = String::new();
    if result.try_reserve_exact(value.len()).is_err() {
        return control.stop(Stop::Allocation);
    }
    result.push_str(value);
    Ok(result)
}
