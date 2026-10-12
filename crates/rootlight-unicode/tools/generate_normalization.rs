//! Regenerates the checked-in canonical normalization tables from Unicode 17.
//!
//! Run this standalone std-only tool from the `crates/rootlight-unicode`
//! directory, then format `src/normalization_data.rs` with rustfmt. Cargo
//! never invokes either step during compilation or packaging.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Write as _,
    fs,
};

const UNICODE_DATA: &str = "data/UnicodeData-17.0.0.txt";
const COMPOSITION_EXCLUSIONS: &str = "data/CompositionExclusions-17.0.0.txt";
const GENERATED: &str = "src/normalization_data.rs";

fn main() -> Result<(), Box<dyn Error>> {
    let mut classes = BTreeMap::new();
    let mut decompositions = BTreeMap::new();
    for (line_index, line) in fs::read_to_string(UNICODE_DATA)?.lines().enumerate() {
        let fields = line.split(';').collect::<Vec<_>>();
        if fields.len() != 15 {
            return Err(format!("invalid UnicodeData record at line {}", line_index + 1).into());
        }
        let code = u32::from_str_radix(fields[0], 16)?;
        let Some(source) = char::from_u32(code) else {
            if fields[3] != "0" || !fields[5].is_empty() {
                return Err("surrogate carries normalization data".into());
            }
            continue;
        };
        let class = fields[3].parse::<u8>()?;
        if class != 0 && classes.insert(source, class).is_some() {
            return Err("duplicate combining class".into());
        }
        let decomposition = fields[5];
        if decomposition.is_empty() || decomposition.starts_with('<') {
            continue;
        }
        let mapping = decomposition
            .split_whitespace()
            .map(scalar)
            .collect::<Result<Vec<_>, _>>()?;
        if mapping.is_empty() || mapping.len() > 2 {
            return Err("unexpected canonical decomposition width".into());
        }
        if decompositions.insert(source, mapping).is_some() {
            return Err("duplicate canonical decomposition".into());
        }
    }

    let mut exclusions = BTreeSet::new();
    let raw_exclusions = fs::read_to_string(COMPOSITION_EXCLUSIONS)?;
    if !raw_exclusions.starts_with("# CompositionExclusions-17.0.0.txt\n") {
        return Err("unexpected composition-exclusion version".into());
    }
    for line in raw_exclusions.lines() {
        let value = line
            .split_once('#')
            .map_or(line, |(prefix, _)| prefix)
            .trim();
        if value.is_empty() {
            continue;
        }
        if let Some((first, last)) = value.split_once("..") {
            for code in u32::from_str_radix(first, 16)?..=u32::from_str_radix(last, 16)? {
                exclusions.insert(char::from_u32(code).ok_or("invalid excluded scalar")?);
            }
        } else {
            exclusions.insert(scalar(value)?);
        }
    }

    let mut compositions = BTreeMap::new();
    for (&source, mapping) in &decompositions {
        if mapping.len() != 2 || exclusions.contains(&source) || classes.contains_key(&mapping[0]) {
            continue;
        }
        let pair = (mapping[0], mapping[1]);
        if compositions.insert(pair, source).is_some() {
            return Err("duplicate canonical composition pair".into());
        }
    }
    let composition_targets = compositions.values().copied().collect::<BTreeSet<_>>();
    let non_nfc_scalars = decompositions
        .keys()
        .filter(|source| !composition_targets.contains(source))
        .copied()
        .collect::<Vec<_>>();
    let starter_suffixes = compositions
        .keys()
        .filter(|(_, second)| !classes.contains_key(second))
        .map(|(_, second)| *second)
        .collect::<BTreeSet<_>>();
    let mut starter_to_nonstarter = Vec::new();
    for (&source, mapping) in &decompositions {
        if classes.contains_key(&source) {
            continue;
        }
        let mut first = mapping[0];
        for _ in 0..32 {
            let Some(parts) = decompositions.get(&first) else {
                break;
            };
            first = parts[0];
        }
        if decompositions.contains_key(&first) {
            return Err("canonical decomposition exceeds the generator depth bound".into());
        }
        if classes.contains_key(&first) {
            starter_to_nonstarter.push(source);
        }
    }
    let mut class_values = vec![0_u16; 0x110000];
    for (&source, &class) in &classes {
        class_values[usize::try_from(u32::from(source))?] = u16::from(class);
    }
    let mut decomposition_values = vec![0_u16; 0x110000];
    for (index, source) in decompositions.keys().enumerate() {
        decomposition_values[usize::try_from(u32::from(*source))?] = u16::try_from(index + 1)?;
    }
    let (class_page_map, class_pages) = compress_pages(&class_values)?;
    let (decomposition_page_map, decomposition_pages) = compress_pages(&decomposition_values)?;

    let mut output = String::from(
        "// Generated from the pinned Unicode 17 canonical normalization data.\n\
         pub(crate) const DECOMPOSITION: &[(char, &[char])] = &[\n",
    );
    for (source, mapping) in &decompositions {
        let parts = mapping.iter().copied().map(literal).collect::<Vec<_>>();
        writeln!(
            output,
            "    ({}, &[{}]),",
            literal(*source),
            parts.join(", ")
        )?;
    }
    output.push_str("];\n\npub(crate) const COMPOSITION: &[((char, char), char)] = &[\n");
    for ((first, second), source) in &compositions {
        writeln!(
            output,
            "    (({}, {}), {}),",
            literal(*first),
            literal(*second),
            literal(*source)
        )?;
    }
    output.push_str("];\n\npub(crate) const NON_NFC_SCALARS: &[char] = &[\n");
    for source in &non_nfc_scalars {
        writeln!(output, "    {},", literal(*source))?;
    }
    output.push_str("];\n\npub(crate) const COMPOSABLE_STARTER_SUFFIXES: &[char] = &[\n");
    for source in &starter_suffixes {
        writeln!(output, "    {},", literal(*source))?;
    }
    output.push_str("];\n\npub(crate) const STARTER_TO_NONSTARTER: &[char] = &[\n");
    for source in &starter_to_nonstarter {
        writeln!(output, "    {},", literal(*source))?;
    }
    output.push_str("];\n\n");
    write_values(&mut output, "COMBINING_PAGE_MAP", &class_page_map)?;
    write_pages(&mut output, "COMBINING_PAGES", &class_pages)?;
    write_values(
        &mut output,
        "DECOMPOSITION_PAGE_MAP",
        &decomposition_page_map,
    )?;
    write_pages(&mut output, "DECOMPOSITION_PAGES", &decomposition_pages)?;
    fs::write(GENERATED, output)?;
    println!(
        "canonical decompositions: {}, combining classes: {}, compositions: {}, non-NFC: {}, starter suffixes: {}, starter-to-nonstarter: {}, class pages: {}, decomposition pages: {}",
        decompositions.len(),
        classes.len(),
        compositions.len(),
        non_nfc_scalars.len(),
        starter_suffixes.len(),
        starter_to_nonstarter.len(),
        class_pages.len(),
        decomposition_pages.len()
    );
    Ok(())
}

