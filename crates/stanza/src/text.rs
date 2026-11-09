use std::cmp::Ordering;

use icu_collator::CollatorBorrowed;
use icu_collator::options::CollatorOptions;

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

static COLLATOR: CollatorBorrowed<'static> = CollatorBorrowed::new_root(CollatorOptions::default());

pub fn locale_compare(left: &str, right: &str) -> Ordering {
    COLLATOR.compare(left, right)
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

    #[test]
    fn collates_like_bun() {
        let mut words = [
            "z", "B", "a.ts", "é", "a/b", "b", "A.ts", "a.b", "a-b", "a_b",
        ];
        words.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(
            words,
            [
                "a_b", "a-b", "a.b", "a.ts", "A.ts", "a/b", "b", "B", "é", "z"
            ]
        );
    }

    fn corpus_paths() -> Vec<String> {
        let home = std::env::var("HOME").unwrap_or_default();
        let corpora = ["Kingshot", "Stanza", "meisai", "F&S", "caasx"]
            .map(|name| format!("{home}/Projects/{name}"));
        let roots = corpora
            .iter()
            .map(String::as_str)
            .chain([concat!(env!("CARGO_MANIFEST_DIR"), "/../..")]);

        roots
            .filter_map(|root| {
                std::process::Command::new("git")
                    .args(["-C", root, "ls-files", "-z"])
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
            })
            .flat_map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn generated_strings(count: usize) -> Vec<String> {
        let pools: [&[char]; 8] = [
            &['a', 'b', 'k', 'z', 'A', 'B', 'K', 'Z', '0', '7', '9'],
            &['-', '_', '.', '/', ' ', '@', '#', '(', ')', '\'', '~'],
            &[
                'é', 'è', 'ê', 'ë', 'à', 'ç', 'ñ', 'ö', 'ü', 'ß', 'Å', 'ø', 'É', 'İ', 'ı',
            ],
            &['\u{301}', '\u{308}', '\u{327}', '\u{200d}', '\u{fe0f}'],
            &[
                '漢', '字', '中', '文', '日', '本', '語', 'あ', 'ア', 'ｱ', '〜',
            ],
            &['가', '나', '한', '글', '힣', 'ᄀ', 'ᅡ', 'ᆨ', 'ㄱ'],
            &['😀', '👍', '🏽', '🇯', '🇵', '🧑', '💻', '❤'],
            &['Ω', 'ω', 'Ж', 'ж', 'ا', 'ب', 'א', 'ก', '½', 'Ⅳ'],
        ];
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };

        (0..count)
            .map(|_| {
                let length = 1 + next(12);
                (0..length)
                    .map(|_| {
                        let pool = pools[next(pools.len())];
                        pool[next(pool.len())]
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn root_collator_orders_like_the_locale_collator() {
        let locale =
            icu_collator::Collator::try_new(Default::default(), CollatorOptions::default())
                .expect("the locale collation data is compiled in");
        let mut inputs = corpus_paths();
        inputs.extend(generated_strings(10_000));
        inputs.sort_by(|left, right| locale_compare(left, right));

        let disagreement = inputs
            .windows(2)
            .find(|pair| locale.compare(&pair[0], &pair[1]) != locale_compare(&pair[0], &pair[1]));
        assert_eq!(disagreement, None);
    }
}
