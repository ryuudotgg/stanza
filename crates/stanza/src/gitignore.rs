#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Xdigit,
}

impl Class {
    fn named(name: &[u8]) -> Option<Class> {
        Some(match name {
            b"alnum" => Class::Alnum,
            b"alpha" => Class::Alpha,
            b"blank" => Class::Blank,
            b"cntrl" => Class::Cntrl,
            b"digit" => Class::Digit,
            b"graph" => Class::Graph,
            b"lower" => Class::Lower,
            b"print" => Class::Print,
            b"punct" => Class::Punct,
            b"space" => Class::Space,
            b"upper" => Class::Upper,
            b"xdigit" => Class::Xdigit,
            _ => return None,
        })
    }

    fn holds(self, byte: u8) -> bool {
        let upper = byte.is_ascii_uppercase();
        let lower = byte.is_ascii_lowercase();
        let digit = byte.is_ascii_digit();
        match self {
            Class::Alnum => upper || lower || digit,
            Class::Alpha => upper || lower,
            Class::Blank => byte == b' ' || byte == b'\t',
            Class::Cntrl => byte < 32 || byte == 127,
            Class::Digit => digit,
            Class::Graph => (33..=126).contains(&byte),
            Class::Lower => lower,
            Class::Print => (32..=126).contains(&byte),
            Class::Punct => (33..=126).contains(&byte) && !upper && !lower && !digit,
            Class::Space => byte == b' ' || (9..=13).contains(&byte),
            Class::Upper => upper,
            Class::Xdigit => byte.is_ascii_hexdigit(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CharacterSet {
    singles: Vec<u8>,
    ranges: Vec<(u8, u8)>,
    classes: Vec<Class>,
    negated: bool,
}

impl CharacterSet {
    fn admits(&self, byte: u8) -> bool {
        let included = self.singles.contains(&byte)
            || self
                .ranges
                .iter()
                .any(|&(start, end)| start <= byte && byte <= end)
            || self.classes.iter().any(|class| class.holds(byte));

        included != self.negated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(u8),
    Star,
    Globstar,
    Directories,
    Any,
    Set(CharacterSet),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    tokens: Vec<Token>,
    anchored: bool,
    directory: bool,
    negated: bool,
}

fn posix_class(pattern: &[u8], index: usize) -> Option<(Class, usize)> {
    let rest = pattern.get(index..)?.strip_prefix(b"[:")?;
    let length = rest
        .iter()
        .take_while(|byte| byte.is_ascii_lowercase())
        .count();
    if length == 0 || !rest[length..].starts_with(b":]") {
        return None;
    }

    Some((Class::named(&rest[..length])?, length + 4))
}

fn character_set(pattern: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
    if negated {
        index += 1;
    }

    let mut set = CharacterSet {
        singles: Vec::new(),
        ranges: Vec::new(),
        classes: Vec::new(),
        negated,
    };

    let mut first = true;
    let mut previous: Option<u8> = None;
    while index < pattern.len() {
        if pattern[index] == b']' && !first {
            set.singles.extend(previous);
            return Some((Token::Set(set), index + 1));
        }

        first = false;
        if let Some((class, length)) = posix_class(pattern, index) {
            set.singles.extend(previous.take());
            set.classes.push(class);
            index += length;
            continue;
        }

        let mut byte = pattern[index];
        let escaped = byte == b'\\';
        if escaped {
            index += 1;
            byte = *pattern.get(index)?;
        }

        let range = !escaped
            && byte == b'-'
            && previous.is_some()
            && index + 1 < pattern.len()
            && pattern[index + 1] != b']';

        if range {
            index += 1;
            let mut end = pattern[index];
            if end == b'\\' {
                index += 1;
                end = *pattern.get(index)?;
            }

            set.ranges.push((previous.take()?, end));
        } else {
            set.singles.extend(previous.replace(byte));
        }

        index += 1;
    }

    None
}

fn tokenize(pattern: &[u8]) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => {
                tokens.push(Token::Literal(*pattern.get(index + 1)?));
                index += 2;
            }
            b'[' => {
                let (token, end) = character_set(pattern, index)?;
                tokens.push(token);
                index = end;
            }
            b'*' => {
                let start = index;
                while pattern.get(index) == Some(&b'*') {
                    index += 1;
                }

                let run = index - start;
                let after_boundary = start == 0 || pattern[start - 1] == b'/';
                if run > 1 && after_boundary && pattern.get(index) == Some(&b'/') {
                    tokens.push(Token::Directories);
                    index += 1;
                } else if run > 1 && after_boundary && index == pattern.len() {
                    tokens.push(Token::Globstar);
                } else {
                    tokens.push(Token::Star);
                }
            }
            b'?' => {
                tokens.push(Token::Any);
                index += 1;
            }
            byte => {
                tokens.push(Token::Literal(byte));
                index += 1;
            }
        }
    }

    Some(tokens)
}

fn trailing_spaces_trimmed(line: &[u8]) -> &[u8] {
    let mut trailing = line.len();
    while trailing > 0 && line[trailing - 1] == b' ' {
        let slashes = line[..trailing - 1]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b'\\')
            .count();

        if slashes % 2 == 1 {
            break;
        }

        trailing -= 1;
    }

    &line[..trailing]
}

pub fn parse_ignore(text: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for line in text.as_bytes().split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }

