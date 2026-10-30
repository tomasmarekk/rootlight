//! Decode PWCT visual-source tables without interpreting their Ring host.
//! Written identifiers and literal origins belong to the visual source; executable
//! meaning of code fields is deliberately outside this source-model contract.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

const MAX_MODEL_VALUES: usize = 2_097_152;

/// Caller-owned ceilings for one inert PWCT decode.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum physical input byte length.
    pub source_bytes: usize,
    /// Maximum number of literal values across all assignments.
    pub values: usize,
    /// Maximum nested literal depth; values greater than 64 are refused.
    pub depth: usize,
    /// Maximum cumulative decoded UTF-8 bytes across every text field.
    pub decoded_bytes: usize,
    /// Maximum number of visual step rows.
    pub steps: usize,
    /// Maximum number of interaction rows.
    pub interactions: usize,
    /// Maximum parser and graph-validation checkpoints.
    pub work: usize,
}

impl Limits {
    /// Derives identical native decode budgets for a provider and an independent consumer.
    ///
    /// Source and syntax-node ceilings remain caller-owned. Literal values, steps
    /// and interactions share the existing 2 Mi value ceiling; depth is capped at
    /// 64 and checked work is eight times source bytes plus admitted values.
    ///
    /// # Errors
    /// Returns a limit refusal when the checked work formula overflows.
    pub fn for_parse(
        max_source_bytes: usize,
        max_syntax_nodes: usize,
        max_syntax_depth: usize,
    ) -> Result<Self, Error> {
        let values = max_syntax_nodes.min(MAX_MODEL_VALUES);
        let work = max_source_bytes
            .checked_mul(8)
            .and_then(|work| work.checked_add(values))
            .ok_or(Error {
                kind: ErrorKind::Limit("work"),
                span: 0..0,
            })?;
        Ok(Self {
            source_bytes: max_source_bytes,
            values,
            depth: max_syntax_depth.min(64),
            decoded_bytes: max_source_bytes,
            steps: values,
            interactions: values,
            work,
        })
    }
}

/// Terminal refusal; malformed data and unsupported syntax remain distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Invalid data in the supported serialization grammar or schema.
    Malformed(&'static str),
    /// A construct outside the closed inert grammar or schema version.
    Unsupported(&'static str),
    /// A caller-owned resource ceiling was exceeded.
    Limit(&'static str),
    /// Cooperative cancellation stopped the decode.
    Cancelled(Cancelled),
    /// A bounded reservation could not be allocated.
    Allocation,
}

/// A source-local refusal, with no partial graph presented as a valid model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Source-free refusal category and stable detail code.
    pub kind: ErrorKind,
    /// Half-open physical byte range associated with the refusal.
    pub span: Range<usize>,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PWCT source {:?} at {}..{}",
            self.kind, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Cancelled(error) => Some(error),
            _ => None,
        }
    }
}

/// Interpretation of one literal origin segment, not an offset-equivalence claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOrigin {
    /// Written literal body whose UTF-8 bytes equal the decoded segment.
    Literal,
    /// The complete inert `char(34)` expression contributing one quote.
    Character,
}

/// Exact physical origin of a contiguous decoded-text segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSegment {
    /// Half-open byte range in the decoded text.
    pub decoded: Range<usize>,
    /// Half-open byte range in the physical input.
    pub source: Range<usize>,
    /// Whether the origin is a literal body or a character constructor.
    pub origin: TextOrigin,
}

/// A closed literal concatenation with source-local segment provenance.
#[derive(Debug, PartialEq, Eq)]
pub struct Text {
    /// Concatenated literal value, without runtime interpolation.
    pub text: String,
    /// Physical extent of the complete literal expression, including quotes.
    pub span: Range<usize>,
    /// Ordered physical origins covering the decoded text without overlap.
    pub segments: Vec<TextSegment>,
}

/// One inert source literal with its complete physical extent.
#[derive(Debug)]
pub struct Value {
    /// Half-open physical byte range of the literal expression.
    pub span: Range<usize>,
    /// Decoded literal payload; no executable expression is retained here.
    pub kind: ValueKind,
}

/// The complete literal grammar accepted for PWCT serialized data.
#[derive(Debug)]
pub enum ValueKind {
    /// An unsigned decimal integer with checked arithmetic.
    Integer(u64),
    /// A closed string/quote-constructor concatenation.
    Text(Text),
    /// An ordered literal list, preserving repeated data until schema validation.
    List(Vec<Value>),
}

struct Parser<'a> {
    source: &'a [u8],
    position: usize,
    limits: Limits,
    values: usize,
    maximum_depth: usize,
    decoded: usize,
    work: usize,
    cancel: &'a Cancellation,
}

