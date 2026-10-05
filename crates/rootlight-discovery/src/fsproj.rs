//! Bounded explicit F# project declarations from admitted immutable snapshots.
//! This closed reader supplies positive language declarations, not evaluated
//! MSBuild membership, exhaustive inventories, or unique project ownership.
//! Discovery joins its results only to paths already admitted by repository policy.

use std::iter::Peekable;

use rootlight_cancel::Cancellation;
use rootlight_vfs::{SourceText, TextEncoding, VfsError};

use super::{DiscoveryError, MAX_DISCOVERY_DEPTH};

/// Maximum original bytes examined for one project document.
pub(super) const MAX_PROJECT_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Maximum explicit member declarations retained from one project document.
pub(super) const MAX_PROJECT_MEMBERS: usize = 8_192;
/// Maximum bytes in one decoded token or canonical member path.
pub(super) const MAX_PROJECT_STRING_BYTES: usize = 4_096;
/// Maximum aggregate normalized Compile declaration-path bytes in one document.
pub(super) const MAX_PROJECT_MEMBER_BYTES: usize = 1024 * 1024;

const MSBUILD_NAMESPACE: &str = "http://schemas.microsoft.com/developer/msbuild/2003";

/// Whole-document outcome without partial declarations from an unproven project.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CompileMembers {
    /// Literal `.fs` declarations, normalized to repository-relative paths.
    Valid(Vec<String>),
    /// Malformed XML or syntax outside the deliberately supported subset.
    Rejected,
    /// A document, token, depth, member-count, or retained-byte ceiling won.
    Bounded,
}

/// Reads literal direct `Project/ItemGroup/Compile Include` declarations.
///
/// The caller supplies the admitted project's canonical repository-relative
/// path. UTF-8 and BOM-backed UTF-16 are borrowed without a decoded source copy.
/// A result never proves evaluated membership, resolves imports, or opens files.
/// No members escape until one complete, supported XML document has ended.
/// Literal `.fsi` and `.fsx` entries are validated and charged, but only `.fs`
/// declarations are returned because signatures and scripts have distinct routes.
///
/// # Errors
/// Returns cancellation or a typed VFS allocation failure. Unsupported syntax
/// and resource ceilings instead return source-free non-witness outcomes.
pub(super) fn compile_members(
    bytes: &[u8],
    project_path: &str,
    cancellation: &Cancellation,
) -> Result<CompileMembers, DiscoveryError> {
    cancellation.check()?;
    if bytes.len() > MAX_PROJECT_DOCUMENT_BYTES {
        return Ok(CompileMembers::Bounded);
    }
    let Some(text) = SourceText::parse(bytes, cancellation)? else {
        return Ok(CompileMembers::Rejected);
    };
    let mut reader = Reader {
        chars: text.chars().peekable(),
        encoding: text.encoding(),
        cancellation,
        consumed: 0,
        stack: Vec::new(),
        members: Vec::new(),
        declared_members: 0,
        member_bytes: 0,
        seen_root: false,
        project_path,
    };
    match reader.document() {
        Ok(()) => {
            cancellation.check()?;
            reader.members.sort_unstable();
            reader.members.dedup();
            Ok(CompileMembers::Valid(reader.members))
        }
        Err(ReadFailure::Rejected) => Ok(CompileMembers::Rejected),
        Err(ReadFailure::Bounded) => Ok(CompileMembers::Bounded),
        Err(ReadFailure::Discovery(error)) => Err(error),
    }
}

enum ReadFailure {
    Rejected,
    Bounded,
    Discovery(DiscoveryError),
}

impl From<DiscoveryError> for ReadFailure {
    fn from(error: DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

type ReadResult<T> = Result<T, ReadFailure>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Project,
    ItemGroup,
    Compile,
    Inert,
}

struct Frame {
    name: String,
    role: Role,
}

struct Attribute {
    name: String,
    value: String,
}

struct Reader<'a, I: Iterator<Item = char>> {
    chars: Peekable<I>,
    encoding: TextEncoding,
    cancellation: &'a Cancellation,
    consumed: usize,
    stack: Vec<Frame>,
    members: Vec<String>,
    declared_members: usize,
    member_bytes: usize,
    seen_root: bool,
    project_path: &'a str,
}

