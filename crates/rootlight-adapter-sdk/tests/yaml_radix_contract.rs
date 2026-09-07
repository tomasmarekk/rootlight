//! Exact public YAML numeric identities across machine-word and decimal-limb boundaries.
//! A deliberately small decimal-digit oracle is independent of the production
//! chunk representation; benchmarks, not timing assertions, cover throughput.

use rootlight_adapter_sdk::YamlDocumentContext;

fn name(source: &str, maximum: usize) -> Option<String> {
    YamlDocumentContext::new(None, &[], maximum)?
        .flow_scalar(source, None)
        .map(|value| value.into_name())
}

fn decimal_oracle(digits: &str, radix: u32) -> String {
    let mut decimal = vec![0_u32];
    for digit in digits.chars() {
        let mut carry = digit.to_digit(radix).unwrap();
        for value in &mut decimal {
            let expanded = *value * radix + carry;
            *value = expanded % 10;
            carry = expanded / 10;
        }
        while carry != 0 {
            decimal.push(carry % 10);
            carry /= 10;
        }
    }
    let mut result = "int:".to_owned();
    result.extend(
        decimal
            .iter()
            .rev()
            .map(|&digit| char::from_digit(digit, 10).unwrap()),
    );
    result
}

#[test]
fn chunked_radices_match_a_decimal_digit_oracle_for_wide_and_sparse_inputs() {
    for (radix, prefix, alphabet) in [(16, "0x", "fF09aA17"), (8, "0o", "77001031")] {
        for length in [
            1, 2, 7, 8, 9, 10, 11, 16, 17, 31, 32, 33, 64, 65, 257, 1024, 4096,
        ] {
            for pattern in [alphabet, "1", "0"] {
                let digits: String = pattern.chars().cycle().take(length).collect();
                let expected = decimal_oracle(&digits, radix);
                for padding in ["", "00000000000000000"] {
                    let source = format!("{prefix}{padding}{digits}");
                    let maximum = source.len().max(expected.len());
                    assert_eq!(
                        name(&source, maximum),
                        Some(expected.clone()),
                        "{radix} length {length}"
                    );
                    let quoted = format!("'{source}'");
                    let tag_budget = (quoted.len() + "!!int".len())
                        .max(expected.len())
                        .max("tag:yaml.org,2002:int".len());
                    let context = YamlDocumentContext::new(None, &[], tag_budget).unwrap();
                    assert_eq!(
                        context
                            .flow_scalar(&quoted, Some("!!int"))
                            .unwrap_or_else(|| panic!("tagged {quoted:?}, budget {tag_budget}"))
                            .name(),
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn radix_output_budget_boundaries_never_drop_high_digits_or_zero_padding() {
    for digits in [
        "ffffffff",
        "100000000",
        "ffffffffffffffff",
        "10000000000000001",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    ] {
        let source = format!("0x{digits}");
        let expected = decimal_oracle(digits, 16);
        for maximum in 0..=expected.len() + 2 {
            let admitted = source.len() <= maximum && expected.len() <= maximum;
            assert_eq!(
                name(&source, maximum),
                admitted.then(|| expected.clone()),
                "{source}: {maximum}"
            );
        }
    }
    for zeros in [0, 1, 8, 9, 10, 31, 64, 4096] {
        let source = format!("0x{}1", "0".repeat(zeros));
        assert_eq!(name(&source, source.len().max(5)), Some("int:1".to_owned()));
        assert!(
            name(&source, source.len() - 1).is_none(),
            "source budget still applies"
        );
    }
}

#[test]
fn word_boundaries_and_decimal_limb_carries_match_machine_formatting() {
    for base in [
        10_u128.pow(9),
        10_u128.pow(18),
        10_u128.pow(27),
        1_u128 << 32,
        1_u128 << 64,
        1_u128 << 96,
    ] {
        for delta in [0, 1, 2, 17] {
            for value in [base - delta, base + delta, u128::MAX - delta] {
                let expected = format!("int:{value}");
                for source in [
                    format!("0x{value:x}"),
                    format!("0x{value:X}"),
                    format!("0o{value:o}"),
                ] {
                    assert_eq!(name(&source, 128), Some(expected.clone()));
                }
            }
        }
    }
}