impl<'a> Parser<'a> {
    fn new(source: &'a [u8], limits: Limits, cancel: &'a Cancellation) -> Result<Self, Error> {
        cancel.check().map_err(|error| Error {
            kind: ErrorKind::Cancelled(error),
            span: 0..0,
        })?;
        if source.len() > limits.source_bytes || limits.depth > 64 {
            return Err(Error {
                kind: ErrorKind::Limit("source-or-depth"),
                span: 0..source.len(),
            });
        }
        std::str::from_utf8(source).map_err(|error| Error {
            kind: ErrorKind::Malformed("utf8"),
            span: error.valid_up_to()..error.valid_up_to(),
        })?;
        cancel.check().map_err(|error| Error {
            kind: ErrorKind::Cancelled(error),
            span: 0..0,
        })?;
        Ok(Self {
            source,
            position: usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3,
            limits,
            values: 0,
            maximum_depth: 0,
            decoded: 0,
            work: 0,
            cancel,
        })
    }

    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            kind,
            span: self.position..self.position.saturating_add(1).min(self.source.len()),
        }
    }

    fn checkpoint(&mut self) -> Result<(), Error> {
        self.cancel
            .check()
            .map_err(|error| self.error(ErrorKind::Cancelled(error)))?;
        self.work = self
            .work
            .checked_add(1)
            .filter(|work| *work <= self.limits.work)
            .ok_or_else(|| self.error(ErrorKind::Limit("work")))?;
        Ok(())
    }

    fn byte(&self) -> Option<u8> {
        self.source.get(self.position).copied()
    }

    fn advance(&mut self) -> Result<(), Error> {
        self.checkpoint()?;
        self.position += 1;
        Ok(())
    }

    fn whitespace(&mut self) -> Result<(), Error> {
        loop {
            match self.byte() {
                Some(byte) if byte.is_ascii_whitespace() => self.advance()?,
                Some(b'#') => {
                    while self.byte().is_some_and(|byte| byte != b'\n') {
                        self.advance()?;
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        self.whitespace()?;
        if self.byte() != Some(byte) {
            return Err(self.error(ErrorKind::Malformed("delimiter")));
        }
        self.advance()
    }

    fn identifier(&mut self) -> Result<Range<usize>, Error> {
        self.whitespace()?;
        let start = self.position;
        if !self.byte().is_some_and(|byte| byte.is_ascii_alphabetic()) {
            return Err(self.error(ErrorKind::Unsupported("statement")));
        }
        while self
            .byte()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.advance()?;
        }
        Ok(start..self.position)
    }

    fn integer(&mut self) -> Result<u64, Error> {
        self.whitespace()?;
        let start = self.position;
        let mut number = 0_u64;
        while let Some(byte) = self.byte().filter(u8::is_ascii_digit) {
            number = number
                .checked_mul(10)
                .and_then(|number| number.checked_add(u64::from(byte - b'0')))
                .ok_or_else(|| self.error(ErrorKind::Malformed("integer-overflow")))?;
            self.advance()?;
        }
        if start == self.position {
            return Err(self.error(ErrorKind::Malformed("integer")));
        }
        Ok(number)
    }

    fn append(&mut self, output: &mut String, text: &str) -> Result<(), Error> {
        self.decoded = self
            .decoded
            .checked_add(text.len())
            .filter(|bytes| *bytes <= self.limits.decoded_bytes)
            .ok_or_else(|| self.error(ErrorKind::Limit("decoded-bytes")))?;
        output
            .try_reserve(text.len())
            .map_err(|_| self.error(ErrorKind::Allocation))?;
        output.push_str(text);
        Ok(())
    }

    fn text(&mut self) -> Result<Text, Error> {
        self.whitespace()?;
        let start = self.position;
        let mut output = Text {
            text: String::new(),
            span: start..start,
            segments: Vec::new(),
        };
        loop {
            self.checkpoint()?;
            let decoded_start = output.text.len();
            let (source, origin) = if let Some(quote @ (b'\'' | b'"' | b'`')) = self.byte() {
                self.advance()?;
                let body = self.position;
                while let Some(byte) = self.byte() {
                    if byte == quote {
                        break;
                    }
                    // Dynamic Ring interpolation is not a literal value.
                    if byte == b'#' && self.source.get(self.position + 1) == Some(&b'{') {
                        return Err(self.error(ErrorKind::Unsupported("interpolation")));
                    }
                    self.advance()?;
                }
                if self.byte() != Some(quote) {
                    return Err(self.error(ErrorKind::Malformed("unterminated-string")));
                }
                let range = body..self.position;
                let text = std::str::from_utf8(
                    self.source
                        .get(range.clone())
                        .ok_or_else(|| self.error(ErrorKind::Malformed("string-span")))?,
                )
                .map_err(|_| self.error(ErrorKind::Malformed("utf8")))?;
                self.append(&mut output.text, text)?;
                self.advance()?;
                (range, TextOrigin::Literal)
            } else {
                let constructor = self.identifier()?;
                if !self
                    .source
                    .get(constructor.clone())
                    .is_some_and(|name| name.eq_ignore_ascii_case(b"char"))
                {
                    return Err(self.error(ErrorKind::Unsupported("literal-constructor")));
                }
                self.expect(b'(')?;
                self.whitespace()?;
                if !self.byte().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(self.error(ErrorKind::Unsupported("character-argument")));
                }
                let character = self.integer()?;
                self.expect(b')')?;
                // PWCT's serializer spells an embedded quote as char(34).
                // Other calls are not licensed by recognizing this one form.
                if character != 34 {
                    return Err(self.error(ErrorKind::Unsupported("character-constructor")));
                }
                self.append(&mut output.text, "\"")?;
                (constructor.start..self.position, TextOrigin::Character)
            };
            if output.segments.len() >= self.limits.values {
                return Err(self.error(ErrorKind::Limit("text-segments")));
            }
            output
                .segments
                .try_reserve(1)
                .map_err(|_| self.error(ErrorKind::Allocation))?;
            output.segments.push(TextSegment {
                decoded: decoded_start..output.text.len(),
                source,
                origin,
            });
            output.span.end = self.position;
            self.whitespace()?;
            if self.byte() != Some(b'+') {
                return Ok(output);
            }
            self.advance()?;
            self.whitespace()?;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.checkpoint()?;
        self.whitespace()?;
        if depth > self.limits.depth || self.values >= self.limits.values {
            return Err(self.error(ErrorKind::Limit("values-or-depth")));
        }
        self.values += 1;
        self.maximum_depth = self.maximum_depth.max(depth);
        let start = self.position;
        let kind = match self.byte() {
            Some(b'[') => {
                self.advance()?;
                let mut values = Vec::new();
                self.whitespace()?;
                while self.byte() != Some(b']') {
                    let value = self.value(depth + 1)?;
                    values
                        .try_reserve(1)
                        .map_err(|_| self.error(ErrorKind::Allocation))?;
                    values.push(value);
                    self.whitespace()?;
                    match self.byte() {
                        Some(b',') => self.advance()?,
                        Some(b']') => break,
                        _ => return Err(self.error(ErrorKind::Malformed("list-delimiter"))),
                    }
                    self.whitespace()?;
                }
                self.advance()?;
                ValueKind::List(values)
            }
            Some(byte) if byte.is_ascii_digit() => ValueKind::Integer(self.integer()?),
            Some(b'\'' | b'"' | b'`' | b'c' | b'C') => ValueKind::Text(self.text()?),
            None => return Err(self.error(ErrorKind::Malformed("unexpected-eof"))),
            _ => return Err(self.error(ErrorKind::Unsupported("value-expression"))),
        };
        let end = match &kind {
            ValueKind::Text(text) => text.span.end,
            _ => self.position,
        };
        Ok(Value {
            span: start..end,
            kind,
        })
    }
}

/// A visual node identifier, independent of row position or display caption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StepId(#[doc = "The positive source-written numeric identifier."] pub u64);

/// The visual parent reference; zero denotes the file-owned root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParentId {
    /// Written zero, denoting the visual tree's file-owned root.
    Root,
    /// A written reference to another validated visual step.
    Step(StepId),
}

/// Identifier of a serialized interaction record, not a callable target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InteractionId(#[doc = "The positive source-written numeric identifier."] pub u64);

/// A source-written property pair retained without runtime interpretation.
#[derive(Debug)]
pub struct Property {
    /// Physical extent of the complete two-element property row.
    pub span: Range<usize>,
    /// Literal written key and its physical origins.
    pub name: Text,
    /// Inert value whose exact expression extent remains available.
    pub value: Value,
}

/// A validated visual data node with exact primary-key and parent references.
#[derive(Debug)]
pub struct Step {
    /// Source-written visual primary key.
    pub id: StepId,
    /// Physical extent of the written primary-key token only.
    pub id_span: Range<usize>,
    /// Validated semantic visual parent, independently of physical nesting.
    pub parent: ParentId,
    /// Physical extent of the written parent-ID token only.
    pub parent_span: Range<usize>,
    /// Index of the validated parent in `Model::steps`, or the file root.
    pub parent_index: Option<usize>,
    /// Physical extent of this complete literal step row.
    pub span: Range<usize>,
    /// Written property rows in physical order.
    pub properties: Vec<Property>,
    /// Source-written interaction reference from the content properties.
    pub interaction: InteractionId,
    /// Index of its validated target in `Model::interactions`.
    pub interaction_index: usize,
    /// Written active flag; it does not establish executable semantics.
    pub active: bool,
    /// Written visible flag; hidden visual data remains in this model.
    pub visible: bool,
    /// Written PWCT step type, restricted to the supported values 1 through 4.
    pub step_type: u64,
    code_index: usize,
}

impl Step {
    /// Returns the decoded code field and every literal origin, without parsing Ring.
    pub fn code(&self) -> Option<&Text> {
        match &self.properties.get(self.code_index)?.value.kind {
            ValueKind::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// A joined interaction row; component and variable text carry no call semantics.
#[derive(Debug)]
pub struct Interaction {
    /// Source-written interaction primary key.
    pub id: InteractionId,
    /// Physical extent of its primary-key token only.
    pub id_span: Range<usize>,
    /// Physical extent of the complete six-element interaction row.
    pub span: Range<usize>,
    /// Written interaction type, restricted to supported values 1 and 2.
    pub kind: u64,
    /// Literal component name, without a callable-target inference.
    pub component: Text,
    /// Literal date metadata, without calendar interpretation.
    pub date: Text,
    /// Literal time metadata, without clock interpretation.
    pub time: Text,
    /// Literal variable payload; its delimiter protocol is not evaluated.
    pub variables: Text,
}

/// A closed visual source model, not a claim of executable Ring semantics.
#[derive(Debug)]
pub struct Model {
    /// Literal syntax nodes observed before the required record projection.
    pub literal_values: usize,
    /// Deepest literal nesting observed, independently of emitted fact depth.
    pub literal_depth: usize,
    /// Written natural-language metadata, without caption-based classification.
    pub natural_language: Text,
    /// Written high-water counter; it is not the number of live rows.
    pub step_counter: u64,
    /// Written interaction high-water counter, independent of row count.
    pub interaction_counter: u64,
    /// Validated visual rows in physical source order.
    pub steps: Vec<Step>,
    /// Validated interaction rows in physical source order.
    pub interactions: Vec<Interaction>,
}

fn malformed(span: Range<usize>, code: &'static str) -> Error {
    Error {
        kind: ErrorKind::Malformed(code),
        span,
    }
}

fn list(value: Value) -> Result<Vec<Value>, Error> {
    match value.kind {
        ValueKind::List(values) => Ok(values),
        _ => Err(malformed(value.span, "expected-list")),
    }
}

fn row<const N: usize>(value: Value) -> Result<[Value; N], Error> {
    let span = value.span.clone();
    list(value)?
        .try_into()
        .map_err(|_| malformed(span, "row-arity"))
}

fn number(value: &Value) -> Result<u64, Error> {
    match &value.kind {
        ValueKind::Integer(number) => Ok(*number),
        _ => Err(malformed(value.span.clone(), "expected-integer")),
    }
}

fn text(value: Value) -> Result<Text, Error> {
    match value.kind {
        ValueKind::Text(text) => Ok(text),
        _ => Err(malformed(value.span, "expected-text")),
    }
}

fn push<T>(values: &mut Vec<T>, value: T, span: Range<usize>) -> Result<(), Error> {
    values.try_reserve(1).map_err(|_| Error {
        kind: ErrorKind::Allocation,
        span,
    })?;
    values.push(value);
    Ok(())
}

fn property<'a>(
    properties: &'a [Property],
    name: &str,
    span: &Range<usize>,
) -> Result<(usize, &'a Value), Error> {
    properties
        .iter()
        .enumerate()
        .find_map(|(index, property)| {
            (property.name.text == name).then_some((index, &property.value))
        })
        .ok_or_else(|| malformed(span.clone(), "missing-property"))
}

fn flag(properties: &[Property], name: &str, span: &Range<usize>) -> Result<bool, Error> {
    match number(property(properties, name, span)?.1)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(malformed(span.clone(), "boolean-property")),
    }
}

fn steps(value: Value, parser: &mut Parser<'_>) -> Result<Vec<Step>, Error> {
    let mut result = Vec::new();
    for value in list(value)? {
        parser.checkpoint()?;
        if result.len() >= parser.limits.steps {
            return Err(parser.error(ErrorKind::Limit("steps")));
        }
        let span = value.span.clone();
        let [id, parent, content] = row(value)?;
        let id_number = number(&id)?;
        if id_number == 0 {
            return Err(malformed(id.span, "zero-step-id"));
        }
        let parent_number = number(&parent)?;
        let mut properties: Vec<Property> = Vec::new();
        for pair in list(content)? {
            parser.checkpoint()?;
            if properties.len() >= 64 {
                return Err(parser.error(ErrorKind::Limit("step-properties")));
            }
            let pair_span = pair.span.clone();
            let [name, value] = row(pair)?;
            let name = text(name)?;
            if properties
                .iter()
                .any(|property| property.name.text == name.text)
            {
                return Err(malformed(name.span, "duplicate-property"));
            }
            push(
                &mut properties,
                Property {
                    span: pair_span.clone(),
                    name,
                    value,
                },
                pair_span,
            )?;
        }
        let (_, name) = property(&properties, "name", &span)?;
        if !matches!(name.kind, ValueKind::Text(_)) {
            return Err(malformed(name.span.clone(), "step-name"));
        }
        if let Some(plain_name) = properties
            .iter()
            .find(|property| property.name.text == "plainname")
            && !matches!(plain_name.value.kind, ValueKind::Text(_))
        {
            return Err(malformed(plain_name.value.span.clone(), "step-plainname"));
        }
        let (code_index, code) = property(&properties, "code", &span)?;
        if !matches!(code.kind, ValueKind::Text(_)) {
            return Err(malformed(code.span.clone(), "step-code"));
        }
        let interaction = number(property(&properties, "interactionid", &span)?.1)?;
        if interaction == 0 {
            return Err(malformed(span, "zero-interaction-reference"));
        }
        let active = flag(&properties, "active", &span)?;
        let visible = flag(&properties, "visible", &span)?;
        number(property(&properties, "stepnumber", &span)?.1)?;
        let step_type = number(property(&properties, "steptype", &span)?.1)?;
        if !(1..=4).contains(&step_type) {
            return Err(Error {
                kind: ErrorKind::Unsupported("step-type"),
                span,
            });
        }
        let step = Step {
            id: StepId(id_number),
            id_span: id.span,
            parent: if parent_number == 0 {
                ParentId::Root
            } else {
                ParentId::Step(StepId(parent_number))
            },
            parent_span: parent.span,
            parent_index: None,
            span: span.clone(),
            properties,
            interaction: InteractionId(interaction),
            interaction_index: 0,
            active,
            visible,
            step_type,
            code_index,
        };
        push(&mut result, step, span)?;
    }
    Ok(result)
}

fn interactions(value: Value, parser: &mut Parser<'_>) -> Result<Vec<Interaction>, Error> {
    let mut result = Vec::new();
    for value in list(value)? {
        parser.checkpoint()?;
        if result.len() >= parser.limits.interactions {
            return Err(parser.error(ErrorKind::Limit("interactions")));
        }
        let span = value.span.clone();
        let [id, kind, component, date, time, variables] = row(value)?;
        let id_number = number(&id)?;
        if id_number == 0 {
            return Err(malformed(id.span, "zero-interaction-id"));
        }
        let kind_number = number(&kind)?;
        if !(1..=2).contains(&kind_number) {
            return Err(Error {
                kind: ErrorKind::Unsupported("interaction-type"),
                span,
            });
        }
        let interaction = Interaction {
            id: InteractionId(id_number),
            id_span: id.span,
            span: span.clone(),
            kind: kind_number,
            component: text(component)?,
            date: text(date)?,
            time: text(time)?,
            variables: text(variables)?,
        };
        push(&mut result, interaction, span)?;
    }
    Ok(result)
}

fn join(model: &mut Model, parser: &mut Parser<'_>) -> Result<(), Error> {
    let mut step_ids = Vec::new();
    let mut interaction_ids = Vec::new();
    for (index, step) in model.steps.iter().enumerate() {
        parser.checkpoint()?;
        if step.id.0 > model.step_counter {
            return Err(malformed(step.id_span.clone(), "step-counter"));
        }
        push(&mut step_ids, (step.id, index), step.id_span.clone())?;
    }
    for (index, interaction) in model.interactions.iter().enumerate() {
        parser.checkpoint()?;
        if interaction.id.0 > model.interaction_counter {
            return Err(malformed(
                interaction.id_span.clone(),
                "interaction-counter",
            ));
        }
        push(
            &mut interaction_ids,
            (interaction.id, index),
            interaction.id_span.clone(),
        )?;
    }
    step_ids.sort_unstable_by_key(|(id, _)| *id);
    interaction_ids.sort_unstable_by_key(|(id, _)| *id);
    if step_ids.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(parser.error(ErrorKind::Malformed("duplicate-step-id")));
    }
    if interaction_ids
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(parser.error(ErrorKind::Malformed("duplicate-interaction-id")));
    }
    for step in &mut model.steps {
        parser.checkpoint()?;
        if let ParentId::Step(parent) = step.parent {
            let index = step_ids
                .binary_search_by_key(&parent, |(id, _)| *id)
                .map_err(|_| malformed(step.parent_span.clone(), "missing-parent"))?;
            step.parent_index = Some(step_ids[index].1);
        }
        let index = interaction_ids
            .binary_search_by_key(&step.interaction, |(id, _)| *id)
            .map_err(|_| malformed(step.span.clone(), "missing-interaction"))?;
        step.interaction_index = interaction_ids[index].1;
    }
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(model.steps.len())
        .map_err(|_| parser.error(ErrorKind::Allocation))?;
    colors.resize(model.steps.len(), 0_u8);
    for start in 0..model.steps.len() {
        if colors[start] != 0 {
            continue;
        }
        let mut current = Some(start);
        while let Some(index) = current {
            parser.checkpoint()?;
            match colors[index] {
                1 => {
                    return Err(malformed(
                        model.steps[index].parent_span.clone(),
                        "parent-cycle",
                    ));
                }
                2 => break,
                _ => colors[index] = 1,
            }
            current = model.steps[index].parent_index;
        }
        current = Some(start);
        while let Some(index) = current {
            parser.checkpoint()?;
            if colors[index] == 2 {
                break;
            }
            colors[index] = 2;
            current = model.steps[index].parent_index;
        }
    }
    Ok(())
}