impl<I: Iterator<Item = char>> Reader<'_, I> {
    fn document(&mut self) -> ReadResult<()> {
        if self.encoding == TextEncoding::Utf8 && self.peek() == Some('\u{feff}') {
            self.take()?;
        }
        let mut declaration_allowed = true;
        while let Some(character) = self.peek() {
            if character != '<' {
                self.text()?;
                declaration_allowed = false;
                continue;
            }
            self.take()?;
            match self.peek() {
                Some('?') => {
                    self.take()?;
                    self.processing_instruction(declaration_allowed)?;
                }
                Some('!') => {
                    self.take()?;
                    match self.peek() {
                        Some('-') => {
                            self.literal("--")?;
                            self.comment()?;
                        }
                        Some('[') if !self.stack.is_empty() => {
                            self.literal("[CDATA[")?;
                            self.cdata()?;
                        }
                        _ => return Err(ReadFailure::Rejected),
                    }
                }
                Some('/') => {
                    self.take()?;
                    self.end_element()?;
                }
                Some(_) => self.start_element()?,
                None => return Err(ReadFailure::Rejected),
            }
            declaration_allowed = false;
        }
        if !self.seen_root || !self.stack.is_empty() {
            return Err(ReadFailure::Rejected);
        }
        Ok(())
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn take(&mut self) -> ReadResult<Option<char>> {
        if self.consumed.is_multiple_of(1024) {
            self.cancellation.check().map_err(DiscoveryError::from)?;
        }
        let character = self.chars.next();
        if let Some(character) = character {
            if !xml_character(character) {
                return Err(ReadFailure::Rejected);
            }
            self.consumed = self.consumed.saturating_add(1);
        }
        Ok(character)
    }

    fn required(&mut self) -> ReadResult<char> {
        self.take()?.ok_or(ReadFailure::Rejected)
    }

    fn literal(&mut self, expected: &str) -> ReadResult<()> {
        for character in expected.chars() {
            if self.required()? != character {
                return Err(ReadFailure::Rejected);
            }
        }
        Ok(())
    }

    fn space(&mut self) -> ReadResult<bool> {
        let mut consumed = false;
        while self.peek().is_some_and(xml_space) {
            self.take()?;
            consumed = true;
        }
        Ok(consumed)
    }

    fn name(&mut self) -> ReadResult<String> {
        let first = self.required()?;
        if !first.is_ascii_alphabetic() && first != '_' {
            return Err(ReadFailure::Rejected);
        }
        let mut name = String::new();
        push_character(&mut name, first)?;
        while let Some(character) = self.peek() {
            if !character.is_ascii_alphanumeric() && !matches!(character, '_' | '.' | '-') {
                break;
            }
            self.take()?;
            push_character(&mut name, character)?;
        }
        Ok(name)
    }

    fn attributes(&mut self, declaration: bool) -> ReadResult<(Vec<Attribute>, bool)> {
        let mut attributes = Vec::new();
        let mut retained_bytes = 0_usize;
        loop {
            let separated = self.space()?;
            match self.peek() {
                Some('>') if !declaration => {
                    self.take()?;
                    return Ok((attributes, false));
                }
                Some('/') if !declaration => {
                    self.literal("/>")?;
                    return Ok((attributes, true));
                }
                Some('?') if declaration => {
                    self.literal("?>")?;
                    return Ok((attributes, false));
                }
                _ if !separated => return Err(ReadFailure::Rejected),
                _ => {}
            }
            if attributes.len() >= MAX_PROJECT_MEMBERS {
                return Err(ReadFailure::Bounded);
            }
            let name = self.name()?;
            self.space()?;
            self.literal("=")?;
            self.space()?;
            let quote = self.required()?;
            if !matches!(quote, '\'' | '"') {
                return Err(ReadFailure::Rejected);
            }
            let value = self.attribute_value(quote, declaration)?;
            retained_bytes = retained_bytes
                .checked_add(name.len())
                .and_then(|bytes| bytes.checked_add(value.len()))
                .filter(|bytes| *bytes <= MAX_PROJECT_DOCUMENT_BYTES)
                .ok_or(ReadFailure::Bounded)?;
            reserve_one(&mut attributes)?;
            attributes.push(Attribute { name, value });
        }
    }

    fn attribute_value(&mut self, quote: char, declaration: bool) -> ReadResult<String> {
        let mut value = String::new();
        loop {
            let character = self.required()?;
            if character == quote {
                return Ok(value);
            }
            let character = match character {
                '<' => return Err(ReadFailure::Rejected),
                '&' if declaration => return Err(ReadFailure::Rejected),
                '&' => self.entity()?,
                '\r' => {
                    if self.peek() == Some('\n') {
                        self.take()?;
                    }
                    ' '
                }
                '\n' | '\t' => ' ',
                character => character,
            };
            push_character(&mut value, character)?;
        }
    }

    fn start_element(&mut self) -> ReadResult<()> {
        if self.stack.len() >= MAX_DISCOVERY_DEPTH {
            return Err(ReadFailure::Bounded);
        }
        let name = self.name()?;
        let (mut attributes, empty) = self.attributes(false)?;
        attributes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        if attributes
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.name == right.name))
        {
            return Err(ReadFailure::Rejected);
        }
        let role = if self.stack.is_empty() {
            if self.seen_root || name != "Project" {
                return Err(ReadFailure::Rejected);
            }
            if attribute(&attributes, "xmlns")
                .is_some_and(|namespace| !namespace.is_empty() && namespace != MSBUILD_NAMESPACE)
            {
                return Err(ReadFailure::Rejected);
            }
            self.seen_root = true;
            Role::Project
        } else {
            // Prefixes and subtree namespace declarations are deliberately not
            // interpreted; an inert subtree cannot shadow a witnessed element.
            if attribute(&attributes, "xmlns").is_some()
                || matches!(name.as_str(), "Project" | "Target" | "Choose")
            {
                return Err(ReadFailure::Rejected);
            }
            let parent = self.stack.last().ok_or(ReadFailure::Rejected)?.role;
            match name.as_str() {
                "ItemGroup" if parent == Role::Project => Role::ItemGroup,
                "Compile" if parent == Role::ItemGroup => Role::Compile,
                "Compile" => return Err(ReadFailure::Rejected),
                _ => Role::Inert,
            }
        };
        if role != Role::Inert && attribute(&attributes, "Condition").is_some() {
            return Err(ReadFailure::Rejected);
        }
        if role == Role::Compile {
            if ["Remove", "Update", "Exclude"]
                .iter()
                .any(|name| attribute(&attributes, name).is_some())
            {
                return Err(ReadFailure::Rejected);
            }
            let include = attribute(&attributes, "Include").ok_or(ReadFailure::Rejected)?;
            if self.declared_members >= MAX_PROJECT_MEMBERS {
                return Err(ReadFailure::Bounded);
            }
            let member = normalize_member(self.project_path, include)?;
            self.member_bytes = self
                .member_bytes
                .checked_add(member.len())
                .filter(|bytes| *bytes <= MAX_PROJECT_MEMBER_BYTES)
                .ok_or(ReadFailure::Bounded)?;
            self.declared_members += 1;
            if member
                .rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("fs"))
            {
                reserve_one(&mut self.members)?;
                self.members.push(member);
            }
        }
        if !empty {
            reserve_one(&mut self.stack)?;
            self.stack.push(Frame { name, role });
        }
        Ok(())
    }

    fn end_element(&mut self) -> ReadResult<()> {
        let name = self.name()?;
        self.space()?;
        self.literal(">")?;
        if self.stack.pop().is_none_or(|frame| frame.name != name) {
            return Err(ReadFailure::Rejected);
        }
        Ok(())
    }

    fn text(&mut self) -> ReadResult<()> {
        let mut closing_brackets = 0_u8;
        while self.peek().is_some_and(|character| character != '<') {
            let character = self.required()?;
            if self.stack.is_empty() && !xml_space(character) {
                return Err(ReadFailure::Rejected);
            }
            match character {
                '&' => {
                    self.entity()?;
                    closing_brackets = 0;
                }
                ']' => closing_brackets = closing_brackets.saturating_add(1).min(2),
                '>' if closing_brackets == 2 => return Err(ReadFailure::Rejected),
                _ => closing_brackets = 0,
            }
        }
        Ok(())
    }

    fn comment(&mut self) -> ReadResult<()> {
        loop {
            if self.required()? == '-' && self.peek() == Some('-') {
                self.literal("->")?;
                return Ok(());
            }
        }
    }

    fn cdata(&mut self) -> ReadResult<()> {
        let mut closing_brackets = 0_u8;
        loop {
            match self.required()? {
                ']' => closing_brackets = closing_brackets.saturating_add(1).min(2),
                '>' if closing_brackets == 2 => return Ok(()),
                _ => closing_brackets = 0,
            }
        }
    }

    fn processing_instruction(&mut self, declaration_allowed: bool) -> ReadResult<()> {
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            if target != "xml" || !declaration_allowed {
                return Err(ReadFailure::Rejected);
            }
            let (attributes, _) = self.attributes(true)?;
            return validate_declaration(&attributes, self.encoding);
        }
        if self.peek() == Some('?') {
            return self.literal("?>");
        }
        if !self.space()? {
            return Err(ReadFailure::Rejected);
        }
        loop {
            if self.required()? == '?' && self.peek() == Some('>') {
                self.take()?;
                return Ok(());
            }
        }
    }

    fn entity(&mut self) -> ReadResult<char> {
        let mut entity = String::new();
        loop {
            let character = self.required()?;
            if character == ';' {
                break;
            }
            if !character.is_ascii_alphanumeric() && character != '#' {
                return Err(ReadFailure::Rejected);
            }
            if entity.len() >= 32 {
                return Err(ReadFailure::Rejected);
            }
            push_character(&mut entity, character)?;
        }
        match entity.as_str() {
            "amp" => Ok('&'),
            "lt" => Ok('<'),
            "gt" => Ok('>'),
            "quot" => Ok('"'),
            "apos" => Ok('\''),
            numeric => {
                let (digits, radix) = if let Some(digits) = numeric.strip_prefix("#x") {
                    (digits, 16)
                } else if let Some(digits) = numeric.strip_prefix('#') {
                    (digits, 10)
                } else {
                    return Err(ReadFailure::Rejected);
                };
                if digits.is_empty() {
                    return Err(ReadFailure::Rejected);
                }
                let mut scalar = 0_u32;
                for digit in digits.chars() {
                    scalar = scalar
                        .checked_mul(radix)
                        .and_then(|value| value.checked_add(digit.to_digit(radix)?))
                        .ok_or(ReadFailure::Rejected)?;
                }
                char::from_u32(scalar)
                    .filter(|character| xml_character(*character))
                    .ok_or(ReadFailure::Rejected)
            }
        }
    }
}

