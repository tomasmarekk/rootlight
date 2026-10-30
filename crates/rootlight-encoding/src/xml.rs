//! Compare a bounded initial XML declaration with independently validated bytes.
//! This gate neither decodes input nor validates the surrounding XML document.

use std::fmt;

/// Maximum UTF-8 declaration bytes inspected after an optional decoded BOM.
pub const MAX_DECLARATION_BYTES: usize = 8192;

/// Physical encoding established by a strict complete-input decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalEncoding {
    /// Validated UTF-8, optionally including its original BOM.
    Utf8,
    /// Validated little-endian UTF-16 with its original BOM.
    Utf16Le,
    /// Validated big-endian UTF-16 with its original BOM.
    Utf16Be,
}

/// Caller-owned observation over every original physical byte, not a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteAscii {
    /// Every byte is ASCII; UTF-8 BOM and UTF-16 inputs cannot supply this proof.
    Proven,
    /// A non-ASCII byte exists or the complete input has not been checked.
    Unproven,
}

/// The declaration cannot establish a compatible interpretation of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationError {
    /// Initial declaration syntax, attribute order, value or uniqueness is invalid.
    Malformed,
    /// The supplied prefix or fixed inspection bound ends inside a declaration.
    Incomplete,
    /// The declared encoding is outside the supported compatibility cases.
    UnsupportedEncoding,
    /// The declaration contradicts the validated physical encoding or BOM.
    EncodingMismatch,
    /// ASCII equivalence requires proof over every original physical byte.
    AsciiProofRequired,
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "malformed initial xml declaration",
            Self::Incomplete => "initial xml declaration is incomplete within its bound",
            Self::UnsupportedEncoding => "xml declaration encoding is unsupported",
            Self::EncodingMismatch => "xml declaration contradicts physical encoding",
            Self::AsciiProofRequired => "xml declaration requires complete physical ascii proof",
        })
    }
}

impl std::error::Error for DeclarationError {}

