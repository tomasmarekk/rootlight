//! Physical unified patches are inert input, rather than executable CMake.
//! The private mask preserves decoded byte coordinates; the complete row plan
//! describes patch ownership without merging deleted and added target programs.

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
use rootlight_cancel::{Cancellation, Cancelled};

use crate::query_pack::{QueryCandidate, StructuralRole};

use super::{CANCELLATION_BYTE_INTERVAL, provider_failure, require_provider_limit};

pub(crate) const CMAKE_PATCH_PROJECTION_POLICY: &str =
    "rootlight.treesitter.cmake-physical-unified-patch/1";

pub(super) struct CmakePatchInput {
    bytes: Vec<u8>,
    candidates: Vec<QueryCandidate>,
    owned_bytes: usize,
    max_depth: usize,
}

impl CmakePatchInput {
    /// Recognizes only an initial physical patch marker and validates the whole file.
    ///
    /// Ordinary CMake strings and comments remain on the native route. Recognized
    /// incomplete, unsupported, or over-budget patches retain a whole-source gap.
    pub(super) fn new(
        source: &[u8],
        max_units: usize,
        max_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        cancellation.check()?;
        let mut lines = Lines::new(source, cancellation);
        let first = loop {
            let Some(line) = lines.next()? else {
                return Ok(None);
            };
            if !all_whitespace(line.content(source), cancellation)? {
                break line;
            }
        };
        let Some(kind) = initial_kind(first.content(source)) else {
            return Ok(None);
        };
        require_provider_limit(ResourceKind::SyntaxNodes, 3, max_units)?;
        let minimum_bytes = source
            .len()
            .checked_add(3 * size_of::<QueryCandidate>())
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            minimum_bytes,
            max_owned_bytes,
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(source.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            bytes
                .capacity()
                .checked_add(3 * size_of::<QueryCandidate>())
                .ok_or(SinkError::AccountingOverflow)?,
            max_owned_bytes,
        )?;
        for chunk in source.chunks(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
            bytes.extend(chunk.iter().map(|byte| {
                if matches!(byte, b'\r' | b'\n') {
                    *byte
                } else {
                    b' '
                }
            }));
        }
        let mut plan = Plan::new(source.len(), max_units, max_owned_bytes, bytes.capacity());
        let parsed = if kind == InitialKind::Unsupported {
            Err(PlanFailure::Issue(PatchIssue::Unsupported))
        } else {
            parse_patch(source, &mut plan, cancellation)
        };
        if let Err(failure) = parsed {
            let issue = match failure {
                PlanFailure::Issue(issue) => issue,
                PlanFailure::Adapter(error) => return Err(error),
            };
            // A prefix from an invalid or exhausted patch cannot establish a
            // complete physical structure. Replace it, rather than truncate it.
            plan = Plan::new(source.len(), max_units, max_owned_bytes, bytes.capacity());
            plan.base().map_err(PlanFailure::into_adapter)?;
            plan.push(candidate(
                0,
                source.len(),
                StructuralRole::Signature,
                issue.syntax(),
                1,
            ))
            .map_err(PlanFailure::into_adapter)?;
        }
        cancellation.check()?;
        let owned_bytes = plan.owned_bytes()?;
        Ok(Some(Self {
            bytes,
            candidates: plan.candidates,
            owned_bytes,
            max_depth: plan.max_depth,
        }))
    }

    pub(super) fn parse_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn owned_bytes(&self) -> usize {
        self.owned_bytes
    }

    pub(super) fn required_candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub(super) fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Moves the mandatory physical plan into place after releasing its private mask.
    pub(super) fn append_candidates(
        self,
        candidates: &mut Vec<QueryCandidate>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        let Self {
            bytes,
            candidates: planned,
            ..
        } = self;
        drop(bytes);
        *candidates = planned;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InitialKind {
    Unified,
    Unsupported,
}

fn initial_kind(line: &[u8]) -> Option<InitialKind> {
    if line.starts_with(b"diff --git ")
        || line == b"diff --git"
        || is_header_marker(line, b"---")
        || is_header_marker(line, b"+++")
        || line.starts_with(b"@@ -")
    {
        Some(InitialKind::Unified)
    } else if line.starts_with(b"diff --cc ")
        || line.starts_with(b"diff --combined ")
        || line.starts_with(b"@@@ ")
        || line.starts_with(b"GIT binary patch")
        || line.starts_with(b"Binary files ")
        || line.starts_with(b"Index: ")
        || line == b"Index:"
        || line.starts_with(b"*** ")
        || line == b"***************"
        || line.starts_with(b"diff -")
        || is_mail_patch_marker(line)
    {
        Some(InitialKind::Unsupported)
    } else {
        None
    }
}

fn is_header_marker(line: &[u8], marker: &[u8]) -> bool {
    line.strip_prefix(marker)
        .is_some_and(|suffix| suffix.is_empty() || matches!(suffix.first(), Some(b' ' | b'\t')))
}

fn is_mail_patch_marker(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(b"From ") else {
        return false;
    };
    // Format-patch's sentinel has a fixed-size hash and suffix. Restrict the
    // probe before searching so ordinary very long first lines remain bounded.
    if !matches!(rest.len(), 65 | 89) {
        return false;
    }
    let Some((hash, suffix)) = split_once_byte(rest, b' ') else {
        return false;
    };
    matches!(hash.len(), 40 | 64)
        && hash.iter().all(u8::is_ascii_hexdigit)
        && suffix == b"Mon Sep 17 00:00:00 2001"
}

#[derive(Clone, Copy)]
enum PatchIssue {
    Invalid,
    Unsupported,
    StructureLimit,
}

impl PatchIssue {
    const fn syntax(self) -> &'static str {
        match self {
            Self::Invalid => "patch.invalid_fragment",
            Self::Unsupported => "patch.unsupported_fragment",
            Self::StructureLimit => "patch.structure_limit",
        }
    }
}

enum PlanFailure {
    Issue(PatchIssue),
    Adapter(AdapterError),
}

impl PlanFailure {
    fn into_adapter(self) -> AdapterError {
        match self {
            Self::Adapter(error) => error,
            Self::Issue(_) => provider_failure("cmake-patch-gap-admission"),
        }
    }
}

impl From<AdapterError> for PlanFailure {
    fn from(value: AdapterError) -> Self {
        Self::Adapter(value)
    }
}

impl From<Cancelled> for PlanFailure {
    fn from(value: Cancelled) -> Self {
        Self::Adapter(value.into())
    }
}

impl From<SinkError> for PlanFailure {
    fn from(value: SinkError) -> Self {
        Self::Adapter(value.into())
    }
}

type PlanResult<T> = Result<T, PlanFailure>;

fn invalid<T>() -> PlanResult<T> {
    Err(PlanFailure::Issue(PatchIssue::Invalid))
}

struct Plan {
    candidates: Vec<QueryCandidate>,
    source_len: usize,
    max_units: usize,
    max_owned_bytes: usize,
    mask_capacity: usize,
    max_depth: usize,
}

impl Plan {
    fn new(
        source_len: usize,
        max_units: usize,
        max_owned_bytes: usize,
        mask_capacity: usize,
    ) -> Self {
        Self {
            candidates: Vec::new(),
            source_len,
            max_units,
            max_owned_bytes,
            mask_capacity,
            max_depth: 0,
        }
    }

