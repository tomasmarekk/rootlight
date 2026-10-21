//! Inert parser input keeps physical Fypp syntax from erasing literal Fortran.
//! The unchanged decoded source remains the query and evidence authority;
//! placeholders neither execute templates nor supply authored identities.

use std::{
    borrow::Cow,
    ops::{ControlFlow, Range},
};

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;

use crate::source_view::NativeSourceView;

use super::{CANCELLATION_BYTE_INTERVAL, provider_failure, require_provider_limit};

pub(crate) const FORTRAN_TEMPLATE_PROJECTION_POLICY: &str =
    "rootlight.treesitter.fortran-template-parse-input/1";

pub(super) struct FortranParseInput<'a> {
    bytes: Cow<'a, [u8]>,
}

impl<'a> FortranParseInput<'a> {
    pub(super) fn new(
        source: &'a NativeSourceView<'_>,
        max_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        admit_capacity(source.owned_bytes(), 0, max_owned_bytes)?;
        let original = source.as_bytes();
        if !needs_projection(original, cancellation)? {
            return Ok(Self {
                bytes: Cow::Borrowed(original),
            });
        }
        admit_capacity(source.owned_bytes(), original.len(), max_owned_bytes)?;
        cancellation.check()?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(original.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        admit_capacity(source.owned_bytes(), bytes.capacity(), max_owned_bytes)?;
        for chunk in original.chunks(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
            bytes.extend_from_slice(chunk);
        }
        walk_masks(original, cancellation, |mask| {
            match mask {
                Mask::Directive(index) => {
                    *bytes
                        .get_mut(index)
                        .ok_or_else(|| provider_failure("fortran-projection-span"))? = b'!';
                }
                Mask::Interpolation(range) => {
                    let target = bytes
                        .get_mut(range)
                        .ok_or_else(|| provider_failure("fortran-projection-span"))?;
                    for chunk in target.chunks_mut(CANCELLATION_BYTE_INTERVAL) {
                        cancellation.check()?;
                        for byte in chunk {
                            // ASCII placeholders preserve every original Unicode
                            // scalar and newline boundary, including UTF-16 maps.
                            if byte.is_ascii()
                                && !byte.is_ascii_alphanumeric()
                                && !matches!(*byte, b'_' | b'\r' | b'\n')
                            {
                                *byte = b'x';
                            }
                        }
                    }
                }
            }
            Ok(ControlFlow::Continue(()))
        })?;
        cancellation.check()?;
        Ok(Self {
            bytes: Cow::Owned(bytes),
        })
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn admit_capacity(existing: usize, added: usize, maximum: usize) -> Result<(), AdapterError> {
    let total = existing
        .checked_add(added)
        .ok_or(SinkError::AccountingOverflow)?;
    require_provider_limit(ResourceKind::ReportedMemoryBytes, total, maximum)
}

pub(super) fn needs_projection(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut found = false;
    walk_masks(source, cancellation, |_| {
        found = true;
        Ok(ControlFlow::Break(()))
    })?;
    Ok(found)
}

enum Mask {
    Directive(usize),
    Interpolation(Range<usize>),
}

fn walk_masks(
    source: &[u8],
    cancellation: &Cancellation,
    mut visit: impl FnMut(Mask) -> Result<ControlFlow<()>, AdapterError>,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    let mut index = if source.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let mut line_start = true;
    let mut quote = None;
    let mut unterminated_sigils = 0u8;
    while let Some(byte) = source.get(index).copied() {
        check_scan(index, cancellation)?;
        if matches!(byte, b'\r' | b'\n') {
            line_start = true;
            index += 1;
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == delimiter {
                if source.get(index + 1) == Some(&delimiter) {
                    index += 2;
                    continue;
                }
                quote = None;
            }
            line_start = false;
            index += 1;
            continue;
        }
        if line_start && matches!(byte, b' ' | b'\t' | b'\x0b' | b'\x0c') {
            index += 1;
            continue;
        }
        if line_start && is_sigil(byte) && source.get(index + 1) == Some(&b':') {
            if visit(Mask::Directive(index))?.is_break() {
                cancellation.check()?;
                return Ok(());
            }
            index = line_end(source, index, cancellation)?;
            line_start = false;
            continue;
        }
        line_start = false;
        if byte == b'!' {
            index = line_end(source, index, cancellation)?;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if is_sigil(byte)
            && source.get(index + 1) == Some(&b'{')
            && unterminated_sigils & sigil_bit(byte) == 0
        {
            if let Some(end) = interpolation_end(source, index + 2, byte, cancellation)? {
                if visit(Mask::Interpolation(index..end))?.is_break() {
                    cancellation.check()?;
                    return Ok(());
                }
                index = end;
                continue;
            }
            // An unmatched opener is not an invented complete expression.
            // Leave its remaining physical line untouched for recovery/gaps.
            // Its sigil has no later closer, so do not rescan the full suffix
            // for every subsequent malformed line of the same template kind.
            unterminated_sigils |= sigil_bit(byte);
            index = line_end(source, index, cancellation)?;
            continue;
        }
        index += 1;
    }
    cancellation.check()?;
    Ok(())
}

const fn is_sigil(byte: u8) -> bool {
    matches!(byte, b'$' | b'@' | b'#')
}

const fn sigil_bit(byte: u8) -> u8 {
    match byte {
        b'$' => 1,
        b'@' => 2,
        b'#' => 4,
        _ => 0,
    }
}

fn line_end(
    source: &[u8],
    mut index: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    while let Some(byte) = source.get(index) {
        check_scan(index, cancellation)?;
        if matches!(byte, b'\r' | b'\n') {
            break;
        }
        index += 1;
    }
    Ok(index)
}

fn interpolation_end(
    source: &[u8],
    mut index: usize,
    sigil: u8,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    while let Some(byte) = source.get(index) {
        check_scan(index, cancellation)?;
        if *byte == b'}' && source.get(index + 1) == Some(&sigil) {
            return Ok(Some(index + 2));
        }
        index += 1;
    }
    Ok(None)
}

fn check_scan(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    // Doubled quotes advance two bytes and can skip the exact checkpoint.
    if index % CANCELLATION_BYTE_INTERVAL <= 1 {
        cancellation.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rootlight_cancel::CancellationReason;
    use rootlight_ids::content_hash;

    use super::*;
    use crate::runtime::{CachedParse, invalid_identity, prepare_reuse};
    use crate::source_view::NATIVE_SOURCE_PROJECTION_POLICY;
    use crate::{
        GrammarFamily, ReuseInvalidation, ReuseStatus, SourceEdit, registry::language_for,
    };

    fn view<'a>(source: &'a [u8]) -> NativeSourceView<'a> {
        NativeSourceView::new(
            source,
            "utf-8",
            source.len(),
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap()
    }

    fn projected(source: &str) -> Vec<u8> {
        let original = view(source.as_bytes());
        FortranParseInput::new(&original, usize::MAX, &Cancellation::new())
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn ordinary_source_borrows_without_allocation_or_literal_rewrites() {
        let source = concat!(
            "module Literal\n",
            "  ! #:set ignored = '${comment}$'\n",
            "  character(*), parameter :: first = 'it''s ${literal}$'\n",
            "  character(*), parameter :: second = \"a\"\"b @{literal}@\"\n",
            "  character(*), parameter :: continued = 'text &\n & ${literal}$'\n",
            "end module Literal\n",
        );
        let original = view(source.as_bytes());
        let input = FortranParseInput::new(&original, 0, &Cancellation::new()).unwrap();
        assert!(matches!(input.bytes, Cow::Borrowed(_)));
        assert_eq!(input.as_bytes(), source.as_bytes());
        assert!(!needs_projection(source.as_bytes(), &Cancellation::new()).unwrap());
    }

    #[test]
    fn directives_preserve_bom_unicode_crlf_and_original_source() {
        let source = "\u{feff}#:set NAME = 'λ🙂'\r\n\t $:ignored\r\n  @:ignored\r\nmodule Literal\r\nend module Literal\r\n";
        let expected = "\u{feff}!:set NAME = 'λ🙂'\r\n\t !:ignored\r\n  !:ignored\r\nmodule Literal\r\nend module Literal\r\n";
        let original = view(source.as_bytes());
        let input = FortranParseInput::new(&original, usize::MAX, &Cancellation::new()).unwrap();
        assert_eq!(input.as_bytes(), expected.as_bytes());
        assert_eq!(input.as_bytes().len(), source.len());
        assert_eq!(original.as_bytes(), source.as_bytes());
        assert_eq!(
            std::str::from_utf8(input.as_bytes())
                .unwrap()
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            source
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn adjacent_and_multiline_interpolations_are_inert_but_unmatched_text_is_not_completed() {
        let source = "Generated_${name}$@{other}@#{third}# = ${one +\r\ntwo}$\n${broken\n";
        let expected = "Generated_xxnamexxxxotherxxxxthirdxx = xxonexx\r\ntwoxx\n${broken\n";
        assert_eq!(projected(source), expected.as_bytes());
        assert_eq!(projected("${unfinished"), b"${unfinished");
        assert_eq!(projected("${λ + 🙂}$"), "xxλxxx🙂xx".as_bytes());
        let malformed = "${unfinished\n".repeat(2048);
        assert_eq!(projected(&malformed), malformed.as_bytes());
        assert_eq!(
            projected("${unfinished\n@:directive\n"),
            b"${unfinished\n!:directive\n"
        );
    }

    #[test]
    fn joint_capacity_is_checked_before_allocation_and_against_actual_capacity() {
        assert!(admit_capacity(17, 29, 46).is_ok());
        assert!(admit_capacity(17, 29, 45).is_err());
        assert!(matches!(
            admit_capacity(usize::MAX, 1, usize::MAX),
            Err(AdapterError::Sink(SinkError::AccountingOverflow))
        ));
        let source = "#:set VALUE = 'λ'\nmodule Literal\nend module Literal\n";
        let raw: Vec<_> = std::iter::once(0xfeff)
            .chain(source.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let original =
            NativeSourceView::new(&raw, "utf-16le", 4096, usize::MAX, &Cancellation::new())
                .unwrap();
        assert!(original.owned_bytes() > 0);
        let needed = original
            .owned_bytes()
            .checked_add(original.as_bytes().len())
            .unwrap();
        assert!(FortranParseInput::new(&original, needed - 1, &Cancellation::new()).is_err());
        let input = FortranParseInput::new(&original, usize::MAX, &Cancellation::new()).unwrap();
        let Cow::Owned(bytes) = &input.bytes else {
            panic!("directive needs a parser-only buffer")
        };
        let actual = original
            .owned_bytes()
            .checked_add(bytes.capacity())
            .unwrap();
        assert!(FortranParseInput::new(&original, actual, &Cancellation::new()).is_ok());
        assert!(admit_capacity(original.owned_bytes(), bytes.capacity(), actual - 1).is_err());
        assert_eq!(
            original
                .to_physical_offset(original.as_str().find("module").unwrap())
                .unwrap(),
            raw.windows(2).position(|pair| pair == [b'm', 0]).unwrap()
        );
    }

    #[test]
    fn cancellation_rejects_empty_and_long_projection_inputs() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let original = view(b"#:set VALUE = 1\n");
        for source in [b"".as_slice(), original.as_bytes()] {
            assert!(matches!(
                needs_projection(source, &cancellation),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
        }
        assert!(matches!(
            FortranParseInput::new(&original, usize::MAX, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        let long = format!("${{{}}}$", "x".repeat(CANCELLATION_BYTE_INTERVAL * 2));
        assert!(needs_projection(long.as_bytes(), &Cancellation::new()).unwrap());
        let mut visits = 0;
        assert!(
            walk_masks(b"#:first\n#:second\n", &cancellation, |_| {
                visits += 1;
                Ok(ControlFlow::Continue(()))
            })
            .is_err()
        );
        assert_eq!(visits, 0);
        let active = Cancellation::new();
        let source = format!(
            "  #:first\n'{}'\n#:second\n",
            "''".repeat(CANCELLATION_BYTE_INTERVAL)
        );
        let mut visits = 0;
        assert!(matches!(
            walk_masks(source.as_bytes(), &active, |_| {
                visits += 1;
                active.cancel(CancellationReason::ClientRequest);
                Ok(ControlFlow::Continue(()))
            }),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(visits, 1);
    }

    fn native_tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Fortran))
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn directive_projection_retains_literal_native_module_and_procedure_structure() {
        let source = "#:set KIND = 'integer'\nmodule Literal\ncontains\n  subroutine Stable(Value)\n    integer :: Value\n  end subroutine Stable\n  subroutine generated_${KIND}$(Value)\n    integer :: Value\n  end subroutine generated_${KIND}$\nend module Literal\n";
        let original = view(source.as_bytes());
        let input = FortranParseInput::new(&original, usize::MAX, &Cancellation::new()).unwrap();
        let tree = native_tree(input.as_bytes());
        let root = tree.root_node();
        let mut cursor = root.walk();
        let module = root
            .named_children(&mut cursor)
            .find(|node| node.kind() == "module")
            .expect("literal module survives inert directive");
        assert_eq!(module.start_byte(), source.find("module Literal").unwrap());
        let mut cursor = module.walk();
        let procedures = module
            .named_children(&mut cursor)
            .find(|node| node.kind() == "internal_procedures")
            .expect("module retains its native procedure container");
        let mut cursor = procedures.walk();
        let stable = procedures
            .named_children(&mut cursor)
            .find(|node| node.kind() == "subroutine")
            .expect("literal procedure retains native owner");
        assert_eq!(
            stable.start_byte(),
            source.find("subroutine Stable").unwrap()
        );
        assert_eq!(original.as_bytes(), source.as_bytes());
    }

    fn cached(source: &[u8]) -> CachedParse {
        let mut identity = invalid_identity();
        identity.family = GrammarFamily::Fortran;
        identity.encoding = "utf-8".to_owned();
        identity.content_hash = content_hash(source);
        let original = view(source);
        let input = FortranParseInput::new(&original, usize::MAX, &Cancellation::new()).unwrap();
        CachedParse {
            entry_id: 1,
            projection_policy: NATIVE_SOURCE_PROJECTION_POLICY,
            c_preprocessor_projection_policy: crate::runtime::C_PREPROCESSOR_PROJECTION_POLICY,
            fortran_projection_policy: FORTRAN_TEMPLATE_PROJECTION_POLICY,
            cmake_header_projection_policy: crate::runtime::CMAKE_HEADER_PROJECTION_POLICY,
            cmake_patch_projection_policy: crate::runtime::CMAKE_PATCH_PROJECTION_POLICY,
            cmake_command_template_projection_policy:
                crate::runtime::CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY,
            gdscript_scanner_contract: crate::runtime::gdscript_scope::CACHE_CONTRACT_IDENTITY,
            dockerfile_scanner_contract:
                crate::runtime::dockerfile_scope::SCANNER_CONTRACT_IDENTITY,
            pug_scanner_contract: crate::runtime::pug_scope::SCANNER_CONTRACT_IDENTITY,
            lean_scanner_contract: crate::runtime::lean_scope::SCANNER_CONTRACT_IDENTITY,
            wolfram_scanner_contract: crate::runtime::wolfram_scope::SCANNER_CONTRACT_IDENTITY,
            idris_scanner_contract: crate::runtime::idris_scope::SCANNER_CONTRACT_IDENTITY,
            idris_frontend_policy: crate::runtime::idris_scope::FRONTEND_POLICY,
            sed_scanner_contract: crate::runtime::sed_scope::SCANNER_CONTRACT_IDENTITY,
            sed_frontend_policy: crate::runtime::sed_scope::FRONTEND_POLICY,
            coffeescript_scanner_contract:
                crate::runtime::coffeescript_scope::SCANNER_CONTRACT_IDENTITY,
            identity,
            source: Arc::new(source.to_vec()),
            tree: Some(native_tree(input.as_bytes())),
            accounted_bytes: source.len(),
            invalidation: None,
        }
    }

    #[test]
    fn edited_projection_transitions_invalidate_but_exact_noop_and_plain_edits_reuse() {
        let plain = "module Literal\nend module Literal\n";
        let directive = "#:set VALUE = 1\n";
        let template = format!("{directive}{plain}");
        let cached_template = cached(template.as_bytes());
        let (tree, status) = prepare_reuse(
            Some(&cached_template),
            &cached_template.identity,
            template.as_bytes(),
            &[],
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(tree.is_some());
        assert!(matches!(status, ReuseStatus::Reused { .. }));
        for (old, current, edit) in [
            (
                plain,
                template.as_str(),
                SourceEdit::new(0, 0, directive).unwrap(),
            ),
            (
                template.as_str(),
                plain,
                SourceEdit::new(0, directive.len(), "").unwrap(),
            ),
            (
                template.as_str(),
                template.as_str(),
                SourceEdit::new(0, 0, "").unwrap(),
            ),
        ] {
            let previous = cached(old.as_bytes());
            let mut identity = previous.identity.clone();
            identity.content_hash = content_hash(current.as_bytes());
            let (tree, status) = prepare_reuse(
                Some(&previous),
                &identity,
                current.as_bytes(),
                &[edit],
                usize::MAX,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(tree.is_none());
            assert_eq!(
                status,
                ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
            );
        }
        let previous = cached(plain.as_bytes());
        let current = format!("! ordinary comment\n{plain}");
        let mut identity = previous.identity.clone();
        identity.content_hash = content_hash(current.as_bytes());
        let (tree, status) = prepare_reuse(
            Some(&previous),
            &identity,
            current.as_bytes(),
            &[SourceEdit::new(0, 0, "! ordinary comment\n").unwrap()],
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(tree.is_some());
        assert!(matches!(status, ReuseStatus::Reused { .. }));
    }

    #[test]
    fn old_fortran_projection_policy_cannot_reuse_cached_native_tree() {
        let source = b"module Literal\nend module Literal\n";
        let mut previous = cached(source);
        previous.fortran_projection_policy = "rootlight.treesitter.fortran-template-parse-input/0";
        let (tree, status) = prepare_reuse(
            Some(&previous),
            &previous.identity,
            source,
            &[],
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(tree.is_none());
        assert_eq!(
            status,
            ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
        );
    }
}