/// Decodes the closed PWCT 1.0.0 RingPWCT serialization without repository execution.
///
/// # Errors
/// Returns a physical refusal for unsupported expressions/version/language, malformed
/// tables or joins, resource limits, allocation failure, or cooperative cancellation.
pub fn decode(source: &[u8], limits: Limits, cancel: &Cancellation) -> Result<Model, Error> {
    const NAMES: [&[u8]; 7] = [
        b"cVisualSourceFileVersion",
        b"cVisualLanguageName",
        b"cNaturalLanguageName",
        b"nStepsID",
        b"nInteractionsID",
        b"aStepsTable",
        b"aInteractionsTable",
    ];
    let mut parser = Parser::new(source, limits, cancel)?;
    let mut assignments: [Option<Value>; 7] = std::array::from_fn(|_| None);
    parser.whitespace()?;
    while parser.byte().is_some() {
        let name = parser.identifier()?;
        let name_bytes = source
            .get(name.clone())
            .ok_or_else(|| malformed(name.clone(), "assignment-span"))?;
        let index = NAMES
            .iter()
            .position(|known| known.eq_ignore_ascii_case(name_bytes))
            .ok_or_else(|| Error {
                kind: ErrorKind::Unsupported("assignment"),
                span: name.clone(),
            })?;
        if assignments[index].is_some() {
            return Err(malformed(name, "duplicate-assignment"));
        }
        parser.expect(b'=')?;
        let value = parser.value(0)?;
        let end = value.span.end;
        assignments[index] = Some(value);
        parser.whitespace()?;
        if parser.byte().is_some()
            && !source
                .get(end..parser.position)
                .is_some_and(|gap| gap.contains(&b'\n') || gap.contains(&b'\r'))
        {
            return Err(parser.error(ErrorKind::Malformed("assignment-separator")));
        }
    }
    let [
        version,
        language,
        natural_language,
        step_counter,
        interaction_counter,
        step_rows,
        interaction_rows,
    ] = assignments;
    let required = |value: Option<Value>| {
        value.ok_or_else(|| malformed(source.len()..source.len(), "missing-assignment"))
    };
    let version = text(required(version)?)?;
    if version.text != "1.0.0" {
        return Err(Error {
            kind: ErrorKind::Unsupported("visual-source-version"),
            span: version.span,
        });
    }
    let language = text(required(language)?)?;
    if language.text != "RingPWCT" {
        return Err(Error {
            kind: ErrorKind::Unsupported("visual-language"),
            span: language.span,
        });
    }
    let mut model = Model {
        literal_values: parser.values,
        literal_depth: parser.maximum_depth,
        natural_language: text(required(natural_language)?)?,
        step_counter: number(&required(step_counter)?)?,
        interaction_counter: number(&required(interaction_counter)?)?,
        steps: steps(required(step_rows)?, &mut parser)?,
        interactions: interactions(required(interaction_rows)?, &mut parser)?,
    };
    join(&mut model, &mut parser)?;
    parser.checkpoint()?;
    Ok(model)
}