fn xml_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

fn xml_character(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .map(|attribute| attribute.value.as_str())
}

fn validate_declaration(attributes: &[Attribute], encoding: TextEncoding) -> ReadResult<()> {
    let mut attributes = attributes.iter();
    if attributes
        .next()
        .is_none_or(|attribute| attribute.name != "version" || attribute.value != "1.0")
    {
        return Err(ReadFailure::Rejected);
    }
    let mut next = attributes.next();
    if let Some(attribute) = next.filter(|attribute| attribute.name == "encoding") {
        let valid = match encoding {
            TextEncoding::Utf8 => attribute.value.eq_ignore_ascii_case("utf-8"),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                attribute.value.eq_ignore_ascii_case("utf-16")
            }
        };
        if !valid {
            return Err(ReadFailure::Rejected);
        }
        next = attributes.next();
    }
    if let Some(attribute) = next {
        if attribute.name != "standalone" || !matches!(attribute.value.as_str(), "yes" | "no") {
            return Err(ReadFailure::Rejected);
        }
        next = attributes.next();
    }
    if next.is_some() {
        return Err(ReadFailure::Rejected);
    }
    Ok(())
}

fn normalize_member(project_path: &str, include: &str) -> ReadResult<String> {
    if project_path.len() > MAX_PROJECT_STRING_BYTES || include.len() > MAX_PROJECT_STRING_BYTES {
        return Err(ReadFailure::Bounded);
    }
    if project_path.is_empty()
        || project_path.contains('\\')
        || project_path.split('/').any(|component| {
            component.is_empty() || matches!(component, "." | "..") || !literal_component(component)
        })
        || include.is_empty()
        || include.starts_with(['/', '\\'])
        || include.trim() != include
        || include
            .rsplit(['/', '\\'])
            .next()
            .is_none_or(|component| matches!(component, "" | "." | ".."))
    {
        return Err(ReadFailure::Rejected);
    }
    let mut path = String::new();
    if let Some((directory, _)) = project_path.rsplit_once('/') {
        push_str(&mut path, directory)?;
    }
    for component in include.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                if path.is_empty() {
                    return Err(ReadFailure::Rejected);
                }
                path.truncate(path.rfind('/').unwrap_or(0));
            }
            component => {
                if !literal_component(component) {
                    return Err(ReadFailure::Rejected);
                }
                if !path.is_empty() {
                    push_character(&mut path, '/')?;
                }
                push_str(&mut path, component)?;
            }
        }
    }
    if !path.rsplit_once('.').is_some_and(|(_, suffix)| {
        ["fs", "fsi", "fsx"]
            .iter()
            .any(|allowed| suffix.eq_ignore_ascii_case(allowed))
    }) {
        return Err(ReadFailure::Rejected);
    }
    Ok(path)
}