    fn owned_bytes(&self) -> Result<usize, AdapterError> {
        self.candidates
            .capacity()
            .checked_mul(size_of::<QueryCandidate>())
            .and_then(|bytes| bytes.checked_add(self.mask_capacity))
            .ok_or_else(|| SinkError::AccountingOverflow.into())
    }

    fn push(&mut self, value: QueryCandidate) -> PlanResult<usize> {
        if self.candidates.len() >= self.max_units {
            return Err(PlanFailure::Issue(PatchIssue::StructureLimit));
        }
        if self.candidates.len() == self.candidates.capacity() {
            let maximum_capacity = self
                .max_owned_bytes
                .checked_sub(self.mask_capacity)
                .map(|bytes| bytes / size_of::<QueryCandidate>())
                .unwrap_or(0)
                .min(self.max_units);
            let remaining = maximum_capacity.saturating_sub(self.candidates.capacity());
            if remaining == 0 {
                return Err(PlanFailure::Issue(PatchIssue::StructureLimit));
            }
            let additional = self.candidates.capacity().max(3).min(remaining);
            self.candidates
                .try_reserve_exact(additional)
                .map_err(|_| SinkError::AllocationFailed)?;
            if self.candidates.capacity() > maximum_capacity {
                return Err(PlanFailure::Issue(PatchIssue::StructureLimit));
            }
        }
        let index = self.candidates.len();
        self.max_depth = self.max_depth.max(value.native_depth);
        self.candidates.push(value);
        Ok(index)
    }

    fn base(&mut self) -> PlanResult<()> {
        self.push(candidate(
            0,
            self.source_len,
            StructuralRole::Root,
            "file",
            0,
        ))?;
        self.push(candidate(
            0,
            self.source_len,
            StructuralRole::Module,
            "patch.file",
            1,
        ))?;
        Ok(())
    }

    fn row(&mut self, line: Line, syntax: &'static str, depth: usize) -> PlanResult<()> {
        self.push(candidate(
            line.start,
            line.end,
            StructuralRole::Documentation,
            syntax,
            depth,
        ))?;
        Ok(())
    }

    fn heading(&mut self, line: Line, depth: usize) -> PlanResult<()> {
        for (role, syntax) in [
            (StructuralRole::Definition, "patch.heading"),
            (StructuralRole::Signature, "patch.heading"),
        ] {
            self.push(candidate(line.start, line.content_end, role, syntax, depth))?;
        }
        self.row(line, "patch.metadata_row", depth)
    }

    fn close(&mut self, index: usize, end: usize) -> PlanResult<()> {
        self.candidates
            .get_mut(index)
            .ok_or_else(|| provider_failure("cmake-patch-container"))?
            .end = end;
        Ok(())
    }
}

fn candidate(
    start: usize,
    end: usize,
    role: StructuralRole,
    syntax: &'static str,
    native_depth: usize,
) -> QueryCandidate {
    QueryCandidate {
        start,
        end,
        role,
        syntax,
        required: true,
        native_depth,
    }
}

#[derive(Clone, Copy)]
struct Line {
    start: usize,
    content_end: usize,
    end: usize,
}

impl Line {
    fn content(self, source: &[u8]) -> &[u8] {
        source.get(self.start..self.content_end).unwrap_or_default()
    }
}

struct Lines<'a> {
    source: &'a [u8],
    cancellation: &'a Cancellation,
    position: usize,
    peeked: Option<Line>,
}