/// Checks the initial declaration against a separately validated physical encoding.
///
/// `decoded_prefix` starts at physical offset zero after strict UTF-8 or BOM-backed
/// UTF-16 decoding; a decoded leading BOM may remain. At most
/// [`MAX_DECLARATION_BYTES`] are inspected. A declaration is optional, and other
/// processing instructions, including `xml-stylesheet`, do not declare encoding.
/// XML version syntax is `1.` followed by digits; version support and all later
/// document syntax remain the native XML parser's responsibility.
///
/// `complete_ascii` describes all original physical bytes, never this prefix.
/// ISO-8859-1 and US-ASCII are compatible only with BOM-free physical UTF-8 and
/// that complete ASCII proof. No bytes or expressions are decoded or expanded,
/// no allocation occurs, and no language or complete-document claim is made.
///
/// # Errors
/// Returns [`DeclarationError`] for malformed or incomplete declarations,
/// unsupported encoding names, physical mismatches or missing ASCII proof.
pub fn validate_declaration(
    decoded_prefix: &str,
    physical: PhysicalEncoding,
    complete_ascii: CompleteAscii,
) -> Result<(), DeclarationError> {
    let had_bom = decoded_prefix.starts_with('\u{feff}');
    let decoded = decoded_prefix
        .strip_prefix('\u{feff}')
        .unwrap_or(decoded_prefix);
    let bytes = decoded.as_bytes();
    const OPENING: &[u8] = b"<?xml";
    if bytes.len() < OPENING.len() {
        return if !bytes.is_empty() && OPENING.starts_with(bytes) {
            Err(DeclarationError::Incomplete)
        } else {
            Ok(())
        };
    }
    if !bytes[..OPENING.len()].eq_ignore_ascii_case(OPENING) {
        return Ok(());
    }
    if bytes.get(OPENING.len()).is_some_and(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':') || *byte >= 0x80
    }) {
        return Ok(());
    }
    if !bytes.starts_with(OPENING) {
        return Err(DeclarationError::Malformed);
    }
    let mut declaration = Declaration {
        bytes: &bytes[..bytes.len().min(MAX_DECLARATION_BYTES)],
        position: OPENING.len(),
    };
    if !declaration.space() {
        return Err(declaration.expected());
    }
    let (name, version) = declaration.attribute()?;
    if name != b"version"
        || !version.starts_with(b"1.")
        || version.len() < 3
        || !version[2..].iter().all(u8::is_ascii_digit)
    {
        return Err(DeclarationError::Malformed);
    }
    let mut encoding = None;
    let mut standalone = false;
    loop {
        let spaced = declaration.space();
        if declaration.bytes[declaration.position..].starts_with(b"?>") {
            break;
        }
        if b"?>".starts_with(&declaration.bytes[declaration.position..]) {
            return Err(DeclarationError::Incomplete);
        }
        if !spaced {
            return Err(declaration.expected());
        }
        let (name, value) = declaration.attribute()?;
        match name {
            b"encoding" if encoding.is_none() && !standalone => {
                if !value.first().is_some_and(u8::is_ascii_alphabetic)
                    || !value.iter().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(DeclarationError::Malformed);
                }
                encoding = Some(value);
            }
            b"standalone" if !standalone && matches!(value, b"yes" | b"no") => {
                standalone = true;
            }
            _ => return Err(DeclarationError::Malformed),
        }
    }
    let Some(encoding) = encoding else {
        return Ok(());
    };
    if encoding.eq_ignore_ascii_case(b"UTF-8") {
        return compatible(physical == PhysicalEncoding::Utf8);
    }
    if encoding.eq_ignore_ascii_case(b"UTF-16") {
        return compatible(matches!(
            physical,
            PhysicalEncoding::Utf16Le | PhysicalEncoding::Utf16Be
        ));
    }
    if encoding.eq_ignore_ascii_case(b"UTF-16LE") {
        return compatible(physical == PhysicalEncoding::Utf16Le);
    }
    if encoding.eq_ignore_ascii_case(b"UTF-16BE") {
        return compatible(physical == PhysicalEncoding::Utf16Be);
    }
    if encoding.eq_ignore_ascii_case(b"ISO-8859-1") || encoding.eq_ignore_ascii_case(b"US-ASCII") {
        compatible(physical == PhysicalEncoding::Utf8 && !had_bom)?;
        return if complete_ascii == CompleteAscii::Proven {
            Ok(())
        } else {
            Err(DeclarationError::AsciiProofRequired)
        };
    }
    Err(DeclarationError::UnsupportedEncoding)
}

fn compatible(matches: bool) -> Result<(), DeclarationError> {
    if matches {
        Ok(())
    } else {
        Err(DeclarationError::EncodingMismatch)
    }
}