fn literal_component(component: &str) -> bool {
    !component.ends_with([' ', '.'])
        && !component.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    ':' | '*' | '?' | '$' | '@' | '%' | ';' | '[' | ']' | '<' | '>' | '"' | '|'
                )
        })
}

fn push_character(value: &mut String, character: char) -> ReadResult<()> {
    let mut bytes = [0; 4];
    push_str(value, character.encode_utf8(&mut bytes))
}

fn push_str(value: &mut String, addition: &str) -> ReadResult<()> {
    if value
        .len()
        .checked_add(addition.len())
        .is_none_or(|length| length > MAX_PROJECT_STRING_BYTES)
    {
        return Err(ReadFailure::Bounded);
    }
    value
        .try_reserve(addition.len())
        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
    value.push_str(addition);
    Ok(())
}

fn reserve_one<T>(values: &mut Vec<T>) -> ReadResult<()> {
    values
        .try_reserve(1)
        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> CompileMembers {
        compile_members(
            source.as_bytes(),
            "src/Example.fsproj",
            &Cancellation::new(),
        )
        .unwrap()
    }

    fn project(include: &str) -> String {
        format!("<Project><ItemGroup><Compile Include=\"{include}\" /></ItemGroup></Project>")
    }

    #[test]
    fn literal_declarations_are_sorted_normalized_and_deduplicated() {
        let source = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
            <!-- <Compile Include='False.fs'/> -->\
            <Project Sdk='Microsoft.NET.Sdk'>\
              <PropertyGroup><Value><![CDATA[<Compile Include='False.fs'/>]]></Value></PropertyGroup>\
              <Import Project='$(Other)' />\
              <ItemGroup><Compile Include='.\\One.fs' /><Compile Include='../shared/Two.fs'>\
                <Link>Two.fs</Link></Compile><Compile Include='One.fs'/></ItemGroup>\
            </Project><?note complete?>";
        assert_eq!(
            read(source),
            CompileMembers::Valid(vec!["shared/Two.fs".to_owned(), "src/One.fs".to_owned()])
        );
        assert_eq!(read("<Project/>"), CompileMembers::Valid(Vec::new()));
    }

    #[test]
    fn signature_and_script_declarations_validate_without_shared_suffix_witnesses() {
        assert_eq!(
            read(
                "<Project><ItemGroup><Compile Include='One.fsi'/><Compile Include='One.fs'/><Compile Include='Script.fsx'/></ItemGroup></Project>"
            ),
            CompileMembers::Valid(vec!["src/One.fs".to_owned()])
        );
        let mut source = String::from("<Project><ItemGroup>");
        source.push_str(&"<Compile Include='One.fsi'/>".repeat(MAX_PROJECT_MEMBERS));
        source.push_str("</ItemGroup></Project>");
        assert_eq!(read(&source), CompileMembers::Valid(Vec::new()));
        source = source.replace("</ItemGroup>", "<Compile Include='One.fs'/></ItemGroup>");
        assert_eq!(read(&source), CompileMembers::Bounded);
    }

    #[test]
    fn full_document_validation_never_releases_a_valid_prefix() {
        let valid = project("One.fs");
        for source in [
            format!("{valid}<Project/>"),
            format!("{valid}tail"),
            format!("{valid}<!-- unfinished"),
            format!("{valid}<?xml version='1.0'?>"),
            "<Project><ItemGroup><Compile Include='One.fs'/></Project>".to_owned(),
            "<Project><ItemGroup><Compile Include='One.fs'/></ItemGroup>".to_owned(),
            "<Project><ItemGroup><Compile Include='One.fs'/></ItemGroup></project>".to_owned(),
        ] {
            assert_eq!(read(&source), CompileMembers::Rejected, "{source}");
        }
    }

    #[test]
    fn namespaces_cannot_be_shadowed_or_prefixed_into_witnesses() {
        for namespace in ["", MSBUILD_NAMESPACE] {
            assert_eq!(
                read(&format!(
                    "<Project xmlns='{namespace}'><ItemGroup><Compile Include='One.fs'/></ItemGroup></Project>"
                )),
                CompileMembers::Valid(vec!["src/One.fs".to_owned()])
            );
        }
        for source in [
            "<Project xmlns='urn:other'><ItemGroup><Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup xmlns='urn:other'><Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup xmlns=''><Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project xmlns:p='urn:other'><ItemGroup><Compile Include='One.fs'/></ItemGroup></Project>",
            "<p:Project><ItemGroup><Compile Include='One.fs'/></ItemGroup></p:Project>",
            "<Project><ItemGroup><p:Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile p:Include='One.fs'/></ItemGroup></Project>",
        ] {
            assert_eq!(read(source), CompileMembers::Rejected, "{source}");
        }
    }

    #[test]
    fn entities_are_decoded_before_literal_path_validation() {
        assert_eq!(
            read(&project("&#x2e;&#46;/shared/A&amp;B&apos;λ.fs")),
            CompileMembers::Valid(vec!["shared/A&B'λ.fs".to_owned()])
        );
        for include in [
            "&custom;.fs",
            "&#0;.fs",
            "&#xD800;.fs",
            "&#x110000;.fs",
            "&#x;.fs",
            "&#;.fs",
            "&#36;(Sources).fs",
            "&#37;2e.fs",
            "&#x2f;Absolute.fs",
            "&lt;One.fs",
            "&quot;One.fs",
            "Line&#10;Break.fs",
        ] {
            assert_eq!(
                read(&project(include)),
                CompileMembers::Rejected,
                "{include}"
            );
        }
    }

    #[test]
    fn declaration_and_markup_subset_is_closed() {
        for source in [
            "<!DOCTYPE Project><Project/>",
            "<!DOCTYPE Project [<!ENTITY x 'One.fs'>]><Project/>",
            "<?xml version='1.1'?><Project/>",
            " <?xml version='1.0'?><Project/>",
            "<?XML version='1.0'?><Project/>",
            "<?xml version='1.0' version='1.0'?><Project/>",
            "<?xml encoding='UTF-8' version='1.0'?><Project/>",
            "<?xml version='1.0' encoding='UTF-16'?><Project/>",
            "<?xml version='1&#46;0'?><Project/>",
            "<?xml version='1.0' encoding='UTF&#45;8'?><Project/>",
            "<Project A='1' A='2'/>",
            "<Project A=unquoted/>",
            "<Project A='1'B='2'/>",
            "<Project><!-- bad -- comment --></Project>",
            "<Project><!-- bad ---></Project>",
            "<Project>text]]>tail</Project>",
            "<Project>&external;</Project>",
            "<Project>\u{1}</Project>",
            "<Project>\u{fffe}</Project>",
            "<![CDATA[outside]]><Project/>",
            "<Project><Élément/></Project>",
        ] {
            assert_eq!(read(source), CompileMembers::Rejected, "{source:?}");
        }
        assert_eq!(
            read("<!--safe--><?note?><Project>safe &lt; text</Project><!--done-->"),
            CompileMembers::Valid(Vec::new())
        );
    }

    #[test]
    fn conditional_or_transforming_compile_syntax_has_no_witness() {
        for source in [
            "<Project Condition='true'><ItemGroup><Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup Condition='true'><Compile Include='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Include='One.fs' Condition='true'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Include='One.fs' Exclude='Other.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Include='One.fs'/><Compile Remove='One.fs'/></ItemGroup></Project>",
            "<Project><ItemGroup><Compile Include='One.fs'/><Compile Update='One.fs'/></ItemGroup></Project>",
            "<Project><Target Name='Build'><ItemGroup><Compile Include='One.fs'/></ItemGroup></Target></Project>",
            "<Project><Choose><When Condition='true'/></Choose></Project>",
            "<Project><Wrapper><ItemGroup><Compile Include='One.fs'/></ItemGroup></Wrapper></Project>",
            "<Project><ItemGroup><Compile/></ItemGroup></Project>",
        ] {
            assert_eq!(read(source), CompileMembers::Rejected, "{source}");
        }
    }

    #[test]
    fn relative_resolution_rejects_escape_and_dynamic_membership() {
        for include in [
            "../../Escape.fs",
            "/Absolute.fs",
            "\\\\host\\Share.fs",
            "C:\\Drive.fs",
            "file:One.fs",
            "*.fs",
            "A?.fs",
            "[AB].fs",
            "$(Files).fs",
            "@(Files).fs",
            "One.fs;Two.fs",
            "%2e.fs",
            "One.cs",
            "One.txt",
            "One.fs/..",
            "One.fs/",
            "One.fs/.",
            " One.fs",
            "One.fs ",
            "dir./One.fs",
            "dir /One.fs",
        ] {
            assert_eq!(
                read(&project(include)),
                CompileMembers::Rejected,
                "{include}"
            );
        }
        assert_eq!(
            read(&project("./nested/../One.fs")),
            CompileMembers::Valid(vec!["src/One.fs".to_owned()])
        );
        assert_eq!(
            read(&project("../One.FS")),
            CompileMembers::Valid(vec!["One.FS".to_owned()])
        );
    }

    #[test]
    fn original_utf8_and_bom_utf16_are_borrowed_and_fully_validated() {
        let source = project("λ𝄞.fs");
        let expected = CompileMembers::Valid(vec!["src/λ𝄞.fs".to_owned()]);
        let mut utf8 = vec![0xef, 0xbb, 0xbf];
        utf8.extend_from_slice(source.as_bytes());
        assert_eq!(
            compile_members(&utf8, "src/Example.fsproj", &Cancellation::new()).unwrap(),
            expected
        );
        let mut duplicate_bom = vec![0xef, 0xbb, 0xbf];
        duplicate_bom.extend_from_slice(&utf8);
        assert_eq!(
            compile_members(&duplicate_bom, "src/Example.fsproj", &Cancellation::new()).unwrap(),
            CompileMembers::Rejected
        );
        for big_endian in [false, true] {
            let mut bytes = Vec::new();
            for unit in std::iter::once(0xfeff).chain(source.encode_utf16()) {
                bytes.extend(if big_endian {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
            assert_eq!(
                compile_members(&bytes, "src/Example.fsproj", &Cancellation::new()).unwrap(),
                expected
            );
            let mut duplicate_bom = if big_endian {
                vec![0xfe, 0xff]
            } else {
                vec![0xff, 0xfe]
            };
            duplicate_bom.extend_from_slice(&bytes);
            assert_eq!(
                compile_members(&duplicate_bom, "src/Example.fsproj", &Cancellation::new())
                    .unwrap(),
                CompileMembers::Rejected
            );
            for suffix in [vec![0], vec![0, 0xd8], vec![0xd8, 0], vec![0, 0]] {
                let mut invalid = bytes.clone();
                invalid.extend(suffix);
                assert_eq!(
                    compile_members(&invalid, "src/Example.fsproj", &Cancellation::new()).unwrap(),
                    CompileMembers::Rejected
                );
            }
            let declaration = format!("<?xml version='1.0' encoding='UTF-16'?>{source}");
            let declared: Vec<_> = std::iter::once(0xfeff)
                .chain(declaration.encode_utf16())
                .flat_map(|unit| {
                    if big_endian {
                        unit.to_be_bytes()
                    } else {
                        unit.to_le_bytes()
                    }
                })
                .collect();
            assert_eq!(
                compile_members(&declared, "src/Example.fsproj", &Cancellation::new()).unwrap(),
                expected
            );
        }
        assert_eq!(
            compile_members(b"<Project/>\xff", "Example.fsproj", &Cancellation::new()).unwrap(),
            CompileMembers::Rejected
        );
    }

    #[test]
    fn document_member_string_and_depth_limits_fail_without_partial_members() {
        let mut bytes = b"<Project/>".to_vec();
        bytes.resize(MAX_PROJECT_DOCUMENT_BYTES, b' ');
        assert_eq!(
            compile_members(&bytes, "Example.fsproj", &Cancellation::new()).unwrap(),
            CompileMembers::Valid(Vec::new())
        );
        bytes.resize(MAX_PROJECT_DOCUMENT_BYTES + 1, b' ');
        assert_eq!(
            compile_members(&bytes, "Example.fsproj", &Cancellation::new()).unwrap(),
            CompileMembers::Bounded
        );
        let maximum_path = format!("{}.fs", "a".repeat(MAX_PROJECT_STRING_BYTES - 3));
        assert_eq!(
            compile_members(
                project(&maximum_path).as_bytes(),
                "Example.fsproj",
                &Cancellation::new()
            )
            .unwrap(),
            CompileMembers::Valid(vec![maximum_path])
        );
        assert_eq!(
            read(&project(&format!(
                "{}.fs",
                "a".repeat(MAX_PROJECT_STRING_BYTES)
            ))),
            CompileMembers::Bounded
        );
        let mut members = String::from("<Project><ItemGroup>");
        members.push_str(&"<Compile Include='One.fs'/>".repeat(MAX_PROJECT_MEMBERS));
        members.push_str("</ItemGroup></Project>");
        assert_eq!(
            read(&members),
            CompileMembers::Valid(vec!["src/One.fs".to_owned()])
        );
        members = members.replace("</ItemGroup>", "<Compile Include='Two.fs'/></ItemGroup>");
        assert_eq!(read(&members), CompileMembers::Bounded);
        let nested = format!(
            "<Project>{}{}</Project>",
            "<A>".repeat(MAX_DISCOVERY_DEPTH - 1),
            "</A>".repeat(MAX_DISCOVERY_DEPTH - 1)
        );
        assert_eq!(read(&nested), CompileMembers::Valid(Vec::new()));
        let excessive = format!(
            "<Project>{}{}</Project>",
            "<A>".repeat(MAX_DISCOVERY_DEPTH),
            "</A>".repeat(MAX_DISCOVERY_DEPTH)
        );
        assert_eq!(read(&excessive), CompileMembers::Bounded);
    }

    #[test]
    fn retained_member_bytes_and_canonical_path_growth_are_bounded() {
        let directory = "d".repeat(4000);
        let project_path = format!("{directory}/Example.fsproj");
        let mut source = String::from("<Project><ItemGroup>");
        source.push_str(&"<Compile Include='One.fs'/>".repeat(262));
        source.push_str("</ItemGroup></Project>");
        assert_eq!(
            compile_members(source.as_bytes(), &project_path, &Cancellation::new()).unwrap(),
            CompileMembers::Bounded
        );
        assert_eq!(
            compile_members(
                project(&format!("{}.fs", "x".repeat(100))).as_bytes(),
                &project_path,
                &Cancellation::new()
            )
            .unwrap(),
            CompileMembers::Bounded
        );
    }

    #[test]
    fn cancellation_is_typed_even_before_a_bounded_input() {
        let cancellation = Cancellation::with_deadline(std::time::Instant::now());
        assert!(matches!(
            compile_members(b"<Project/>", "Example.fsproj", &cancellation),
            Err(DiscoveryError::Cancelled(_))
        ));
        let bytes = vec![b' '; MAX_PROJECT_DOCUMENT_BYTES + 1];
        assert!(matches!(
            compile_members(&bytes, "Example.fsproj", &cancellation),
            Err(DiscoveryError::Cancelled(_))
        ));
    }
}