impl<'a> Lines<'a> {
    fn new(source: &'a [u8], cancellation: &'a Cancellation) -> Self {
        Self {
            source,
            cancellation,
            position: usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3,
            peeked: None,
        }
    }

    fn peek(&mut self) -> Result<Option<Line>, AdapterError> {
        if self.peeked.is_none() {
            self.peeked = self.read()?;
        }
        Ok(self.peeked)
    }

    fn next(&mut self) -> Result<Option<Line>, AdapterError> {
        match self.peeked.take() {
            Some(line) => Ok(Some(line)),
            None => self.read(),
        }
    }

    fn read(&mut self) -> Result<Option<Line>, AdapterError> {
        self.cancellation.check()?;
        if self.position == self.source.len() {
            return Ok(None);
        }
        let start = self.position;
        while let Some(byte) = self.source.get(self.position) {
            if self.position.is_multiple_of(CANCELLATION_BYTE_INTERVAL) {
                self.cancellation.check()?;
            }
            if matches!(byte, b'\r' | b'\n') {
                break;
            }
            self.position += 1;
        }
        let content_end = self.position;
        if self.source.get(self.position) == Some(&b'\r') {
            self.position += 1;
        }
        if self.source.get(self.position) == Some(&b'\n') {
            self.position += 1;
        }
        Ok(Some(Line {
            start,
            content_end,
            end: self.position,
        }))
    }
}

fn parse_patch(source: &[u8], plan: &mut Plan, cancellation: &Cancellation) -> PlanResult<()> {
    plan.base()?;
    let mut lines = Lines::new(source, cancellation);
    while let Some(line) = lines.peek()? {
        if all_whitespace(line.content(source), cancellation)? {
            lines.next()?;
            plan.row(line, "patch.metadata_row", 2)?;
        } else {
            break;
        }
    }
    let mut files = 0usize;
    while let Some(first) = lines.next()? {
        cancellation.check()?;
        let git = first.content(source).starts_with(b"diff --git ");
        if !git && !first.content(source).starts_with(b"--- ") {
            return invalid();
        }
        let section = plan.push(candidate(
            first.start,
            source.len(),
            StructuralRole::Declaration,
            "patch.file_section",
            2,
        ))?;
        plan.heading(first, 3)?;
        let mut headers = None;
        let mut metadata = GitMetadata::default();
        if git {
            let names = first
                .content(source)
                .strip_prefix(b"diff --git ")
                .unwrap_or_default();
            if !valid_git_names(names, cancellation)? {
                return invalid();
            }
            while let Some(line) = lines.peek()? {
                let content = line.content(source);
                if content.starts_with(b"--- ") {
                    lines.next()?;
                    headers = Some(parse_headers(source, line, &mut lines, plan, cancellation)?);
                    break;
                }
                if content.starts_with(b"diff --git ") {
                    break;
                }
                if unsupported_row(content) {
                    return Err(PlanFailure::Issue(PatchIssue::Unsupported));
                }
                if !metadata.accept(content, cancellation)? {
                    return invalid();
                }
                lines.next()?;
                plan.row(line, "patch.metadata_row", 3)?;
            }
        } else {
            headers = Some(parse_headers(
                source,
                first,
                &mut lines,
                plan,
                cancellation,
            )?);
        }
        if git && !metadata.valid(headers) {
            return invalid();
        }
        let mut hunks = 0usize;
        let mut previous = None;
        let mut closed_sides = (false, false);
        while let Some(line) = lines.peek()? {
            let content = line.content(source);
            if content.starts_with(b"@@ ") {
                lines.next()?;
                let ranges = parse_hunk_header(content, cancellation)?
                    .ok_or(PlanFailure::Issue(PatchIssue::Invalid))?;
                if headers.is_none()
                    || headers.is_some_and(|(old_null, new_null)| {
                        (old_null && (ranges.old.start != 0 || ranges.old.count != 0))
                            || (new_null && (ranges.new.start != 0 || ranges.new.count != 0))
                    })
                    || previous.is_some_and(|prior: HunkRanges| !prior.precedes(ranges))
                    || (closed_sides.0 && ranges.old.count != 0)
                    || (closed_sides.1 && ranges.new.count != 0)
                {
                    return invalid();
                }
                parse_hunk(source, line, ranges, &mut lines, plan, &mut closed_sides)?;
                previous = Some(ranges);
                hunks += 1;
            } else if content.starts_with(b"diff --git ") || content.starts_with(b"--- ") {
                break;
            } else if unsupported_row(content) {
                return Err(PlanFailure::Issue(PatchIssue::Unsupported));
            } else if content.is_empty() {
                lines.next()?;
                plan.row(line, "patch.metadata_row", 3)?;
            } else {
                return invalid();
            }
        }
        if headers.is_some() && hunks == 0 {
            return invalid();
        }
        let end = lines.peek()?.map_or(source.len(), |line| line.start);
        plan.close(section, end)?;
        files += 1;
    }
    if files == 0 {
        return invalid();
    }
    plan.push(candidate(
        0,
        source.len(),
        StructuralRole::Signature,
        "patch.unapplied_fragment",
        1,
    ))?;
    Ok(())
}