struct Declaration<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Declaration<'a> {
    fn expected(&self) -> DeclarationError {
        if self.position == self.bytes.len() {
            DeclarationError::Incomplete
        } else {
            DeclarationError::Malformed
        }
    }

    fn space(&mut self) -> bool {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.position += 1;
        }
        self.position > start
    }

    fn attribute(&mut self) -> Result<(&'a [u8], &'a [u8]), DeclarationError> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            self.position += 1;
        }
        let name = &self.bytes[start..self.position];
        if name.is_empty() {
            return Err(self.expected());
        }
        self.space();
        if self.bytes.get(self.position) != Some(&b'=') {
            return Err(self.expected());
        }
        self.position += 1;
        self.space();
        let quote = match self.bytes.get(self.position) {
            Some(quote @ (b'\'' | b'"')) => *quote,
            _ => return Err(self.expected()),
        };
        self.position += 1;
        let start = self.position;
        while let Some(byte) = self.bytes.get(self.position) {
            if *byte == quote {
                let value = &self.bytes[start..self.position];
                self.position += 1;
                return Ok((name, value));
            }
            if !byte.is_ascii() || matches!(byte, b'<' | b'&') {
                return Err(DeclarationError::Malformed);
            }
            self.position += 1;
        }
        Err(DeclarationError::Incomplete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_declarations_and_other_processing_instructions_stay_outside_this_gate() {
        for source in [
            "",
            "<root/>",
            "<?xml-stylesheet encoding='unknown'?>\n<root/>",
            "<?XML-stylesheet href='style'?>",
            "<?xmlé value='other-target'?>",
            "<root><?xml version='broken'?></root>",
            "<!-- earlier comment --><?xml version='broken'?>",
        ] {
            for physical in [
                PhysicalEncoding::Utf8,
                PhysicalEncoding::Utf16Le,
                PhysicalEncoding::Utf16Be,
            ] {
                assert_eq!(
                    validate_declaration(source, physical, CompleteAscii::Unproven),
                    Ok(()),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn unicode_declarations_require_the_independently_validated_physical_encoding() {
        for physical in [
            PhysicalEncoding::Utf8,
            PhysicalEncoding::Utf16Le,
            PhysicalEncoding::Utf16Be,
        ] {
            for (encoding, matches) in [
                ("UTF-8", physical == PhysicalEncoding::Utf8),
                ("utf-16", physical != PhysicalEncoding::Utf8),
                ("UTF-16LE", physical == PhysicalEncoding::Utf16Le),
                ("utf-16be", physical == PhysicalEncoding::Utf16Be),
            ] {
                let source =
                    format!("<?xml version='1.1' encoding='{encoding}' standalone='yes'?>");
                assert_eq!(
                    validate_declaration(&source, physical, CompleteAscii::Proven),
                    compatible(matches)
                );
            }
        }
        for source in [
            "<?xml version='1.999'?>",
            "\u{feff}<?xml version = \"1.0\" encoding = 'uTf-8' standalone = \"no\" ?>",
            "<?xml\nversion\t=\r'1.0'\nstandalone='yes'?>",
        ] {
            assert_eq!(
                validate_declaration(source, PhysicalEncoding::Utf8, CompleteAscii::Unproven),
                Ok(())
            );
        }
    }

    #[test]
    fn ascii_equivalence_requires_complete_original_bytes_and_rejects_a_bom() {
        for encoding in ["ISO-8859-1", "US-ASCII", "iso-8859-1", "us-ascii"] {
            let source = format!("<?xml version='1.0' encoding='{encoding}'?><system/>");
            assert_eq!(
                validate_declaration(&source, PhysicalEncoding::Utf8, CompleteAscii::Proven),
                Ok(())
            );
            assert_eq!(
                validate_declaration(&source, PhysicalEncoding::Utf8, CompleteAscii::Unproven),
                Err(DeclarationError::AsciiProofRequired)
            );
            assert_eq!(
                validate_declaration(
                    &format!("\u{feff}{source}"),
                    PhysicalEncoding::Utf8,
                    CompleteAscii::Proven
                ),
                Err(DeclarationError::EncodingMismatch)
            );
            for physical in [PhysicalEncoding::Utf16Le, PhysicalEncoding::Utf16Be] {
                assert_eq!(
                    validate_declaration(&source, physical, CompleteAscii::Proven),
                    Err(DeclarationError::EncodingMismatch)
                );
            }
        }
    }

    #[test]
    fn late_non_ascii_cannot_borrow_an_ascii_prefix_as_a_complete_proof() {
        let source = format!(
            "<?xml version='1.0' encoding='ISO-8859-1'?><system>{}é</system>",
            " ".repeat(MAX_DECLARATION_BYTES)
        );
        assert!(source.as_bytes()[..MAX_DECLARATION_BYTES].is_ascii());
        assert!(!source.is_ascii());
        assert_eq!(
            validate_declaration(
                &source[..MAX_DECLARATION_BYTES],
                PhysicalEncoding::Utf8,
                CompleteAscii::Unproven
            ),
            Err(DeclarationError::AsciiProofRequired)
        );
        assert_eq!(
            validate_declaration(&source, PhysicalEncoding::Utf8, CompleteAscii::Unproven),
            Err(DeclarationError::AsciiProofRequired)
        );
        assert_eq!(
            validate_declaration(
                "<?xml version='1.0' encoding='ISO-8859-1'?><é/>",
                PhysicalEncoding::Utf8,
                CompleteAscii::Unproven
            ),
            Err(DeclarationError::AsciiProofRequired)
        );
    }

    #[test]
    fn unsupported_encoding_names_never_inherit_ascii_equivalence() {
        for encoding in ["UTF-32", "windows-1252", "latin1", "ISO-8859-2", "UTF8"] {
            let source = format!("<?xml version='1.0' encoding='{encoding}'?>");
            assert_eq!(
                validate_declaration(&source, PhysicalEncoding::Utf8, CompleteAscii::Proven),
                Err(DeclarationError::UnsupportedEncoding)
            );
        }
    }

    #[test]
    fn malformed_declarations_preserve_mandatory_version_order_and_unique_attributes() {
        for source in [
            "<?XML version='1.0'?>",
            "<?Xml version='1.0'?>",
            "<?xml?>",
            "<?xml version=1.0?>",
            "<?xml version='2.0'?>",
            "<?xml version='1.'?>",
            "<?xml version='1.a'?>",
            "<?xml version='1.0'version='1.0'?>",
            "<?xml version='1.0' version='1.0'?>",
            "<?xml encoding='UTF-8' version='1.0'?>",
            "<?xml version='1.0' encoding='UTF-8' encoding='UTF-8'?>",
            "<?xml version='1.0' standalone='no' encoding='UTF-8'?>",
            "<?xml version='1.0' standalone='yes' standalone='yes'?>",
            "<?xml version='1.0' standalone='true'?>",
            "<?xml version='1.0' extra='value'?>",
            "<?xml version='1.0' encoding=''?>",
            "<?xml version='1.0' encoding='8UTF'?>",
            "<?xml version='1.0' encoding='UTF 8'?>",
            "<?xml version='1.0' encoding='UTF&x;'?>",
            "<?xml version='1.0' encoding='é'?>",
        ] {
            assert_eq!(
                validate_declaration(source, PhysicalEncoding::Utf8, CompleteAscii::Proven),
                Err(DeclarationError::Malformed),
                "{source}"
            );
        }
    }

    #[test]
    fn incomplete_declarations_do_not_borrow_missing_delimiters() {
        for source in [
            "<?xml",
            "<?xml ",
            "<?xml version",
            "<?xml version=",
            "<?xml version='1.0",
            "<?xml version='1.0'",
            "<?xml version='1.0' ?",
            "<?xml version='1.0' encoding='UTF-8",
        ] {
            assert_eq!(
                validate_declaration(source, PhysicalEncoding::Utf8, CompleteAscii::Proven),
                Err(DeclarationError::Incomplete),
                "{source}"
            );
        }
    }

    #[test]
    fn declarations_must_close_inside_the_fixed_inspection_bound() {
        let opening = "<?xml version='1.0'";
        let source = format!(
            "{opening}{}?>",
            " ".repeat(MAX_DECLARATION_BYTES - opening.len() - 2)
        );
        assert_eq!(source.len(), MAX_DECLARATION_BYTES);
        assert_eq!(
            validate_declaration(&source, PhysicalEncoding::Utf8, CompleteAscii::Unproven),
            Ok(())
        );
        let beyond = source.replacen("?>", " ?>", 1);
        assert_eq!(
            validate_declaration(&beyond, PhysicalEncoding::Utf8, CompleteAscii::Unproven),
            Err(DeclarationError::Incomplete)
        );
    }
}
