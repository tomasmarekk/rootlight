//! Canonical Unicode normalization for repository and query identity.

use crate::{UnicodeCaseFold as _, normalization_data};

/// Unicode data version used for canonical decomposition and composition.
pub const NORMALIZATION_UNICODE_VERSION: (u8, u8, u8) = (17, 0, 0);

const HANGUL_S_BASE: u32 = 0xAC00;
const HANGUL_L_BASE: u32 = 0x1100;
const HANGUL_V_BASE: u32 = 0x1161;
const HANGUL_T_BASE: u32 = 0x11A7;
const HANGUL_L_COUNT: u32 = 19;
const HANGUL_V_COUNT: u32 = 21;
const HANGUL_T_COUNT: u32 = 28;
const HANGUL_N_COUNT: u32 = HANGUL_V_COUNT * HANGUL_T_COUNT;
const HANGUL_S_COUNT: u32 = HANGUL_L_COUNT * HANGUL_N_COUNT;

/// Returns the Unicode 17 canonical decomposed form of `value`.
pub fn normalize_nfd(value: &str) -> String {
    if value.is_ascii() {
        return value.to_owned();
    }
    decompose(value.chars()).into_iter().collect()
}

/// Returns the Unicode 17 canonical composed form of `value`.
pub fn normalize_nfc(value: &str) -> String {
    if value.is_ascii() {
        return value.to_owned();
    }
    compose(decompose(value.chars()))
}

/// Preserves Rootlight's Unicode 9 case-folded, Unicode 17 composed key form.
pub fn canonical_case_fold(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    if let Some(folded) = fold_independent_starters(value) {
        return folded;
    }
    // A decomposition that begins with a starter cannot reorder across the
    // next starter boundary, so only the folded output needs composition.
    if value.chars().all(is_canonical_boundary_starter) {
        return normalize_nfc(&value.case_fold().collect::<String>());
    }
    canonical_case_fold_full(value)
}

fn canonical_case_fold_full(value: &str) -> String {
    let decomposed = decompose(value.chars());
    compose(decompose(decomposed.into_iter().case_fold()))
}

fn fold_independent_starters(value: &str) -> Option<String> {
    let mut folded = String::new();
    for original in value.chars() {
        if !is_independent_nfc_starter(original) {
            return None;
        }
        for character in original.case_fold() {
            if !is_independent_nfc_starter(character) {
                return None;
            }
            folded.push(character);
        }
    }
    Some(folded)
}

fn is_independent_nfc_starter(character: char) -> bool {
    if character.is_ascii() {
        return true;
    }
    let code = u32::from(character);
    // A non-starter, non-NFC scalar, or starter that can complete a preceding
    // composition must use the full canonical-ordering path.
    combining_class(character) == 0
        && normalization_data::NON_NFC_SCALARS
            .binary_search(&character)
            .is_err()
        && normalization_data::COMPOSABLE_STARTER_SUFFIXES
            .binary_search(&character)
            .is_err()
        && !(HANGUL_V_BASE..HANGUL_V_BASE + HANGUL_V_COUNT).contains(&code)
        && !(HANGUL_T_BASE + 1..HANGUL_T_BASE + HANGUL_T_COUNT).contains(&code)
}

fn is_canonical_boundary_starter(character: char) -> bool {
    combining_class(character) == 0
        && normalization_data::STARTER_TO_NONSTARTER
            .binary_search(&character)
            .is_err()
}

fn decompose(input: impl IntoIterator<Item = char>) -> Vec<char> {
    let mut output = Vec::new();
    for character in input {
        append_decomposed(character, &mut output);
    }
    output
}

fn append_decomposed(character: char, output: &mut Vec<char>) {
    if character.is_ascii() {
        output.push(character);
        return;
    }
    let code = u32::from(character);
    if (HANGUL_S_BASE..HANGUL_S_BASE + HANGUL_S_COUNT).contains(&code) {
        let syllable = code - HANGUL_S_BASE;
        let leading = HANGUL_L_BASE + syllable / HANGUL_N_COUNT;
        let vowel = HANGUL_V_BASE + syllable % HANGUL_N_COUNT / HANGUL_T_COUNT;
        append_ordered(
            char::from_u32(leading).expect("Hangul leading jamo is a scalar"),
            output,
        );
        append_ordered(
            char::from_u32(vowel).expect("Hangul vowel jamo is a scalar"),
            output,
        );
        let trailing = syllable % HANGUL_T_COUNT;
        if trailing != 0 {
            append_ordered(
                char::from_u32(HANGUL_T_BASE + trailing).expect("Hangul trailing jamo is a scalar"),
                output,
            );
        }
        return;
    }
    let index = indexed_value(
        character,
        normalization_data::DECOMPOSITION_PAGE_MAP,
        normalization_data::DECOMPOSITION_PAGES,
    );
    if index != 0 {
        for &part in normalization_data::DECOMPOSITION[usize::from(index) - 1].1 {
            append_decomposed(part, output);
        }
    } else {
        append_ordered(character, output);
    }
}

fn append_ordered(character: char, output: &mut Vec<char>) {
    let class = combining_class(character);
    if class == 0 {
        output.push(character);
        return;
    }
    // Stable insertion cannot cross a starter or an equal-class mark.
    let mut index = output.len();
    while index > 0 && combining_class(output[index - 1]) > class {
        index -= 1;
    }
    output.insert(index, character);
}

fn compose(decomposed: Vec<char>) -> String {
    let mut output = Vec::with_capacity(decomposed.len());
    let mut starter = None;
    let mut preceding_class = 0;
    for character in decomposed {
        let class = combining_class(character);
        if let Some(index) = starter
            && (preceding_class == 0 || preceding_class < class)
            && (class != 0 || can_complete_starter_pair(character))
            && let Some(composed) = compose_pair(output[index], character)
        {
            output[index] = composed;
            continue;
        }
        if class == 0 {
            starter = Some(output.len());
        }
        preceding_class = class;
        output.push(character);
    }
    output.into_iter().collect()
}

