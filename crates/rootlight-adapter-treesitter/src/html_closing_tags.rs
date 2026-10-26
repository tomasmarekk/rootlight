//! Prove explicit HTML closing names from the complete current native tree.
//! Producer and consumer share this bounded CST predicate, while the consumer
//! reparses current source and independently joins the supplied IR witnesses.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

/// Exact native extents for one explicit pair and its nearest-first element owners.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Proof {
    pub(crate) element: (usize, usize),
    pub(crate) opening: (usize, usize),
    pub(crate) opening_name: (usize, usize),
    pub(crate) closing: (usize, usize),
    pub(crate) closing_name: (usize, usize),
    pub(crate) ancestors: Vec<(usize, usize)>,
}

struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn bytes(&mut self, raw: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn span(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

fn html_space(byte: &u8) -> bool {
    matches!(*byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

fn name(raw: &[u8]) -> bool {
    raw.first().is_some_and(u8::is_ascii_alphabetic)
        && std::str::from_utf8(raw).is_ok()
        && raw
            .iter()
            .all(|byte| !html_space(byte) && !b"\0/><".contains(byte))
}

fn foreign_or_scripting(raw: &[u8]) -> bool {
    [b"svg".as_slice(), b"math", b"noscript"]
        .iter()
        .any(|name| raw.eq_ignore_ascii_case(name))
}

fn is_void(raw: &[u8]) -> bool {
    // The pinned scanner's END_OF_VOID_TAGS boundary includes legacy spellings.
    // Keeping that exact set permits source <meta> without inventing an end tag.
    [
        b"area".as_slice(),
        b"base",
        b"basefont",
        b"bgsound",
        b"br",
        b"col",
        b"command",
        b"embed",
        b"frame",
        b"hr",
        b"image",
        b"img",
        b"input",
        b"isindex",
        b"keygen",
        b"link",
        b"menuitem",
        b"meta",
        b"nextid",
        b"param",
        b"source",
        b"track",
        b"wbr",
    ]
    .iter()
    .any(|name| raw.eq_ignore_ascii_case(name))
}

fn written_name<'tree>(
    tag: Node<'tree>,
    source: &[u8],
    closing: bool,
    self_closing: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if tag.is_missing() || tag.has_error() || source.get(tag.byte_range()).is_none() {
        return Ok(None);
    }
    let mut found = None;
    let mut cursor = tag.walk();
    for child in tag.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if child.kind() == "tag_name" && found.replace(child).is_some() {
            return Ok(None);
        }
    }
    let Some(written) = found else {
        return Ok(None);
    };
    let Some(raw) = source.get(written.byte_range()) else {
        return Ok(None);
    };
    if written.is_missing() || !work.bytes(raw, cancel)? || !name(raw) {
        return Ok(None);
    }
    let prefix = if closing {
        b"</".as_slice()
    } else {
        b"<".as_slice()
    };
    if source.get(tag.start_byte()..written.start_byte()) != Some(prefix) {
        return Ok(None);
    }
    let Some(suffix) = source.get(written.end_byte()..tag.end_byte()) else {
        return Ok(None);
    };
    if closing {
        let Some(space) = suffix.strip_suffix(b">") else {
            return Ok(None);
        };
        if tag.named_child_count() != 1 {
            return Ok(None);
        }
        for chunk in space.chunks(64) {
            if !work.step(cancel)? || !chunk.iter().all(html_space) {
                return Ok(None);
            }
        }
    } else if self_closing {
        if !suffix.ends_with(b"/>") {
            return Ok(None);
        }
    } else if !suffix.ends_with(b">") {
        return Ok(None);
    }
    Ok(Some(written))
}

enum Element {
    Paired(Proof),
    Leaf,
}

fn element(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Element>, AdapterError> {
    let mut opening = None;
    let mut closing = None;
    let mut self_closing = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let duplicate = match child.kind() {
            "start_tag" => opening.replace(child).is_some(),
            "end_tag" => closing.replace(child).is_some(),
            "self_closing_tag" => self_closing.replace(child).is_some(),
            _ => false,
        };
        if duplicate {
            return Ok(None);
        }
    }
    if let Some(tag) = self_closing {
        if opening.is_some() || closing.is_some() || span(tag) != span(node) {
            return Ok(None);
        }
        let Some(written) = written_name(tag, source, false, true, work, cancel)? else {
            return Ok(None);
        };
        if source
            .get(written.byte_range())
            .is_some_and(foreign_or_scripting)
        {
            return Ok(None);
        }
        return Ok(Some(Element::Leaf));
    }
    let Some(opening) = opening else {
        return Ok(None);
    };
    let Some(opening_name) = written_name(opening, source, false, false, work, cancel)? else {
        return Ok(None);
    };
    let Some(opening_text) = source.get(opening_name.byte_range()) else {
        return Ok(None);
    };
    if opening.start_byte() != node.start_byte() || foreign_or_scripting(opening_text) {
        return Ok(None);
    }
    if is_void(opening_text) {
        return Ok(closing.is_none().then_some(Element::Leaf));
    }
    let Some(closing) = closing else {
        // The scanner can pop a nonvoid element implicitly before another close.
        // Such a document cannot supply an unambiguous explicit-pair certificate.
        return Ok(None);
    };
    let Some(closing_name) = written_name(closing, source, true, false, work, cancel)? else {
        return Ok(None);
    };
    let Some(closing_text) = source.get(closing_name.byte_range()) else {
        return Ok(None);
    };
    if closing.end_byte() != node.end_byte()
        || opening.end_byte() > closing.start_byte()
        || !opening_text.eq_ignore_ascii_case(closing_text)
    {
        return Ok(None);
    }
    Ok(Some(Element::Paired(Proof {
        element: span(node),
        opening: span(opening),
        opening_name: span(opening_name),
        closing: span(closing),
        closing_name: span(closing_name),
        ancestors: Vec::new(),
    })))
}

fn is_element(node: Node<'_>) -> bool {
    matches!(node.kind(), "element" | "script_element" | "style_element")
}

fn collect(
    root: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Vec<Proof>>, AdapterError> {
    if !work.step(cancel)? {
        return Ok(None);
    }
    if root.kind() != "document"
        || root.parent().is_some()
        || root.has_error()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(None);
    }
    let mut proofs = Vec::new();
    let mut ancestors = Vec::new();
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let node = cursor.node();
        if node.is_missing()
            || node.is_error()
            || node.kind() == "erroneous_end_tag"
            || source.get(node.byte_range()).is_none()
        {
            return Ok(None);
        }
        if is_element(node) {
            let Some(shape) = element(node, source, work, cancel)? else {
                return Ok(None);
            };
            if let Element::Paired(mut proof) = shape {
                proof
                    .ancestors
                    .try_reserve(ancestors.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                for (_, owner) in ancestors.iter().rev() {
                    if !work.step(cancel)? {
                        return Ok(None);
                    }
                    proof.ancestors.push(*owner);
                }
                proofs
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                proofs.push(proof);
            }
            if !work.step(cancel)? {
                return Ok(None);
            }
            ancestors
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ancestors.push((node.id(), span(node)));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if !work.step(cancel)? {
                return Ok(None);
            }
            let finished = cursor.node();
            if is_element(finished) && ancestors.pop().map(|(id, _)| id) != Some(finished.id()) {
                return Ok(None);
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(Some(proofs));
            }
        }
    }
}

/// Return all explicit current-tree pairs, or atomically refuse unsupported input.
///
/// The bound charges native visits, copied ancestry and written name bytes. Actual
/// cancellation and allocation errors propagate; refused or exhausted work never
/// returns a partial certificate list. UTF-8 native extents remain logical bytes.
pub(crate) fn census(
    root: Node<'_>,
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Option<Vec<Proof>>, AdapterError> {
    Ok(census_with_status(root, source, maximum, cancel)?.0)
}

/// Preserve the distinction between an unsupported document and work exhaustion.
pub(crate) fn census_with_status(
    root: Node<'_>,
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<(Option<Vec<Proof>>, bool), AdapterError> {
    let mut work = Work {
        remaining: maximum,
        exhausted: false,
    };
    let proofs = collect(root, source, &mut work, cancel)?;
    Ok((proofs, work.exhausted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_html::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn explicit_pairs_keep_nearest_native_owners_and_opening_spelling() {
        let source = b"<MAIN><section><Part></pArT></section><Part></Part></main>";
        let native = tree(source);
        let proofs = census(native.root_node(), source, 4096, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(proofs.len(), 4);
        let main = &proofs[0];
        let section = &proofs[1];
        let first = &proofs[2];
        let second = &proofs[3];
        assert_eq!(main.element, (0, source.len()));
        assert!(main.ancestors.is_empty());
        assert_eq!(first.ancestors, [section.element, main.element]);
        assert_eq!(second.ancestors, [main.element]);
        assert_ne!(first.opening_name, second.opening_name);
        assert_eq!(
            source.get(first.opening_name.0..first.opening_name.1),
            Some(b"Part".as_slice())
        );
        assert_eq!(
            source.get(first.closing_name.0..first.closing_name.1),
            Some(b"pArT".as_slice())
        );
    }

    #[test]
    fn void_headers_and_raw_text_do_not_supply_extra_closing_names() {
        let source = b"<!doctype html><html><head><meta charset='utf-8'><title>kept</title></head><body><script>const value='</head>'; /* <p></p> */</script><style>x::after{content:'</main>'}</style><custom/></body></html>";
        let native = tree(source);
        let proofs = census(native.root_node(), source, 8192, &Cancellation::new())
            .unwrap()
            .unwrap();
        let names = proofs
            .iter()
            .map(|proof| {
                source
                    .get(proof.closing_name.0..proof.closing_name.1)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                b"html".as_slice(),
                b"head",
                b"title",
                b"body",
                b"script",
                b"style"
            ]
        );
        assert!(!names.contains(&b"meta".as_slice()));
        assert!(!names.contains(&b"custom".as_slice()));
    }

    #[test]
    fn implicit_nonvoid_foreign_and_mismatched_documents_refuse_atomically() {
        for source in [
            "<main><p>implicit</main>",
            "<main></main></wrong>",
            "<main><item></main></item>",
            "<main><svg><g></g></svg></main>",
            "<main><math><mi>x</mi></math></main>",
            "<main><noscript><p>x</p></noscript></main>",
            "<main><plaintext>no explicit boundary</main>",
            "<main><unclosed>",
            "<aé></aÉ>",
        ] {
            let native = tree(source.as_bytes());
            let (proofs, exhausted) = census_with_status(
                native.root_node(),
                source.as_bytes(),
                8192,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(proofs.is_none(), "{source}");
            assert!(
                !exhausted,
                "unsupported syntax is not a work limit: {source}"
            );
        }
    }

    #[test]
    fn bom_crlf_and_utf8_names_keep_native_byte_coordinates() {
        let source = "\u{feff}<main>λ😀\r\n<aé></aé>\r\n</main>".as_bytes();
        let native = tree(source);
        let proofs = census(native.root_node(), source, 4096, &Cancellation::new())
            .unwrap()
            .unwrap();
        let inner = proofs
            .iter()
            .find(|proof| {
                source.get(proof.opening_name.0..proof.opening_name.1) == Some("aé".as_bytes())
            })
            .unwrap();
        let opening = source
            .windows("<aé>".len())
            .position(|raw| raw == "<aé>".as_bytes())
            .unwrap()
            + 1;
        let closing = source
            .windows("</aé>".len())
            .position(|raw| raw == "</aé>".as_bytes())
            .unwrap()
            + 2;
        assert_eq!(inner.opening_name, (opening, opening + "aé".len()));
        assert_eq!(inner.closing_name, (closing, closing + "aé".len()));
    }

    #[test]
    fn zero_below_and_exact_work_have_no_partial_census_or_hidden_cancellation() {
        let source = b"<main><part></part></main>";
        let native = tree(source);
        let cancel = Cancellation::new();
        let (none, exhausted) = census_with_status(native.root_node(), source, 0, &cancel).unwrap();
        assert!(none.is_none() && exhausted);
        let exact = (1..4096)
            .find(|maximum| {
                census(native.root_node(), source, *maximum, &cancel)
                    .unwrap()
                    .is_some()
            })
            .unwrap();
        let (none, exhausted) =
            census_with_status(native.root_node(), source, exact - 1, &cancel).unwrap();
        assert!(none.is_none() && exhausted);
        assert_eq!(
            census(native.root_node(), source, exact, &cancel)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            census(native.root_node(), source, 0, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn long_closing_whitespace_is_charged_before_exact_pair_admission() {
        let short = b"<main></main>";
        let long = format!("<main></main{}>", " ".repeat(4096));
        let short_tree = tree(short);
        let long_tree = tree(long.as_bytes());
        let cancel = Cancellation::new();
        let short_exact = (1..16384)
            .find(|maximum| {
                census(short_tree.root_node(), short, *maximum, &cancel)
                    .unwrap()
                    .is_some()
            })
            .unwrap();
        let long_exact = (1..16384)
            .find(|maximum| {
                census(long_tree.root_node(), long.as_bytes(), *maximum, &cancel)
                    .unwrap()
                    .is_some()
            })
            .unwrap();
        assert!(long_exact > short_exact);
        let (none, exhausted) =
            census_with_status(long_tree.root_node(), long.as_bytes(), short_exact, &cancel)
                .unwrap();
        assert!(none.is_none() && exhausted);
        let (none, exhausted) = census_with_status(
            long_tree.root_node(),
            long.as_bytes(),
            long_exact - 1,
            &cancel,
        )
        .unwrap();
        assert!(none.is_none() && exhausted);
        assert_eq!(
            census(long_tree.root_node(), long.as_bytes(), long_exact, &cancel)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
    }
}
