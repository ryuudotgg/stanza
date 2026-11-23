use std::cmp::Ordering;

use regex_syntax::hir::{Class, HirKind};

use super::directives::continues_identifier;

struct UnicodeClass(Vec<(char, char)>);

impl UnicodeClass {
    fn new(source: &str) -> Self {
        let hir = regex_syntax::parse(source).expect("the class parses");
        let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
            panic!("{source} is a Unicode class");
        };

        Self(
            class
                .ranges()
                .iter()
                .map(|range| (range.start(), range.end()))
                .collect(),
        )
    }

    fn contains(&self, char: char) -> bool {
        self.0
            .binary_search_by(|&(start, end)| {
                if end < char {
                    Ordering::Less
                } else if start > char {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .is_ok()
    }
}

#[test]
fn identifier_continuation_differs_from_the_regex_crate_only_on_unassigned_code_points() {
    let id_continue = UnicodeClass::new(r"\p{ID_Continue}");
    let unassigned = UnicodeClass::new(r"\p{Cn}");
    let old = regex::Regex::new(r"^\p{ID_Continue}").expect("the old pattern compiles");
    let mut buffer = [0; 4];
    for char in (0..=0x10_ffff).filter_map(char::from_u32) {
        let text = char.encode_utf8(&mut buffer);
        if (char as u32).is_multiple_of(61) {
            assert_eq!(
                old.is_match(text),
                id_continue.contains(char),
                "the class table matches the regex crate on {text:?}"
            );
        }

        if id_continue.contains(char) != continues_identifier(text) {
            assert!(
                unassigned.contains(char),
                "identifier continuation differs on assigned {text:?}"
            );
        }
    }
}
