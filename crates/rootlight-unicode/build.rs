//! Generates first-party case-fold lookup data from the pinned Unicode 9 file.
//! Cargo reads only the reviewed local input; this build script has no network path.

use std::{collections::BTreeMap, error::Error, fmt::Write as _, fs, path::PathBuf};

const DATA: &str = "data/CaseFolding-9.0.0.txt";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={DATA}");
    let raw = fs::read_to_string(DATA)?;
    if !raw.starts_with("# CaseFolding-9.0.0.txt\n") {
        return Err("unexpected Unicode case-fold data version".into());
    }
    let mut common = BTreeMap::new();
    let mut full = BTreeMap::new();
    let mut simple_count = 0;
    let mut turkic_count = 0;
    for (line_index, line) in raw.lines().enumerate() {
        let content = line
            .split_once('#')
            .map_or(line, |(prefix, _)| prefix)
            .trim();
        if content.is_empty() {
            continue;
        }
        let fields = content.split(';').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 4 || !fields[3].is_empty() {
            return Err(format!("invalid case-fold record at line {}", line_index + 1).into());
        }
        let source = scalar(fields[0], line_index)?;
        let mapping = fields[2]
            .split_whitespace()
            .map(|value| scalar(value, line_index))
            .collect::<Result<Vec<_>, _>>()?;
        if mapping.is_empty() || mapping.len() > 3 {
            return Err(format!("invalid case-fold width at line {}", line_index + 1).into());
        }
        match fields[1] {
            "C" if mapping.len() == 1 => {
                if common.insert(source, mapping).is_some() {
                    return Err("duplicate common case-fold record".into());
                }
            }
            "F" => {
                if full.insert(source, mapping).is_some() {
                    return Err("duplicate full case-fold record".into());
                }
            }
            "S" if mapping.len() == 1 => simple_count += 1,
            "T" => turkic_count += 1,
            _ => return Err(format!("invalid case-fold status at line {}", line_index + 1).into()),
        }
    }
    if (common.len(), full.len(), simple_count, turkic_count) != (1297, 104, 28, 2) {
        return Err("Unicode case-fold record counts differ from the pinned release".into());
    }
    common.extend(full);
    let mut generated = String::from(
        "// Generated from the pinned Unicode 9 CaseFolding data.\n\
         pub(crate) const CASE_FOLD_TABLE: &[(char, [char; 3], u8)] = &[\n",
    );
    for (source, mapping) in common {
        let mut expanded = ['\0'; 3];
        for (slot, character) in expanded.iter_mut().zip(&mapping) {
            *slot = *character;
        }
        writeln!(
            generated,
            "    ('\\u{{{:X}}}', ['\\u{{{:X}}}', '\\u{{{:X}}}', '\\u{{{:X}}}'], {}),",
            u32::from(source),
            u32::from(expanded[0]),
            u32::from(expanded[1]),
            u32::from(expanded[2]),
            mapping.len()
        )?;
    }
    generated.push_str("];\n");
    let path = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing Cargo output directory")?)
        .join("case_folding.rs");
    fs::write(path, generated)?;
    Ok(())
}

fn scalar(value: &str, line_index: usize) -> Result<char, Box<dyn Error>> {
    let code = u32::from_str_radix(value, 16)?;
    char::from_u32(code)
        .ok_or_else(|| format!("invalid Unicode scalar at line {}", line_index + 1).into())
}