fn compose_pair(first: char, second: char) -> Option<char> {
    let first_code = u32::from(first);
    let second_code = u32::from(second);
    if (HANGUL_L_BASE..HANGUL_L_BASE + HANGUL_L_COUNT).contains(&first_code)
        && (HANGUL_V_BASE..HANGUL_V_BASE + HANGUL_V_COUNT).contains(&second_code)
    {
        let syllable = HANGUL_S_BASE
            + (first_code - HANGUL_L_BASE) * HANGUL_N_COUNT
            + (second_code - HANGUL_V_BASE) * HANGUL_T_COUNT;
        return char::from_u32(syllable);
    }
    if (HANGUL_S_BASE..HANGUL_S_BASE + HANGUL_S_COUNT).contains(&first_code)
        && (first_code - HANGUL_S_BASE).is_multiple_of(HANGUL_T_COUNT)
        && (HANGUL_T_BASE + 1..HANGUL_T_BASE + HANGUL_T_COUNT).contains(&second_code)
    {
        return char::from_u32(first_code + second_code - HANGUL_T_BASE);
    }
    let index = normalization_data::COMPOSITION
        .binary_search_by_key(&(first, second), |entry| entry.0)
        .ok()?;
    Some(normalization_data::COMPOSITION[index].1)
}

fn can_complete_starter_pair(character: char) -> bool {
    let code = u32::from(character);
    (HANGUL_V_BASE..HANGUL_V_BASE + HANGUL_V_COUNT).contains(&code)
        || (HANGUL_T_BASE + 1..HANGUL_T_BASE + HANGUL_T_COUNT).contains(&code)
        || normalization_data::COMPOSABLE_STARTER_SUFFIXES
            .binary_search(&character)
            .is_ok()
}

fn combining_class(character: char) -> u8 {
    if character.is_ascii() {
        return 0;
    }
    let value = indexed_value(
        character,
        normalization_data::COMBINING_PAGE_MAP,
        normalization_data::COMBINING_PAGES,
    );
    u8::try_from(value).expect("generated combining class fits in u8")
}

fn indexed_value(character: char, map: &[u16], pages: &[[u16; 256]]) -> u16 {
    // Every scalar fits the generated 0x1100-page map, and its page IDs are
    // generated only from entries in `pages`.
    let code = usize::try_from(u32::from(character)).expect("Unicode scalar fits usize");
    pages[usize::from(map[code >> 8])][code & 0xff]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_17_normalization_conformance() {
        let data = include_str!("../data/NormalizationTest-17.0.0.txt");
        assert!(data.starts_with("# NormalizationTest-17.0.0.txt\n"));
        let mut cases = 0;
        for line in data.lines() {
            let record = line
                .split_once('#')
                .map_or(line, |(prefix, _)| prefix)
                .trim();
            if record.is_empty() || record.starts_with('@') {
                continue;
            }
            let columns = record
                .split(';')
                .take(5)
                .map(|field| {
                    field
                        .split_whitespace()
                        .map(|code| {
                            char::from_u32(u32::from_str_radix(code, 16).expect("hex scalar"))
                                .expect("Unicode scalar")
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            assert_eq!(columns.len(), 5, "invalid normalization test record");
            for source in &columns[..3] {
                assert_eq!(normalize_nfc(source), columns[1], "NFC case {cases}");
                assert_eq!(normalize_nfd(source), columns[2], "NFD case {cases}");
            }
            for source in &columns[3..] {
                assert_eq!(normalize_nfc(source), columns[3], "NFC case {cases}");
                assert_eq!(normalize_nfd(source), columns[4], "NFD case {cases}");
            }
            cases += 1;
        }
        assert!(cases > 15_000, "the pinned conformance suite was truncated");
    }

    #[test]
    fn canonical_keys_preserve_decomposition_and_full_folding() {
        assert_eq!(canonical_case_fold("Straße"), "strasse");
        assert_eq!(
            canonical_case_fold("ÉCOLE"),
            canonical_case_fold("E\u{301}cole")
        );
        assert_eq!(canonical_case_fold("\u{212B}"), "å");
        assert_eq!(NORMALIZATION_UNICODE_VERSION, (17, 0, 0));
    }

    #[test]
    fn fast_paths_match_full_normalization_for_all_scalars() {
        for code in 0..=0x10ffff {
            let Some(character) = char::from_u32(code) else {
                continue;
            };
            let source = character.to_string();
            assert_eq!(
                canonical_case_fold(&source),
                canonical_case_fold_full(&source),
                "U+{code:04X}"
            );
        }
    }

    #[test]
    fn starter_fast_path_respects_composition_boundaries() {
        for source in [
            "Straße résumé ÉCOLE café",
            "Cafe\u{301} A\u{030A} e\u{0323}\u{0301}",
            "각",
            "\u{9C7}\u{9BE}",
            "\u{212B}",
            "\u{F73}A",
            "Ὀδυσσεύς καὶ Ἀθηνᾶ",
        ] {
            assert_eq!(
                canonical_case_fold(source),
                canonical_case_fold_full(source)
            );
        }
        let mut state = 0x9e3779b97f4a7c15_u64;
        for _ in 0..100_000 {
            let mut source = String::new();
            for _ in 0..4 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let code = u32::try_from((state >> 24) % 0x110000).expect("bounded scalar");
                if let Some(character) = char::from_u32(code) {
                    source.push(character);
                }
            }
            assert_eq!(
                canonical_case_fold(&source),
                canonical_case_fold_full(&source)
            );
        }
    }
}