/// One closed physical record role emitted by the native PWCT provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Full immutable source extent, with local identifier 1.
    Root,
    /// Complete literal row of one authored visual data node.
    Step,
    /// Exact written StepID token defining that visual node.
    Definition,
    /// Exact written ParentID token carrying a checked visual ownership proposal.
    Parent,
}

impl Role {
    /// Returns the exact PWCT-owned syntax label shared by producer and consumer.
    #[must_use]
    pub const fn syntax(self) -> &'static str {
        match self {
            Self::Root => "pwct.source_file",
            Self::Step => "pwct.step.declaration",
            Self::Definition => "pwct.step.definition",
            Self::Parent => "pwct.step.parent",
        }
    }

    /// Returns the parser-independent class; parent tokens do not supply signatures.
    #[must_use]
    pub const fn kind(self) -> crate::SyntaxFactKind {
        match self {
            Self::Root => crate::SyntaxFactKind::Root,
            Self::Step => crate::SyntaxFactKind::Declaration,
            Self::Definition => crate::SyntaxFactKind::Occurrence,
            Self::Parent => crate::SyntaxFactKind::Signature,
        }
    }

    /// Returns physical nesting depth, independently of the visual parent graph.
    #[must_use]
    pub const fn depth(self) -> usize {
        match self {
            Self::Root => 0,
            Self::Step => 1,
            Self::Definition | Self::Parent => 2,
        }
    }
}