fn scalar(value: &str) -> Result<char, Box<dyn Error>> {
    let code = u32::from_str_radix(value, 16)?;
    char::from_u32(code).ok_or_else(|| "invalid Unicode scalar".into())
}

fn literal(value: char) -> String {
    format!("'\\u{{{:X}}}'", u32::from(value))
}

fn compress_pages(values: &[u16]) -> Result<(Vec<u16>, Vec<[u16; 256]>), Box<dyn Error>> {
    if values.len() != 0x110000 {
        return Err("lookup table does not cover every Unicode code point".into());
    }
    let mut page_ids = BTreeMap::new();
    let mut pages = vec![[0_u16; 256]];
    page_ids.insert([0_u16; 256], 0_u16);
    let mut map = Vec::with_capacity(values.len() / 256);
    for &page in values.as_chunks::<256>().0 {
        let id = if let Some(&id) = page_ids.get(&page) {
            id
        } else {
            let id = u16::try_from(pages.len())?;
            page_ids.insert(page, id);
            pages.push(page);
            id
        };
        map.push(id);
    }
    Ok((map, pages))
}

fn write_values(output: &mut String, name: &str, values: &[u16]) -> Result<(), Box<dyn Error>> {
    writeln!(output, "pub(crate) const {name}: &[u16] = &[")?;
    for chunk in values.chunks(16) {
        output.push_str("    ");
        for &value in chunk {
            write!(output, "{value}, ")?;
        }
        output.push('\n');
    }
    output.push_str("];\n\n");
    Ok(())
}

fn write_pages(
    output: &mut String,
    name: &str,
    pages: &[[u16; 256]],
) -> Result<(), Box<dyn Error>> {
    writeln!(output, "pub(crate) const {name}: &[[u16; 256]] = &[")?;
    for page in pages {
        output.push_str("    [\n");
        for chunk in page.chunks(16) {
            output.push_str("        ");
            for &value in chunk {
                write!(output, "{value}, ")?;
            }
            output.push('\n');
        }
        output.push_str("    ],\n");
    }
    output.push_str("];\n\n");
    Ok(())
}