        let mut line = trailing_spaces_trimmed(line);
        if line.is_empty() {
            continue;
        }

        let negated = line[0] == b'!';
        if negated {
            line = &line[1..];
        }

        let directory = line.last() == Some(&b'/');
        if directory {
            line = &line[..line.len() - 1];
        }

        let anchored = line.contains(&b'/');
        if line.first() == Some(&b'/') {
            line = &line[1..];
        }

        if let Some(tokens) = tokenize(line) {
            rules.push(Rule {
                tokens,
                anchored,
                directory,
                negated,
            });
        }
    }

    rules
}

fn matches(tokens: &[Token], path: &[u8]) -> bool {
    let length = path.len();
    let mut next: Vec<bool> = (0..=length).map(|index| index == length).collect();
    for token in tokens.iter().rev() {
        let mut current = vec![false; length + 1];
        let mut through_slash = vec![false; length + 1];
        for index in (0..=length).rev() {
            let byte = path.get(index).copied();
            current[index] = match token {
                Token::Literal(literal) => byte == Some(*literal) && next[index + 1],
                Token::Any => byte.is_some_and(|byte| byte != b'/') && next[index + 1],
                Token::Set(set) => {
                    byte.is_some_and(|byte| byte != b'/' && set.admits(byte)) && next[index + 1]
                }
                Token::Directories => {
                    through_slash[index] = byte.is_some()
                        && ((byte == Some(b'/') && next[index + 1]) || through_slash[index + 1]);

                    next[index] || through_slash[index]
                }
                Token::Star | Token::Globstar => {
                    next[index]
                        || byte.is_some_and(|byte| {
                            (matches!(token, Token::Globstar) || byte != b'/') && current[index + 1]
                        })
                }
            };
        }

        next = current;
    }

    next[0]
}

pub fn ignored_by_rules(rules: &[Rule], path: &str, is_directory: bool) -> Option<bool> {
    let target = path.as_bytes();
    let name = match target.iter().rposition(|&byte| byte == b'/') {
        Some(slash) => &target[slash + 1..],
        None => target,
    };

    let mut ignored = None;
    for rule in rules {
        if rule.directory && !is_directory {
            continue;
        }

        if matches(&rule.tokens, if rule.anchored { target } else { name }) {
            ignored = Some(!rule.negated);
        }
    }

    ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignored(patterns: &str, path: &str) -> Option<bool> {
        ignored_by_rules(&parse_ignore(patterns), path, false)
    }

    #[test]
    fn matches_the_reference_wildcards() {
        assert_eq!(ignored("?.ts", "a.ts"), Some(true));
        assert_eq!(ignored("?.ts", "ab.ts"), None);
        assert_eq!(ignored("[!a].ts", "a.ts"), None);
        assert_eq!(ignored("[a\\-z].ts", "-.ts"), Some(true));
        assert_eq!(ignored("[a\\-z].ts", "b.ts"), None);
        assert_eq!(ignored("[]a].ts", "].ts"), Some(true));
        assert_eq!(ignored("[[:digit:]]x.ts", "5x.ts"), Some(true));
        assert_eq!(ignored("a/**/b.ts", "a/x/y/b.ts"), Some(true));
        assert_eq!(ignored("a/**/b.ts", "a/b.ts"), Some(true));
        assert_eq!(ignored("a/**/b.ts", "a/xb.ts"), None);
        assert_eq!(ignored("keep/**", "keep/x/f.ts"), Some(true));
        assert_eq!(ignored("*.ts\n!keep.ts", "z/keep.ts"), Some(false));
        assert_eq!(ignored("/root.ts", "z/root.ts"), None);
        assert_eq!(ignored("[x.ts", "[x.ts"), None);
        assert_eq!(ignored("bad\\", "bad"), None);
        assert_eq!(ignored("sp\\ ", "sp "), Some(true));
        assert_eq!(ignored("tr ", "tr"), Some(true));
        assert_eq!(ignored("/v?.ts", "v\u{e9}.ts"), None);
        assert_eq!(ignored("/w??.ts", "w\u{e9}.ts"), Some(true));
    }

    #[test]
    fn directory_rules_need_a_directory() {
        let rules = parse_ignore("logs.ts/\n");
        assert_eq!(ignored_by_rules(&rules, "z/logs.ts", false), None);
        assert_eq!(ignored_by_rules(&rules, "logs.ts", true), Some(true));
    }
}