/// A required parser-independent record with physical containment only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Checked sparse identity derived from the authored StepID, or 1 for Root.
    pub local_id: u64,
    /// Physical containing record; this never encodes a visual parent StepID.
    pub parent: Option<u64>,
    /// Authored visual primary key, absent only for Root.
    pub step: Option<StepId>,
    /// Closed role determining exact syntax label, class and physical depth.
    pub role: Role,
    /// Half-open physical input byte range.
    pub span: Range<usize>,
}

/// A deterministic source census for independent provider and lowerer validation.
///
/// This is data derived from one supplied byte slice, not a reusable authority:
/// consumers must recensus their authenticated request bytes before accepting facts.
#[derive(Debug)]
pub struct Plan {
    /// Complete validated visual source model in physical row order.
    pub model: Model,
    /// Root followed by each row's declaration, Definition and parent token.
    pub records: Vec<Record>,
}

/// Builds the complete required physical record census without sparse-ID allocation.
///
/// `maximum_records` is checked against the full required count before allocation.
/// A row owns three checked local IDs starting at `2 + 3 * (StepID - 1)`;
/// neither source order, display text nor visual parentage participates in these IDs.
///
/// # Errors
/// Returns decode refusals, record/identifier overflow, allocation failure, or
/// cancellation. No partial required census is returned when a ceiling is reached.
pub fn plan(
    source: &[u8],
    limits: Limits,
    maximum_records: usize,
    cancel: &Cancellation,
) -> Result<Plan, Error> {
    let model = decode(source, limits, cancel)?;
    let count = model
        .steps
        .len()
        .checked_mul(3)
        .and_then(|count| count.checked_add(1))
        .filter(|count| *count <= maximum_records)
        .ok_or(Error {
            kind: ErrorKind::Limit("records"),
            span: 0..source.len(),
        })?;
    let mut records = Vec::new();
    records.try_reserve_exact(count).map_err(|_| Error {
        kind: ErrorKind::Allocation,
        span: 0..source.len(),
    })?;
    records.push(Record {
        local_id: 1,
        parent: None,
        step: None,
        role: Role::Root,
        span: 0..source.len(),
    });
    for step in &model.steps {
        cancel.check().map_err(|error| Error {
            kind: ErrorKind::Cancelled(error),
            span: step.span.clone(),
        })?;
        let declaration = step
            .id
            .0
            .checked_sub(1)
            .and_then(|id| id.checked_mul(3))
            .and_then(|id| id.checked_add(2));
        let Some(declaration) = declaration.filter(|id| id.checked_add(2).is_some()) else {
            return Err(Error {
                kind: ErrorKind::Limit("local-identifiers"),
                span: step.id_span.clone(),
            });
        };
        records.push(Record {
            local_id: declaration,
            parent: Some(1),
            step: Some(step.id),
            role: Role::Step,
            span: step.span.clone(),
        });
        records.push(Record {
            local_id: declaration + 1,
            parent: Some(declaration),
            step: Some(step.id),
            role: Role::Definition,
            span: step.id_span.clone(),
        });
        records.push(Record {
            local_id: declaration + 2,
            parent: Some(declaration),
            step: Some(step.id),
            role: Role::Parent,
            span: step.parent_span.clone(),
        });
    }
    cancel.check().map_err(|error| Error {
        kind: ErrorKind::Cancelled(error),
        span: 0..source.len(),
    })?;
    Ok(Plan { model, records })
}

