pub fn is_js_whitespace(char: char) -> bool {
    matches!(
        char,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}'
    ) || matches!(char, '\u{2000}'..='\u{200a}')
        || matches!(
            char,
            '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        )
        || char == '\u{feff}'
}

pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

pub fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

fn decimal_literal(text: &str) -> bool {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };

    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let exponent_ok = exponent.is_none_or(|exponent| {
        let unsigned = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !unsigned.is_empty() && digits(unsigned)
    });

    !(whole.is_empty() && fraction.is_empty()) && digits(whole) && digits(fraction) && exponent_ok
}

pub fn js_number(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }

    let radixes = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ];

    for (prefix, radix) in radixes {
        let Some(digits) = text.strip_prefix(prefix) else {
            continue;
        };

        if digits.is_empty() || !digits.chars().all(|char| char.is_digit(radix)) {
            return f64::NAN;
        }

        return u128::from_str_radix(digits, radix).map_or_else(
            |_| {
                digits.chars().fold(0.0, |value, char| {
                    value * f64::from(radix) + f64::from(char.to_digit(radix).unwrap_or(0))
                })
            },
            |value| value as f64,
        );
    }

    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }

    if !decimal_literal(unsigned) {
        return f64::NAN;
    }

    text.parse().unwrap_or(f64::NAN)
}

pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

pub fn js_number_string(number: f64) -> String {
    if number.is_finite() {
        return dragonbox_ecma::Buffer::new()
            .format_finite(number)
            .to_owned();
    }

    let name = if number.is_nan() {
        "NaN"
    } else if number > 0.0 {
        "Infinity"
    } else {
        "-Infinity"
    };

    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_what_javascript_trims() {
        assert_eq!(js_trim("\u{feff}\u{a0} x \u{3000}"), "x");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
    }

    #[test]
    fn prints_numbers_as_javascript_does() {
        let printed: Vec<String> = [120.0, 1e20, 1e21, 1.5e22, f64::INFINITY]
            .into_iter()
            .map(js_number_string)
            .collect();

        assert_eq!(
            printed,
            [
                "120",
                "100000000000000000000",
                "1e+21",
                "1.5e+22",
                "Infinity"
            ]
        );
    }

    #[test]
    fn counts_utf16_units() {
        assert_eq!(utf16_len("a😀é"), 4);
    }
}
