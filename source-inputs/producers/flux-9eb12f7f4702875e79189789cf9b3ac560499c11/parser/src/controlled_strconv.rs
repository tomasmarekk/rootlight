//! Controlled version of the pinned Flux literal conversions.
//!
//! Regex validation is deliberately unavailable until its standard validator can stop.

use std::{iter::Peekable, str::Chars};
use chrono::{prelude::*, FixedOffset};
use crate::{ast, control::{Control, Stop}};

enum Error { Syntax(String), Control(Stop) }
impl From<String> for Error { fn from(value: String) -> Self { Self::Syntax(value) } }
impl From<Stop> for Error { fn from(value: Stop) -> Self { Self::Control(value) } }

fn finish<T>(value: Result<T, Error>) -> Result<Result<T, String>, Stop> {
    match value { Ok(value) => Ok(Ok(value)), Err(Error::Syntax(error)) => Ok(Err(error)), Err(Error::Control(stop)) => Err(stop) }
}

fn allocate(control: &mut Control<'_>, bytes: usize) -> Result<Vec<u8>, Error> {
    control.check()?;
    let mut result = Vec::new();
    if result.try_reserve_exact(bytes).is_err() { return Err(Error::Control(control.stop_reason(Stop::Allocation))); }
    Ok(result)
}

fn text_inner(lit: &str, control: &mut Control<'_>) -> Result<String, Error> {
    let mut result = allocate(control, lit.len())?;
    let mut chars = lit.chars();
    loop {
        control.conversion()?;
        let Some(character) = chars.next() else { break };
        match character {
            '\\' => unescape(&mut result, &mut chars, control)?,
            _ => { let mut encoded = [0;4]; result.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes()); }
        }
    }
    // Each escape emits no more bytes than its exact source spelling, so the reserved capacity suffices.
    String::from_utf8(result).map_err(|error| Error::Syntax(error.utf8_error().to_string()))
}

pub fn parse_text(lit: &str, control: &mut Control<'_>) -> Result<Result<String, String>, Stop> {
    finish(text_inner(lit, control))
}

pub fn parse_string(lit: &str, control: &mut Control<'_>) -> Result<Result<String, String>, Stop> {
    control.check()?;
    if lit.len() < 2 || !lit.starts_with('"') || !lit.ends_with('"') { return Ok(Err("invalid string literal".to_owned())); }
    finish(text_inner(&lit[1..lit.len()-1], control))
}

fn next(chars: &mut Chars<'_>, control: &mut Control<'_>) -> Result<Option<char>, Error> {
    control.conversion()?;
    Ok(chars.next())
}

fn unescape(result: &mut Vec<u8>, chars: &mut Chars<'_>, control: &mut Control<'_>) -> Result<(), Error> {
    match next(chars, control)? {
        Some(character) => match character {
            'n' => result.push(b'\n'), 'r' => result.push(b'\r'), 't' => result.push(b'\t'),
            '\\' => result.push(b'\\'), '"' => result.push(b'"'), '$' => result.push(b'$'),
            'x' => hex_byte(result, chars, control)?,
            _ => return Err(format!("invalid escape character {character}").into()),
        },
        None => return Err("invalid escape sequence".to_owned().into()),
    }
    Ok(())
}

fn hex_byte(result: &mut Vec<u8>, chars: &mut Chars<'_>, control: &mut Control<'_>) -> Result<(), Error> {
    let first = next(chars, control)?.ok_or_else(|| Error::Syntax(r"\x followed by 0 char, must be 2".to_owned()))?;
    let second = next(chars, control)?.ok_or_else(|| Error::Syntax(r"\x followed by 1 char, must be 2".to_owned()))?;
    let byte = |character| match character { '0'..='9' => Some(character as u8-b'0'), 'a'..='f' => Some(character as u8-b'a'+10), 'A'..='F' => Some(character as u8-b'A'+10), _ => None };
    let (Some(first), Some(second)) = (byte(first), byte(second)) else { return Err("invalid byte value".to_owned().into()) };
    result.push((first << 4) | second);
    Ok(())
}

pub fn parse_regex(_: &str, control: &mut Control<'_>) -> Result<Result<String, String>, Stop> {
    control.check()?;
    control.stop(Stop::UnsupportedRegexValidation)
}

pub fn parse_time(lit: &str, control: &mut Control<'_>) -> Result<Result<DateTime<FixedOffset>, String>, Stop> {
    control.check()?;
    // Calendar/date-time validation is a fixed-size leaf for the supported nanosecond-precision spelling.
    // Longer native fractional spellings need a separately controlled standard conversion.
    if lit.len() > 35 { return control.stop(Stop::UnsupportedTimePrecision); }
    for _ in lit.bytes() { control.conversion()?; }
    let result = if !lit.contains('T') {
        NaiveDate::parse_from_str(lit, "%Y-%m-%d").map(|date| {
            let offset = FixedOffset::east_opt(0);
            let time = NaiveTime::from_hms_opt(0,0,0);
            match (offset,time) { (Some(offset),Some(time)) => Ok(DateTime::from_naive_utc_and_offset(date.and_time(time),offset)), _ => Err(Stop::Invariant) }
        }).map_err(|error| error.to_string())
    } else { DateTime::parse_from_rfc3339(lit).map(Ok).map_err(|error|error.to_string()) };
    control.check()?;
    match result { Ok(Ok(value)) => Ok(Ok(value)), Ok(Err(stop)) => control.stop(stop), Err(error) => Ok(Err(error)) }
}

pub fn parse_duration(lit: &str, control: &mut Control<'_>) -> Result<Result<Vec<ast::Duration>, String>, Stop> {
    finish(duration_inner(lit, control))
}

fn duration_inner(lit: &str, control: &mut Control<'_>) -> Result<Vec<ast::Duration>, Error> {
    let mut result = Vec::new();
    let mut chars = lit.chars().peekable();
    loop {
        control.conversion()?;
        if chars.peek().is_none() { break; }
        let magnitude = magnitude(&mut chars, control)?;
        let unit = unit(&mut chars, control)?;
        control.node()?;
        if result.try_reserve(1).is_err() { return Err(Error::Control(control.stop_reason(Stop::Allocation))); }
        result.push(ast::Duration { magnitude, unit });
    }
    Ok(result)
}

fn push(result: &mut String, character: char, control: &mut Control<'_>) -> Result<(), Error> {
    control.check()?;
    if result.try_reserve(character.len_utf8()).is_err() { return Err(Error::Control(control.stop_reason(Stop::Allocation))); }
    result.push(character);
    Ok(())
}

fn magnitude(chars: &mut Peekable<Chars<'_>>, control: &mut Control<'_>) -> Result<i64, Error> {
    let mut value = String::new();
    loop {
        control.conversion()?;
        let Some(character) = chars.peek() else { break };
        if !character.is_ascii_digit() { break; }
        push(&mut value, *character, control)?;
        chars.next();
    }
    if value.is_empty() { return Err("parsing empty magnitude".to_owned().into()); }
    value.parse::<i64>().map_err(|error|Error::Syntax(error.to_string()))
}

fn unit(chars: &mut Peekable<Chars<'_>>, control: &mut Control<'_>) -> Result<String, Error> {
    let mut value = String::new();
    loop {
        control.conversion()?;
        let Some(character) = chars.peek() else { break };
        if !character.is_alphabetic() { break; }
        push(&mut value, *character, control)?;
        chars.next();
    }
    if value.is_empty() { return Err("parsing empty unit".to_owned().into()); }
    if value == "µs" { value.clear(); value.push_str("us"); }
    Ok(value)
}