/// Returns the content identity of the complete shared PWCT source-model input.
#[must_use]
pub fn source_digest() -> rootlight_ids::ContentHash {
    rootlight_ids::content_hash(include_bytes!("pwct.rs"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rootlight_cancel::CancellationReason;

    use super::*;

    fn limits() -> Limits {
        Limits {
            source_bytes: 1_048_576,
            values: 100_000,
            depth: 16,
            decoded_bytes: 1_048_576,
            steps: 2_000,
            interactions: 2_000,
            work: 4_194_304,
        }
    }

    fn step(id: u64, parent: u64, interaction: u64) -> String {
        format!(
            r#"[{id},{parent},[["name","node"],["active",1],["code","see 1"],["interactionid",{interaction}],["visible",1],["stepnumber",1],["steptype",2]]]"#
        )
    }

    const INTERACTION: &str = r#"[1,1,"print","01/01/2023","12:00:00",""]"#;

    fn document(rows: &str, interactions: &str) -> String {
        format!(
            "# Visual Source File\ncVisualSourceFileVersion='1.0.0'\ncVisualLanguageName='RingPWCT'\ncNaturalLanguageName='english'\nnStepsID=1000\nnInteractionsID=1000\naStepsTable=[{rows}]\naInteractionsTable=[{interactions}]\n"
        )
    }

    fn parse(source: &str) -> Result<Model, Error> {
        decode(source.as_bytes(), limits(), &Cancellation::new())
    }

    fn refusal(source: &str, expected: ErrorKind) {
        let error = parse(source).unwrap_err();
        assert_eq!(error.kind, expected, "{}", source);
        assert!(error.span.start <= error.span.end && error.span.end <= source.len());
    }

    #[test]
    fn written_ids_own_sibling_rows_and_survive_reordering() {
        let first = step(2, 1, 1);
        let second = step(1, 0, 1);
        let source = document(&format!("{first},{second}"), INTERACTION);
        let model = parse(&source).unwrap();
        assert_eq!(model.steps[0].id, StepId(2));
        assert_eq!(model.steps[0].parent, ParentId::Step(StepId(1)));
        assert_eq!(model.steps[0].parent_index, Some(1));
        assert_eq!(model.steps[1].parent, ParentId::Root);
        assert_eq!(&source[model.steps[0].id_span.clone()], "2");
        assert_eq!(&source[model.steps[0].parent_span.clone()], "1");
        assert!(model.steps[0].span.end < model.steps[1].span.start);
        assert_eq!(model.steps[0].interaction_index, 0);
        assert_eq!(model.interactions[0].id, InteractionId(1));
        assert_eq!(model.interactions[0].component.text, "print");
    }

    #[test]
    fn malformed_graphs_never_return_a_partial_model() {
        let cases = [
            (
                format!("{},{}", step(1, 0, 1), step(1, 0, 1)),
                "duplicate-step-id",
            ),
            (step(1, 99, 1), "missing-parent"),
            (step(1, 1, 1), "parent-cycle"),
            (
                format!("{},{}", step(1, 2, 1), step(2, 1, 1)),
                "parent-cycle",
            ),
            (step(1, 0, 2), "missing-interaction"),
            (step(0, 0, 1), "zero-step-id"),
            (step(1, 0, 0), "zero-interaction-reference"),
        ];
        for (rows, expected) in cases {
            refusal(
                &document(&rows, INTERACTION),
                ErrorKind::Malformed(expected),
            );
        }
        let source = document(&step(1, 0, 1), &format!("{INTERACTION},{INTERACTION}"));
        refusal(&source, ErrorKind::Malformed("duplicate-interaction-id"));
        let source = document(&step(1, 0, 1), INTERACTION);
        refusal(
            &source.replace("[1,0,", "[1,0,9,"),
            ErrorKind::Malformed("row-arity"),
        );
        refusal(
            &source.replace(r#"["active",1]"#, r#"["active",2]"#),
            ErrorKind::Malformed("boolean-property"),
        );
        refusal(
            &source.replace(r#"["code","see 1"]"#, r#"["code","see 1"],["code",""]"#),
            ErrorKind::Malformed("duplicate-property"),
        );
        refusal(
            &source.replace("nStepsID=1000", "nStepsID=0"),
            ErrorKind::Malformed("step-counter"),
        );
        refusal(
            &source.replace("nInteractionsID=1000", "nInteractionsID=0"),
            ErrorKind::Malformed("interaction-counter"),
        );
        refusal(
            &source.replace(r#"["active",1],"#, ""),
            ErrorKind::Malformed("missing-property"),
        );
    }

    #[test]
    fn host_execution_and_header_lookalikes_are_unsupported() {
        let source = document(&step(1, 0, 1), INTERACTION);
        for expression in [
            r#"eval("see 1")"#,
            r#""a"+eval("see 1")"#,
            "char(35)",
            "char(other)",
            r##""#{run()}""##,
            "new Object",
        ] {
            let candidate = source.replace(r#""see 1""#, expression);
            assert!(
                matches!(
                    parse(&candidate).unwrap_err().kind,
                    ErrorKind::Unsupported(_)
                ),
                "{expression}"
            );
        }
        for statement in [
            r#"eval("see 1")"#,
            r#"load "program.ring""#,
            "system('command')",
            "aStepsTableExtra=[]",
        ] {
            assert!(
                matches!(
                    parse(&format!("{source}{statement}\n")).unwrap_err().kind,
                    ErrorKind::Unsupported(_)
                ),
                "{statement}"
            );
        }
        refusal(
            &source.replace("cVisualSourceFileVersion", "cVisualSourceFileVersionExtra"),
            ErrorKind::Unsupported("assignment"),
        );
        refusal(
            &source.replace("'1.0.0'", "'2.0.0'"),
            ErrorKind::Unsupported("visual-source-version"),
        );
        refusal(
            &source.replace("'RingPWCT'", "'Other'"),
            ErrorKind::Unsupported("visual-language"),
        );
        refusal(
            &source.replace("nStepsID=1000\n", "nStepsID=1000 nStepsID=1000\n"),
            ErrorKind::Malformed("assignment-separator"),
        );
        refusal(
            &source.replace("nStepsID=1000\n", "nStepsID=1000\nnStepsID=1000\n"),
            ErrorKind::Malformed("duplicate-assignment"),
        );
    }

    #[test]
    fn literal_concatenation_preserves_each_exact_origin() {
        let source = document(&step(1, 0, 1), INTERACTION)
            .replace(r#""see 1""#, r#""see "+char(34)+"π"+CHAR(34)+"""#);
        let model = parse(&source).unwrap();
        let code = model.steps[0].code().unwrap();
        assert_eq!(code.text, "see \"π\"");
        assert_eq!(code.segments.len(), 5);
        for segment in &code.segments {
            match segment.origin {
                TextOrigin::Literal => assert_eq!(
                    &code.text[segment.decoded.clone()],
                    &source[segment.source.clone()]
                ),
                TextOrigin::Character => {
                    assert_eq!(&code.text[segment.decoded.clone()], "\"");
                    assert!(source[segment.source.clone()].eq_ignore_ascii_case("char(34)"));
                }
            }
        }
        assert!(source[code.span.clone()].contains("char(34)"));
        let malformed = format!("{}\"unterminated", &source[..code.span.start]);
        refusal(&malformed, ErrorKind::Malformed("unterminated-string"));
    }

    #[test]
    fn every_declared_resource_ceiling_is_enforced() {
        let source = document(&step(1, 0, 1), INTERACTION);
        let base = limits();
        for limited in [
            Limits {
                source_bytes: source.len() - 1,
                ..base
            },
            Limits { values: 1, ..base },
            Limits { depth: 1, ..base },
            Limits {
                decoded_bytes: 1,
                ..base
            },
            Limits { steps: 0, ..base },
            Limits {
                interactions: 0,
                ..base
            },
            Limits { work: 1, ..base },
            Limits { depth: 65, ..base },
        ] {
            assert!(matches!(
                decode(source.as_bytes(), limited, &Cancellation::new())
                    .unwrap_err()
                    .kind,
                ErrorKind::Limit(_)
            ));
        }
        let cancel = Cancellation::new();
        let mut parser = Parser::new(
            b"\"a\"+char(34)+\"b\"",
            Limits { values: 2, ..base },
            &cancel,
        )
        .unwrap();
        assert_eq!(
            parser.text().unwrap_err().kind,
            ErrorKind::Limit("text-segments")
        );
    }

    #[test]
    fn cancellation_is_checked_after_source_work_has_started() {
        let cancel = Cancellation::new();
        let source = b"'first' [1,2,3]";
        let mut parser = Parser::new(source, limits(), &cancel).unwrap();
        assert!(matches!(parser.value(0).unwrap().kind, ValueKind::Text(_)));
        assert!(parser.position > 0);
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            parser.value(0).unwrap_err().kind,
            ErrorKind::Cancelled(_)
        ));
        assert!(matches!(
            decode(source, limits(), &cancel).unwrap_err().kind,
            ErrorKind::Cancelled(_)
        ));
    }

    #[test]
    fn raw_plan_uses_stable_ids_and_physical_containment_only() {
        let source = document(&format!("{},{}", step(2, 1, 1), step(1, 0, 1)), INTERACTION);
        let reordered = document(&format!("{},{}", step(1, 0, 1), step(2, 1, 1)), INTERACTION);
        let first = plan(source.as_bytes(), limits(), 7, &Cancellation::new()).unwrap();
        let second = plan(reordered.as_bytes(), limits(), 7, &Cancellation::new()).unwrap();
        assert_eq!(first.records.len(), 7);
        assert_eq!(first.records[0].role, Role::Root);
        assert_eq!(first.records[0].span, 0..source.len());
        assert_eq!(first.records[1].step, Some(StepId(2)));
        assert_eq!(first.model.steps[0].parent_index, Some(1));
        for record in first.records.iter().skip(1) {
            let same = second
                .records
                .iter()
                .find(|other| other.step == record.step && other.role == record.role)
                .unwrap();
            assert_eq!(record.local_id, same.local_id);
            assert_eq!(record.parent, same.parent);
            let parent = first
                .records
                .iter()
                .find(|parent| Some(parent.local_id) == record.parent)
                .unwrap();
            assert!(parent.span.start <= record.span.start && record.span.end <= parent.span.end);
            if record.role == Role::Step {
                assert_eq!(record.parent, Some(1));
            } else {
                assert_eq!(parent.role, Role::Step);
            }
        }
    }

    #[test]
    fn raw_count_and_identifier_overflow_refuse_without_id_sized_storage() {
        let source = document(&step(1, 0, 1), INTERACTION);
        assert_eq!(
            plan(source.as_bytes(), limits(), 3, &Cancellation::new())
                .unwrap_err()
                .kind,
            ErrorKind::Limit("records")
        );
        assert_eq!(
            plan(source.as_bytes(), limits(), 4, &Cancellation::new())
                .unwrap()
                .records
                .len(),
            4
        );
        let sparse = document(&step(1_000_000_000, 0, 1), INTERACTION)
            .replace("nStepsID=1000", "nStepsID=1000000000");
        let sparse = plan(sparse.as_bytes(), limits(), 4, &Cancellation::new()).unwrap();
        assert_eq!(sparse.records.len(), 4);
        assert_eq!(sparse.records[1].local_id, 2_999_999_999);
        let overflow = document(&step(u64::MAX, 0, 1), INTERACTION)
            .replace("nStepsID=1000", &format!("nStepsID={}", u64::MAX));
        assert_eq!(
            plan(overflow.as_bytes(), limits(), 4, &Cancellation::new())
                .unwrap_err()
                .kind,
            ErrorKind::Limit("local-identifiers")
        );
    }

    #[test]
    fn shared_parse_limits_conserve_the_native_budget_formula() {
        let shared = Limits::for_parse(3_145_728, usize::MAX, 128).unwrap();
        assert_eq!(shared.source_bytes, 3_145_728);
        assert_eq!(shared.decoded_bytes, shared.source_bytes);
        assert_eq!(shared.values, 2_097_152);
        assert_eq!(
            (shared.steps, shared.interactions),
            (shared.values, shared.values)
        );
        assert_eq!(shared.depth, 64);
        assert_eq!(shared.work, 27_262_976);
        assert_eq!(
            Limits::for_parse(usize::MAX, 1, 1).unwrap_err().kind,
            ErrorKind::Limit("work")
        );
    }

    #[test]
    fn only_written_numeric_definition_tokens_supply_pwct_names() {
        let file = rootlight_ids::FileId::from_bytes([7; 20]);
        let fact = |role: Role| {
            crate::SyntaxFact::new(
                3,
                Some(2),
                role.kind(),
                rootlight_ir::SourceSpan::new(file, 0, 1).unwrap(),
                role.depth(),
                crate::SyntaxKindLabel::new(role.syntax()).unwrap(),
            )
        };
        assert_eq!(
            crate::structural_entity_kind(&fact(Role::Step)),
            Some(rootlight_ir::EntityKind::Property)
        );
        assert_eq!(
            crate::structural_captured_name_for_fact("pwct", &fact(Role::Definition), "2", 64)
                .as_deref(),
            Some("2")
        );
        assert!(
            crate::structural_captured_name_for_fact("pwct", &fact(Role::Parent), "1", 64)
                .is_none()
        );
        assert!(crate::structural_captured_name_for_language("pwct", "Functions", 64).is_none());
        for invalid in [
            "0",
            "-1",
            "1.0",
            "1e2",
            "2foo",
            "Functions",
            " 2",
            "2 ",
            "18446744073709551616",
        ] {
            assert!(
                crate::structural_captured_name_for_fact(
                    "pwct",
                    &fact(Role::Definition),
                    invalid,
                    64
                )
                .is_none(),
                "{invalid}"
            );
        }
    }

    fn original(relative: &str, bytes: usize) -> (Vec<u8>, Model) {
        let root = PathBuf::from(
            std::env::var_os("ROOTLIGHT_PWCT_REPOSITORY")
                .expect("set ROOTLIGHT_PWCT_REPOSITORY to the independently pinned PWCT2 checkout"),
        );
        let source = std::fs::read(root.join(relative)).expect("original PWCT witness");
        assert_eq!(source.len(), bytes, "{relative}");
        let model =
            decode(&source, limits(), &Cancellation::new()).expect("original schema decode");
        assert_eq!(model.natural_language.text, "english");
        let mut code_fields = 0;
        let mut character_segments = 0;
        let mut properties = 0;
        for step in &model.steps {
            assert!(step.id.0 <= model.step_counter);
            assert!((1..=4).contains(&step.step_type));
            assert_eq!(
                std::str::from_utf8(&source[step.id_span.clone()])
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                step.id.0
            );
            assert_eq!(
                model.interactions[step.interaction_index].id,
                step.interaction
            );
            if let Some(index) = step.parent_index {
                assert_eq!(step.parent, ParentId::Step(model.steps[index].id));
            }
            for property in &step.properties {
                assert!(
                    step.span.start <= property.span.start && property.span.end <= step.span.end
                );
                assert!(
                    property.span.start <= property.name.span.start
                        && property.name.span.end <= property.span.end
                );
                properties += 1;
            }
            let code = step.code().unwrap();
            code_fields += usize::from(!code.text.is_empty());
            for segment in &code.segments {
                if segment.origin == TextOrigin::Literal {
                    assert_eq!(
                        &source[segment.source.clone()],
                        code.text[segment.decoded.clone()].as_bytes()
                    );
                } else {
                    assert_eq!(&code.text[segment.decoded.clone()], "\"");
                    character_segments += 1;
                }
            }
        }
        for interaction in &model.interactions {
            assert!(interaction.id.0 <= model.interaction_counter);
            assert!(
                interaction.span.start <= interaction.id_span.start
                    && interaction.id_span.end <= interaction.span.end
            );
            assert!((1..=2).contains(&interaction.kind));
            assert!(!interaction.date.text.is_empty() && !interaction.time.text.is_empty());
            assert!(interaction.variables.span.end <= source.len());
        }
        println!(
            "{relative}: steps={} interactions={} properties={properties} nonempty_code_fields={code_fields} code_character_segments={character_segments} active_steps={} visible_steps={}; Ring semantics remain unproven",
            model.steps.len(),
            model.interactions.len(),
            model.steps.iter().filter(|step| step.active).count(),
            model.steps.iter().filter(|step| step.visible).count()
        );
        (source, model)
    }

    #[test]
    #[ignore = "requires the independently pinned original PWCT2 checkout"]
    fn original_program9_ids_join_and_plain_code_origin() {
        let (source, model) = original("applications/FromPWCTDocs/program9.pwct", 1292);
        assert_eq!((model.steps.len(), model.interactions.len()), (2, 2));
        assert_eq!(model.steps[1].id_span, 523..524);
        assert_eq!(model.steps[1].parent_index, Some(0));
        let code = model.steps[1].code().unwrap();
        assert_eq!(code.text, "see 3+5*4");
        assert_eq!(code.span, 793..804);
        assert_eq!(code.segments[0].source, 794..803);
        assert_eq!(&source[794..803], b"see 3+5*4");
        assert_eq!(
            model.interactions[model.steps[1].interaction_index]
                .component
                .text,
            "print"
        );
    }

    #[test]
    #[ignore = "requires the independently pinned original PWCT2 checkout"]
    fn original_program3_organizational_captions_remain_data() {
        let (_, model) = original("applications/FromPWCTDocs/program3.pwct", 3183);
        assert_eq!((model.steps.len(), model.interactions.len()), (7, 3));
        for caption in ["Functions", "Classes"] {
            let step = model.steps.iter().find(|step| property(&step.properties, "name", &step.span).is_ok_and(|(_, value)| matches!(&value.kind, ValueKind::Text(text) if text.text == caption))).unwrap();
            assert_eq!(step.code().unwrap().text, "");
        }
        let print = model
            .steps
            .iter()
            .find(|step| step.id == StepId(7))
            .unwrap();
        assert_eq!(print.parent, ParentId::Step(StepId(4)));
        assert_eq!(
            model.interactions[print.interaction_index].component.text,
            "print"
        );
    }

    #[test]
    #[ignore = "requires the independently pinned original PWCT2 checkout"]
    fn original_file_read_has_literal_quotes_and_component_join() {
        let (_, model) = original("applications/FromPWCTVideos/lesson32_part2.pwct", 2253);
        assert_eq!((model.steps.len(), model.interactions.len()), (3, 3));
        let read = model
            .steps
            .iter()
            .find(|step| step.id == StepId(2))
            .unwrap();
        assert_eq!(
            model.interactions[read.interaction_index].component.text,
            "file_read"
        );
        assert_eq!(
            read.code().unwrap().text,
            "cStr = read(\"text/myfile.txt\")"
        );
        assert_eq!(
            read.code()
                .unwrap()
                .segments
                .iter()
                .filter(|segment| segment.origin == TextOrigin::Character)
                .count(),
            2
        );
    }

    #[test]
    #[ignore = "requires the independently pinned original PWCT2 checkout"]
    fn original_cards_keeps_visual_nodes_code_fragments_and_joins() {
        let (_, model) = original("applications/Cards/cards.pwct", 301114);
        assert_eq!((model.steps.len(), model.interactions.len()), (452, 302));
        let function = model
            .steps
            .iter()
            .find(|step| step.id == StepId(408))
            .unwrap();
        assert_eq!(function.parent, ParentId::Step(StepId(130)));
        assert_eq!(function.interaction, InteractionId(280));
        assert_eq!(function.code().unwrap().text, "func isnewgame  { ");
        assert!(!function.code().unwrap().text.contains('}'));
        assert!(model.steps.iter().any(|step| {
            step.code()
                .is_some_and(|code| code.text == "load \"stdlibcore.ring\"")
        }));
    }
}