fn parse_headers(
    source: &[u8],
    old: Line,
    lines: &mut Lines<'_>,
    plan: &mut Plan,
    cancellation: &Cancellation,
) -> PlanResult<(bool, bool)> {
    let Some(new) = lines.next()? else {
        return invalid();
    };
    let Some(old_path) = old.content(source).strip_prefix(b"--- ") else {
        return invalid();
    };
    let Some(new_path) = new.content(source).strip_prefix(b"+++ ") else {
        return invalid();
    };
    let Some(old_null) = valid_header_path(old_path, cancellation)? else {
        return invalid();
    };
    let Some(new_null) = valid_header_path(new_path, cancellation)? else {
        return invalid();
    };
    if old_null && new_null {
        return invalid();
    }
    // The traditional first header is already the section heading. Git's two
    // physical path rows remain metadata, never local filesystem operations.
    if plan
        .candidates
        .last()
        .is_none_or(|value| value.start != old.start)
    {
        plan.row(old, "patch.metadata_row", 3)?;
    }
    plan.row(new, "patch.metadata_row", 3)?;
    Ok((old_null, new_null))
}

fn valid_header_path(
    value: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<bool>, AdapterError> {
    if value.is_empty() || contains_nul(value, cancellation)? {
        return Ok(None);
    }
    if value.first() == Some(&b'"') {
        let Some(end) = quoted_path_end(value, cancellation)? else {
            return Ok(None);
        };
        if end <= 2 || value.get(end).is_some_and(|byte| *byte != b'\t') {
            return Ok(None);
        }
        return Ok(Some(value.get(1..end - 1) == Some(b"/dev/null")));
    }
    let end = byte_position(value, b'\t', cancellation)?.unwrap_or(value.len());
    let path = value.get(..end).unwrap_or_default();
    if all_whitespace(path, cancellation)? {
        return Ok(None);
    }
    Ok(Some(path == b"/dev/null"))
}

fn quoted_path_end(
    value: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut position = 1usize;
    while let Some(byte) = value.get(position) {
        // Escape sequences can skip an exact multiple of the interval.
        if position % CANCELLATION_BYTE_INTERVAL <= 4 {
            cancellation.check()?;
        }
        match byte {
            b'"' => return Ok(Some(position + 1)),
            b'\\' => {
                position += 1;
                let Some(byte) = value.get(position) else {
                    return Ok(None);
                };
                match byte {
                    b'"' | b'\\' | b'a' | b'b' | b't' | b'n' | b'v' | b'f' | b'r' => {
                        position += 1;
                    }
                    b'0'..=b'7' => {
                        let mut octal = 0u16;
                        for _ in 0..3 {
                            if let Some(byte @ b'0'..=b'7') = value.get(position) {
                                octal = octal * 8 + u16::from(*byte - b'0');
                                position += 1;
                            } else {
                                break;
                            }
                        }
                        if octal == 0 || octal > u16::from(u8::MAX) {
                            return Ok(None);
                        }
                    }
                    _ => return Ok(None),
                }
            }
            _ => position += 1,
        }
    }
    Ok(None)
}

fn contains_nul(value: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    Ok(byte_position(value, 0, cancellation)?.is_some())
}

fn all_whitespace(value: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    for chunk in value.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        if !chunk.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn byte_position(
    value: &[u8],
    byte: u8,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    for (index, chunk) in value.chunks(CANCELLATION_BYTE_INTERVAL).enumerate() {
        cancellation.check()?;
        if let Some(position) = chunk.iter().position(|candidate| *candidate == byte) {
            return Ok(Some(index * CANCELLATION_BYTE_INTERVAL + position));
        }
    }
    Ok(None)
}

fn valid_git_names(value: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    if contains_nul(value, cancellation)? {
        return Ok(false);
    }
    let split = if value.first() == Some(&b'"') {
        let Some(end) = quoted_path_end(value, cancellation)? else {
            return Ok(false);
        };
        if end <= 2 || value.get(end) != Some(&b' ') {
            return Ok(false);
        }
        end
    } else {
        let Some(end) = byte_position(value, b' ', cancellation)? else {
            return Ok(false);
        };
        if end == 0 {
            return Ok(false);
        }
        end
    };
    let rest = value.get(split + 1..).unwrap_or_default();
    if rest.first() == Some(&b'"') {
        Ok(quoted_path_end(rest, cancellation)?.is_some_and(|end| end > 2 && end == rest.len()))
    } else {
        Ok(!all_whitespace(rest, cancellation)?)
    }
}

fn unsupported_row(value: &[u8]) -> bool {
    value.starts_with(b"@@@")
        || value.starts_with(b"diff --cc ")
        || value.starts_with(b"diff --combined ")
        || value == b"GIT binary patch"
        || (value.starts_with(b"Binary files ") && value.ends_with(b" differ"))
}

#[derive(Default)]
struct GitMetadata {
    old_mode: bool,
    new_mode: bool,
    new_file: bool,
    deleted_file: bool,
    rename_from: bool,
    rename_to: bool,
    copy_from: bool,
    copy_to: bool,
    similarity: bool,
    dissimilarity: bool,
    index: bool,
}

impl GitMetadata {
    fn accept(&mut self, value: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
        for (prefix, field) in [
            (b"old mode ".as_slice(), &mut self.old_mode),
            (b"new mode ".as_slice(), &mut self.new_mode),
            (b"new file mode ".as_slice(), &mut self.new_file),
            (b"deleted file mode ".as_slice(), &mut self.deleted_file),
        ] {
            if let Some(mode) = value.strip_prefix(prefix) {
                return Ok(valid_mode(mode) && mark_once(field));
            }
        }
        for (prefix, field) in [
            (b"similarity index ".as_slice(), &mut self.similarity),
            (b"dissimilarity index ".as_slice(), &mut self.dissimilarity),
        ] {
            if let Some(percent) = value
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(b"%"))
            {
                return Ok(
                    parse_decimal(percent, cancellation)?.is_some_and(|value| value <= 100)
                        && mark_once(field),
                );
            }
        }
        for (prefix, field) in [
            (b"rename from ".as_slice(), &mut self.rename_from),
            (b"rename to ".as_slice(), &mut self.rename_to),
            (b"copy from ".as_slice(), &mut self.copy_from),
            (b"copy to ".as_slice(), &mut self.copy_to),
        ] {
            if let Some(path) = value.strip_prefix(prefix) {
                return Ok(
                    valid_header_path(path, cancellation)?.is_some_and(|is_null| !is_null)
                        && mark_once(field),
                );
            }
        }
        if let Some(index) = value.strip_prefix(b"index ") {
            return Ok(valid_index(index) && mark_once(&mut self.index));
        }
        Ok(false)
    }

    fn valid(&self, headers: Option<(bool, bool)>) -> bool {
        let mode_change = self.old_mode && self.new_mode;
        let rename = self.rename_from && self.rename_to;
        let copy = self.copy_from && self.copy_to;
        if self.old_mode != self.new_mode
            || self.rename_from != self.rename_to
            || self.copy_from != self.copy_to
            || (rename && copy)
            || (self.new_file && self.deleted_file)
            || (mode_change && (self.new_file || self.deleted_file))
            || (self.similarity && self.dissimilarity)
        {
            return false;
        }
        match headers {
            Some((old_null, new_null)) => {
                (!self.new_file || old_null) && (!self.deleted_file || new_null)
            }
            None => mode_change || rename || copy || self.new_file || self.deleted_file,
        }
    }
}

fn mark_once(value: &mut bool) -> bool {
    if *value {
        false
    } else {
        *value = true;
        true
    }
}

fn valid_index(index: &[u8]) -> bool {
    // Hashes have a bounded representation; reject oversize rows before any
    // delimiter search rather than rescan an unbounded metadata value.
    if index.len() > 2 * 64 + 2 + 1 + 6 {
        return false;
    }
    let mut fields = index.split(|byte| *byte == b' ');
    let Some(hashes) = fields.next() else {
        return false;
    };
    let Some(split) = hashes.windows(2).position(|bytes| bytes == b"..") else {
        return false;
    };
    let old = hashes.get(..split).unwrap_or_default();
    let new = hashes.get(split + 2..).unwrap_or_default();
    let hashes_valid = [old, new]
        .iter()
        .all(|hash| !hash.is_empty() && hash.len() <= 64 && hash.iter().all(u8::is_ascii_hexdigit));
    hashes_valid && fields.next().is_none_or(valid_mode) && fields.next().is_none()
}

fn valid_mode(value: &[u8]) -> bool {
    value.len() == 6 && value.iter().all(|byte| matches!(byte, b'0'..=b'7'))
}

#[derive(Clone, Copy)]
struct HunkRange {
    start: u64,
    count: u64,
}

impl HunkRange {
    fn follows(self, prior: Self) -> bool {
        // Unified zero-count ranges anchor after their numbered line; nonempty
        // ranges begin before it. Compare boundaries in the same coordinate space.
        let begin = if self.count == 0 {
            Some(self.start)
        } else {
            self.start.checked_sub(1)
        };
        let end = prior.start.checked_add(prior.count.saturating_sub(1));
        begin.zip(end).is_some_and(|(begin, end)| begin >= end)
    }
}

#[derive(Clone, Copy)]
struct HunkRanges {
    old: HunkRange,
    new: HunkRange,
}

impl HunkRanges {
    fn precedes(self, next: Self) -> bool {
        next.old.follows(self.old) && next.new.follows(self.new)
    }
}

fn parse_hunk_header(
    value: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<HunkRanges>, AdapterError> {
    let Some(rest) = value.strip_prefix(b"@@ -") else {
        return Ok(None);
    };
    let Some((old, rest)) = split_once_cancellable(rest, b' ', cancellation)? else {
        return Ok(None);
    };
    let Some(rest) = rest.strip_prefix(b"+") else {
        return Ok(None);
    };
    let Some((new, suffix)) = split_once_cancellable(rest, b' ', cancellation)? else {
        return Ok(None);
    };
    let Some(suffix) = suffix.strip_prefix(b"@@") else {
        return Ok(None);
    };
    if !suffix.is_empty() && suffix.first() != Some(&b' ') {
        return Ok(None);
    }
    let Some(old) = parse_range(old, cancellation)? else {
        return Ok(None);
    };
    let Some(new) = parse_range(new, cancellation)? else {
        return Ok(None);
    };
    Ok(Some(HunkRanges { old, new }))
}

fn parse_range(
    value: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<HunkRange>, AdapterError> {
    let (start, count) = match split_once_cancellable(value, b',', cancellation)? {
        Some((start, count)) => (
            parse_decimal(start, cancellation)?,
            parse_decimal(count, cancellation)?,
        ),
        None => (parse_decimal(value, cancellation)?, Some(1)),
    };
    let (Some(start), Some(count)) = (start, count) else {
        return Ok(None);
    };
    if (count != 0 && start == 0) || start.checked_add(count.saturating_sub(1)).is_none() {
        return Ok(None);
    }
    Ok(Some(HunkRange { start, count }))
}

fn parse_decimal(value: &[u8], cancellation: &Cancellation) -> Result<Option<u64>, AdapterError> {
    if value.is_empty() {
        return Ok(None);
    }
    let mut total = 0u64;
    for chunk in value.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        for byte in chunk {
            if !byte.is_ascii_digit() {
                return Ok(None);
            }
            let Some(next) = total
                .checked_mul(10)
                .and_then(|total| total.checked_add(u64::from(*byte - b'0')))
            else {
                return Ok(None);
            };
            total = next;
        }
    }
    Ok(Some(total))
}

type ByteSplit<'a> = (&'a [u8], &'a [u8]);

fn split_once_cancellable<'a>(
    value: &'a [u8],
    delimiter: u8,
    cancellation: &Cancellation,
) -> Result<Option<ByteSplit<'a>>, AdapterError> {
    let Some(position) = byte_position(value, delimiter, cancellation)? else {
        return Ok(None);
    };
    Ok(value.get(..position).zip(value.get(position + 1..)))
}

fn split_once_byte(value: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let position = value.iter().position(|byte| *byte == delimiter)?;
    Some((value.get(..position)?, value.get(position + 1..)?))
}

#[derive(Clone, Copy)]
enum RowRole {
    Deleted,
    Added,
    Context,
}

fn parse_hunk(
    source: &[u8],
    heading: Line,
    ranges: HunkRanges,
    lines: &mut Lines<'_>,
    plan: &mut Plan,
    closed_sides: &mut (bool, bool),
) -> PlanResult<()> {
    let hunk = plan.push(candidate(
        heading.start,
        source.len(),
        StructuralRole::Declaration,
        "patch.hunk",
        3,
    ))?;
    plan.heading(heading, 4)?;
    let mut old = ranges.old.count;
    let mut new = ranges.new.count;
    let mut preceding = None;
    while let Some(line) = lines.peek()? {
        let content = line.content(source);
        if content == b"\\ No newline at end of file" {
            let Some(role) = preceding.take() else {
                return invalid();
            };
            match role {
                RowRole::Deleted => closed_sides.0 = true,
                RowRole::Added => closed_sides.1 = true,
                RowRole::Context => *closed_sides = (true, true),
            }
            lines.next()?;
            plan.row(line, "patch.no_newline_row", 4)?;
            continue;
        }
        if old == 0 && new == 0 {
            break;
        }
        let (role, syntax) = match content.first() {
            Some(b'-') if old != 0 && !closed_sides.0 => {
                old -= 1;
                (RowRole::Deleted, "patch.deleted_row")
            }
            Some(b'+') if new != 0 && !closed_sides.1 => {
                new -= 1;
                (RowRole::Added, "patch.added_row")
            }
            Some(b' ') if old != 0 && new != 0 && !closed_sides.0 && !closed_sides.1 => {
                old -= 1;
                new -= 1;
                (RowRole::Context, "patch.context_row")
            }
            _ => return invalid(),
        };
        if line.content_end == line.end {
            return invalid();
        }
        lines.next()?;
        plan.row(line, syntax, 4)?;
        preceding = Some(role);
    }
    if old != 0 || new != 0 {
        return invalid();
    }
    let end = lines.peek()?.map_or(source.len(), |line| line.start);
    plan.close(hunk, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> CmakePatchInput {
        CmakePatchInput::new(source, 100_000, usize::MAX, &Cancellation::new())
            .unwrap()
            .expect("fixture is a physical patch")
    }

    fn has(input: &CmakePatchInput, syntax: &str) -> bool {
        input
            .candidates
            .iter()
            .any(|candidate| candidate.syntax == syntax)
    }

    fn complete(input: &CmakePatchInput) -> bool {
        has(input, "patch.unapplied_fragment")
    }

    #[test]
    fn complete_multiple_files_preserve_physical_ownership_and_all_rows() {
        let source = b"diff --git a/a.cmake b/a.cmake\nindex 1234..5678 100644\n--- a/a.cmake\n+++ b/a.cmake\n@@ -1,2 +1,2 @@ title\n keep\n-old\n+new\n@@ -5 +5 @@\n-later\n+last\ndiff --git a/b.cmake b/b.cmake\n--- a/b.cmake\n+++ b/b.cmake\n@@ -0,0 +1 @@\n+added\n";
        let input = parse(source);
        assert!(complete(&input));
        assert_eq!(input.parse_bytes().len(), source.len());
        assert!(
            input
                .parse_bytes()
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\n' | b'\r'))
        );
        assert_eq!(
            input
                .candidates
                .iter()
                .filter(|c| c.syntax == "patch.file_section")
                .count(),
            2
        );
        assert_eq!(
            input
                .candidates
                .iter()
                .filter(|c| c.syntax == "patch.hunk")
                .count(),
            3
        );
        assert_eq!(
            input
                .candidates
                .iter()
                .filter(|c| c.role == StructuralRole::Documentation)
                .count(),
            source.iter().filter(|b| **b == b'\n').count()
        );
        for candidate in &input.candidates {
            assert!(candidate.required);
            assert!(candidate.start <= candidate.end && candidate.end <= source.len());
            assert!(!matches!(
                candidate.role,
                StructuralRole::Call | StructuralRole::Reference
            ));
            if candidate.syntax == "patch.heading" {
                assert!(!source[candidate.start..candidate.end].contains(&b'\n'));
            }
        }
        let sections: Vec<_> = input
            .candidates
            .iter()
            .filter(|c| c.syntax == "patch.file_section")
            .collect();
        assert_eq!(sections[0].end, sections[1].start);
    }

    #[test]
    fn traditional_patch_retains_deleted_added_and_context_markers_as_rows() {
        let source = b"--- old.cmake\t2026-01-01\n+++ new.cmake\t2026-01-02\n@@ -1,3 +1,3 @@\n--- deleted text\n+++ added text\n context\n-old\n+new\n";
        let input = parse(source);
        assert!(complete(&input));
        let deleted: Vec<_> = input
            .candidates
            .iter()
            .filter(|c| c.syntax == "patch.deleted_row")
            .map(|c| &source[c.start..c.end])
            .collect();
        assert_eq!(deleted, [b"--- deleted text\n".as_slice(), b"-old\n"]);
        assert!(has(&input, "patch.context_row"));
    }

    #[test]
    fn zero_ranges_default_counts_and_quoted_unicode_paths_are_valid() {
        for source in [
            "--- /dev/null\n+++ \"b/λ file.cmake\"\n@@ -0,0 +1 @@\n+set(x y)\n",
            "--- \"a/file\\tname.cmake\"\n+++ /dev/null\n@@ -1 +0,0 @@\n-set(x y)\n",
            "--- \"a/\\303\\251.cmake\"\n+++ b/name\n@@ -1 +1 @@\n-old\n+new\n",
            "--- old\n+++ new\n@@ -1,0 +1,0 @@\n",
        ] {
            assert!(complete(&parse(source.as_bytes())), "{source}");
        }
    }

    #[test]
    fn zero_count_hunks_use_line_boundaries_when_checking_order() {
        for source in [
            b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n@@ -1,0 +2 @@\n+inserted\n".as_slice(),
            b"--- a\n+++ b\n@@ -1,0 +2 @@\n+inserted\n@@ -2 +3 @@\n-old\n+new\n",
            b"--- a\n+++ b\n@@ -1 +0,0 @@\n-deleted\n@@ -2 +1 @@\n-old\n+new\n",
        ] {
            assert!(
                complete(&parse(source)),
                "{}",
                String::from_utf8_lossy(source)
            );
        }
        let backwards = b"--- a\n+++ b\n@@ -1,0 +2 @@\n+inserted\n@@ -1 +3 @@\n-old\n+new\n";
        assert!(has(&parse(backwards), "patch.invalid_fragment"));
    }

    #[test]
    fn incomplete_metadata_and_null_side_body_counts_are_rejected() {
        for source in [
            b"diff --git a/a b/a\nold mode 100644\n".as_slice(),
            b"diff --git a/a b/b\nrename from a\n",
            b"diff --git a/a b/b\ncopy to b\n",
            b"diff --git a/a b/a\nindex 1234..5678\n",
            b"diff --git a/a b/a\nold mode 100644\nold mode 100644\nnew mode 100755\n",
            b"--- /dev/null\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- a\n+++ /dev/null\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- \"\"\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- \"a\" unexpected\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- \"a/\\000.cmake\"\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- \"a/\\400.cmake\"\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
        ] {
            let input = parse(source);
            assert!(
                has(&input, "patch.invalid_fragment"),
                "{}",
                String::from_utf8_lossy(source)
            );
            assert_eq!(input.required_candidate_count(), 3);
        }
    }

    #[test]
    fn long_paths_and_leading_zero_counts_do_not_add_unbounded_plan_units() {
        let path = "a".repeat(2 * CANCELLATION_BYTE_INTERVAL + 1);
        let zeros = "0".repeat(2 * CANCELLATION_BYTE_INTERVAL + 1);
        let source = format!("--- \"{path}\"\n+++ b\n@@ -{zeros}1 +1 @@\n-old\n+new\n");
        let input = parse(source.as_bytes());
        assert!(complete(&input));
        assert!(input.required_candidate_count() < 32);
        assert_eq!(input.max_depth(), 4);
        assert!(input.owned_bytes() < source.len() + 32 * size_of::<QueryCandidate>());
    }

    #[test]
    fn crlf_bom_and_unicode_keep_exact_source_coordinates() {
        let source = "\u{feff}\r\n--- a/λ.cmake\r\n+++ b/λ.cmake\r\n@@ -1 +1 @@ λ\r\n-λ\r\n+μ\r\n"
            .as_bytes();
        let input = parse(source);
        assert!(complete(&input));
        for (before, after) in source.iter().zip(input.parse_bytes()) {
            assert_eq!(
                matches!(before, b'\r' | b'\n'),
                matches!(after, b'\r' | b'\n')
            );
        }
        let added = input
            .candidates
            .iter()
            .find(|c| c.syntax == "patch.added_row")
            .unwrap();
        assert_eq!(&source[added.start..added.end], "+μ\r\n".as_bytes());
    }

    #[test]
    fn annotations_belong_to_the_preceding_physical_side() {
        let input = parse(b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n");
        assert!(complete(&input));
        assert_eq!(
            input
                .candidates
                .iter()
                .filter(|c| c.syntax == "patch.no_newline_row")
                .count(),
            2
        );
        for source in [
            b"--- a\n+++ b\n@@ -1 +1 @@\n\\ No newline at end of file\n-old\n+new\n".as_slice(),
            b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n\\ No newline at end of file\n+new\n",
            b"--- a\n+++ b\n@@ -1,2 +1 @@\n-old\n\\ No newline at end of file\n-more\n+new\n",
        ] {
            assert!(has(&parse(source), "patch.invalid_fragment"));
        }
    }

    #[test]
    fn malformed_or_truncated_patch_never_publishes_a_partial_structure() {
        for source in [
            b"--- a\n".as_slice(),
            b"--- a\n+++ b\n",
            b"--- \n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
            b"--- /dev/null\n+++ /dev/null\n@@ -0,0 +0,0 @@\n",
            b"--- a\n+++ b\n@@ -1,2 +1 @@\n-old\n+new\n",
            b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n+extra\n",
            b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new",
            b"--- a\n+++ b\n@@ -0 +1 @@\n-old\n+new\n",
            b"--- a\n+++ b\n@@ -18446744073709551616 +1 @@\n-old\n+new\n",
            b"--- a\n+++ b\n@@ -18446744073709551615,2 +1 @@\n-old\n-more\n+new\n",
            b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n@@ -1 +2 @@\n-again\n+again\n",
            b"diff --git a/a b/a\nindex nonsense\n",
        ] {
            let input = parse(source);
            assert!(
                has(&input, "patch.invalid_fragment"),
                "{}",
                String::from_utf8_lossy(source)
            );
            assert_eq!(input.candidates.len(), 3);
            assert!(!complete(&input));
        }
    }

    #[test]
    fn unsupported_combined_binary_and_mail_patches_are_explicit() {
        for source in [
            b"diff --cc a.cmake\n".as_slice(),
            b"diff --combined a.cmake\n@@@ -1 -1 +1 @@@\n",
            b"diff --git a/a b/a\nGIT binary patch\nliteral 12\n",
            b"Binary files a/a and b/a differ\n",
            b"Binary files a/a and b/a\n",
            b"*** old.cmake\t2026-01-01\n--- new.cmake\t2026-01-02\n***************\n",
            b"From 0123456789012345678901234567890123456789 Mon Sep 17 00:00:00 2001\nSubject: patch\n",
        ] {
            let input = parse(source);
            assert!(has(&input, "patch.unsupported_fragment"));
            assert_eq!(input.candidates.len(), 3);
        }
    }

    #[test]
    fn orphan_and_truncated_patch_markers_cannot_become_native_cmake() {
        for source in [
            b"---\n-set(removed yes)\n+set(added yes)\n".as_slice(),
            b"---\told.cmake\n+++ new.cmake\n@@ -1 +1 @@\n-old\n+new\n",
            b"+++ b/build.cmake\n+set(added yes)\n",
            b"+++\n+set(added yes)\n",
            b"@@ -1 +1 @@\n-set(removed yes)\n+set(added yes)\n",
            b"diff --git\n-set(removed yes)\n+set(added yes)\n",
        ] {
            let input = parse(source);
            assert!(has(&input, "patch.invalid_fragment"));
            assert_eq!(input.required_candidate_count(), 3);
        }
    }

    #[test]
    fn ordinary_cmake_comments_strings_and_later_markers_are_not_patches() {
        for source in [
            b"# diff --git a/a b/a\nset(x y)\n".as_slice(),
            b"set(text \"--- a\n+++ b\")\n",
            b"#[[\ndiff --git a/a b/a\n]]\n",
            b"set(before yes)\ndiff --git a/a b/a\n",
            b"\"--- a\"\n",
            b"\n\t\n",
        ] {
            assert!(
                CmakePatchInput::new(source, 100, usize::MAX, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn mode_and_rename_only_git_sections_remain_physical_metadata() {
        for source in [
            b"diff --git a/a b/a\nold mode 100644\nnew mode 100755\n".as_slice(),
            b"diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\n",
        ] {
            assert!(complete(&parse(source)));
        }
    }

    #[test]
    fn exhausted_unit_or_memory_credit_retains_only_a_whole_source_gap() {
        let source = b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n";
        let cancellation = Cancellation::new();
        let minimum = source.len() + 3 * size_of::<QueryCandidate>();
        for (units, memory) in [(3, usize::MAX), (1000, minimum)] {
            let input = CmakePatchInput::new(source, units, memory, &cancellation)
                .unwrap()
                .unwrap();
            assert!(has(&input, "patch.structure_limit"));
            assert_eq!(input.required_candidate_count(), 3);
            assert!(input.owned_bytes() <= memory);
        }
        assert!(CmakePatchInput::new(source, 2, usize::MAX, &cancellation).is_err());
        assert!(CmakePatchInput::new(source, 100, minimum - 1, &cancellation).is_err());
    }

    #[test]
    fn cancellation_is_observed_before_detection_and_at_long_line_checkpoints() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(CmakePatchInput::new(b"--- a\n", 100, usize::MAX, &cancellation).is_err());
        let mut source = b"--- ".to_vec();
        source.resize(2 * CANCELLATION_BYTE_INTERVAL + 3, b'x');
        let mut lines = Lines::new(&source, &cancellation);
        assert!(lines.next().is_err());
    }

    #[test]
    fn appending_replaces_host_facts_with_the_mandatory_patch_inventory() {
        let input = parse(b"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n");
        let expected = input.candidates.clone();
        let mut candidates = vec![candidate(0, 1, StructuralRole::Call, "normal_command", 1)];
        input
            .append_candidates(&mut candidates, &Cancellation::new())
            .unwrap();
        assert_eq!(candidates, expected);
        assert!(candidates.iter().all(|candidate| candidate.required));
    }
}
