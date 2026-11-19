fn class(name: &[u8]) -> Option<fn(u8) -> bool> {
    Some(match name {
        b"alnum" => |byte| byte.is_ascii_alphanumeric(),
        b"alpha" => |byte| byte.is_ascii_alphabetic(),
        b"blank" => |byte| byte == b' ' || byte == b'\t',
        b"cntrl" => |byte| byte.is_ascii_control(),
        b"digit" => |byte| byte.is_ascii_digit(),
        b"graph" => |byte| byte.is_ascii_graphic(),
        b"lower" => |byte| byte.is_ascii_lowercase(),
        b"print" => |byte| byte == b' ' || byte.is_ascii_graphic(),
        b"punct" => |byte| byte.is_ascii_punctuation(),
        b"space" => |byte| byte == b' ' || (9..=13).contains(&byte),
        b"upper" => |byte| byte.is_ascii_uppercase(),
        b"xdigit" => |byte| byte.is_ascii_hexdigit(),
        _ => return None,
    })
}

struct ByteSet([u64; 4]);

impl Extend<u8> for ByteSet {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, bytes: T) {
        for byte in bytes {
            self.0[usize::from(byte >> 6)] |= 1 << (byte & 63);
        }
    }
}

impl ByteSet {
    fn contains(&self, byte: u8) -> bool {
        self.0[usize::from(byte >> 6)] & (1 << (byte & 63)) != 0
    }
}

enum Token {
    Literal(u8),
    Star,
    Globstar,
    Directories,
    Any,
    Set(ByteSet),
}

fn literals(tokens: &[Token]) -> Option<Vec<u8>> {
    tokens
        .iter()
        .map(|token| match token {
            Token::Literal(byte) => Some(*byte),
            _ => None,
        })
        .collect()
}

enum Pattern {
    Exact(Vec<u8>),
    Suffix(Vec<u8>),
    Glob(Vec<Token>),
}

impl Pattern {
    fn new(tokens: Vec<Token>) -> Pattern {
        if let Some(bytes) = literals(&tokens) {
            return Pattern::Exact(bytes);
        }

        if let Some((Token::Star, rest)) = tokens.split_first()
            && let Some(bytes) = literals(rest)
        {
            return Pattern::Suffix(bytes);
        }

        Pattern::Glob(tokens)
    }

    fn matches(&self, path: &[u8]) -> bool {
        match self {
            Pattern::Exact(bytes) => path == bytes.as_slice(),
            Pattern::Suffix(bytes) => path
                .strip_suffix(bytes.as_slice())
                .is_some_and(|head| !head.contains(&b'/')),
            Pattern::Glob(tokens) => glob_matches(tokens, path),
        }
    }
}

pub struct Rule {
    pattern: Pattern,
    anchored: bool,
    directory: bool,
    negated: bool,
}

enum Bracket {
    Class(fn(u8) -> bool, usize),
    Member,
    Invalid,
}

fn posix_class(pattern: &[u8], index: usize, close: &mut usize) -> Bracket {
    if !pattern[index..].starts_with(b"[:") {
        return Bracket::Member;
    }

    let from = index + 2;
    if *close < from {
        *close = pattern[from..]
            .iter()
            .position(|&byte| byte == b']')
            .map_or(pattern.len(), |end| from + end);
    }

    if *close == pattern.len() {
        return Bracket::Member;
    }

    match pattern[from..*close].strip_suffix(b":").map(class) {
        Some(Some(class)) => Bracket::Class(class, *close + 1 - index),
        Some(None) => Bracket::Invalid,
        None => Bracket::Member,
    }
}

fn character_set(pattern: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
    if negated {
        index += 1;
    }

    let mut set = ByteSet([0; 4]);
    let mut first = true;
    let mut previous: Option<u8> = None;
    let mut close = 0;
    while index < pattern.len() {
        if pattern[index] == b']' && !first {
            set.extend(previous);
            if negated {
                set.0 = set.0.map(|bits| !bits);
            }

            set.0[0] &= !(1 << b'/');
            return Some((Token::Set(set), index + 1));
        }

        first = false;
        match posix_class(pattern, index, &mut close) {
            Bracket::Class(holds, length) => {
                set.extend(previous.take());
                set.extend((0..=u8::MAX).filter(|&byte| holds(byte)));
                index += length;
                continue;
            }
            Bracket::Invalid => return None,
            Bracket::Member => {}
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

            let start = previous.take()?;
            // git admits a range's first byte even when the range is reversed
            set.extend([start]);
            set.extend(start..=end);
        } else {
            set.extend(previous.replace(byte));
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

                let rest = &pattern[index..];
                let globstar = index - start > 1 && (start == 0 || pattern[start - 1] == b'/');
                if globstar && rest.first() == Some(&b'/') {
                    tokens.push(Token::Directories);
                    index += 1;
                } else if globstar && (rest.is_empty() || rest.starts_with(b"\\/")) {
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

pub fn parse_ignore(text: impl AsRef<[u8]>) -> Vec<Rule> {
    let text = text.as_ref();
    let text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    let mut rules = Vec::new();
    for line in text.split(|&byte| byte == b'\n') {
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
                pattern: Pattern::new(tokens),
                anchored,
                directory,
                negated,
            });
        }
    }

    rules
}

fn glob_matches(tokens: &[Token], path: &[u8]) -> bool {
    let length = path.len();
    let mut next = vec![false; length + 1];
    let mut current = vec![false; length + 1];
    next[length] = true;
    for token in tokens.iter().rev() {
        let mut through_slash = false;
        for index in (0..=length).rev() {
            let byte = path.get(index).copied();
            current[index] = match token {
                Token::Literal(literal) => byte == Some(*literal) && next[index + 1],
                Token::Any => byte.is_some_and(|byte| byte != b'/') && next[index + 1],
                Token::Set(set) => byte.is_some_and(|byte| set.contains(byte)) && next[index + 1],
                Token::Directories => {
                    through_slash = byte.is_some()
                        && ((byte == Some(b'/') && next[index + 1]) || through_slash);

                    next[index] || through_slash
                }
                Token::Star | Token::Globstar => {
                    next[index]
                        || byte.is_some_and(|byte| {
                            (matches!(token, Token::Globstar) || byte != b'/') && current[index + 1]
                        })
                }
            };
        }

        std::mem::swap(&mut next, &mut current);
    }

    next[0]
}

pub fn ignored_by_rules(
    rules: &[Rule],
    path: impl AsRef<[u8]>,
    is_directory: bool,
) -> Option<bool> {
    let target = path.as_ref();
    let name = match target.iter().rposition(|&byte| byte == b'/') {
        Some(slash) => &target[slash + 1..],
        None => target,
    };

    rules
        .iter()
        .rev()
        .find(|rule| {
            (is_directory || !rule.directory)
                && rule
                    .pattern
                    .matches(if rule.anchored { target } else { name })
        })
        .map(|rule| !rule.negated)
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
    fn follows_git_where_its_parsing_differs_from_a_plain_glob() {
        assert_eq!(ignored("\u{feff}a.ts", "a.ts"), Some(true));
        assert_eq!(ignored("\u{feff}#a.ts", "\u{feff}#a.ts"), None);
        assert_eq!(ignored("\u{feff}!keep.ts", "keep.ts"), Some(false));
        assert_eq!(ignored("a.ts\n\u{feff}b.ts", "b.ts"), None);
        assert_eq!(ignored("x**\\/b", "xy/b"), Some(true));
        assert_eq!(ignored("x**\\/b", "xy/z/b"), None);
        assert_eq!(ignored("[[:foo:]]x.ts", "f]x.ts"), None);
        assert_eq!(ignored("[[:foo:]]x.ts\n!x.ts", "x.ts"), Some(false));
        assert_eq!(ignored("[[:a-b:]]x.ts", "ax.ts"), None);
        assert_eq!(ignored("[[::]]x.ts", ":x.ts"), None);
        assert_eq!(ignored("[[::]x.ts", ":x.ts"), None);
        assert_eq!(ignored("[[:x\\:]x.ts", "xx.ts"), None);
        assert_eq!(ignored("[[:]x.ts", ":x.ts"), Some(true));
        assert_eq!(ignored("[[:a]x.ts", "[x.ts"), Some(true));
        assert_eq!(ignored("[[:x.ts", "[x.ts"), None);
        assert_eq!(ignored("a/**\\/b.ts", "a/x/y/b.ts"), Some(true));
        assert_eq!(ignored("a/**\\/b.ts", "a/b.ts"), None);
        assert_eq!(ignored("[z-a]x.ts", "zx.ts"), Some(true));
        assert_eq!(ignored("[z-a]x.ts", "ax.ts"), None);
        assert_eq!(ignored("[z-a]x.ts", "mx.ts"), None);
    }

    #[test]
    fn sets_answer_every_byte_like_posix() {
        let classes: [(&str, &[(u8, u8)]); 12] = [
            ("alnum", &[(b'0', b'9'), (b'A', b'Z'), (b'a', b'z')]),
            ("alpha", &[(b'A', b'Z'), (b'a', b'z')]),
            ("blank", &[(b'\t', b'\t'), (b' ', b' ')]),
            ("cntrl", &[(0, 31), (127, 127)]),
            ("digit", &[(b'0', b'9')]),
            ("graph", &[(33, 126)]),
            ("lower", &[(b'a', b'z')]),
            ("print", &[(32, 126)]),
            ("punct", &[(33, 47), (58, 64), (91, 96), (123, 126)]),
            ("space", &[(b'\t', b'\r'), (b' ', b' ')]),
            ("upper", &[(b'A', b'Z')]),
            ("xdigit", &[(b'0', b'9'), (b'A', b'F'), (b'a', b'f')]),
        ];

        for (name, members) in classes {
            let holds = |byte| {
                members
                    .iter()
                    .any(|&(first, last)| (first..=last).contains(&byte))
            };

            let rules = parse_ignore(format!("/[[:{name}:]]"));
            let negated = parse_ignore(format!("/[![:{name}:]]"));
            for byte in 0..=u8::MAX {
                let path = [byte];
                let admitted = byte != b'/' && holds(byte);
                let refused = byte != b'/' && !holds(byte);
                assert_eq!(
                    ignored_by_rules(&rules, path, false),
                    admitted.then_some(true),
                    "[:{name}:] on {byte}"
                );
                assert_eq!(
                    ignored_by_rules(&negated, path, false),
                    refused.then_some(true),
                    "[![:{name}:]] on {byte}"
                );
            }
        }
    }

    #[test]
    fn single_byte_wildcards_never_cross_a_slash() {
        for pattern in [
            "/a?b",
            "/a*b",
            "/a[!x]b",
            "/a[/]b",
            "/a[[:punct:]]b",
            "/a[!a]*b",
        ] {
            assert_eq!(ignored(pattern, "a/b"), None, "{pattern}");
            assert_eq!(ignored(pattern, "a/x/b"), None, "{pattern}");
        }

        assert_eq!(ignored("/a*b", "axyb"), Some(true));
        assert_eq!(ignored("*.ts", "a/b.ts"), Some(true));
        assert_eq!(ignored("/*.ts", "a/b.ts"), None);
        assert_eq!(ignored("*/b.ts", "a/b.ts"), Some(true));
        assert_eq!(ignored("*/b.ts", "x/a/b.ts"), None);
    }

    fn allocations(work: impl FnOnce() -> Option<bool>) -> (Option<bool>, usize) {
        let before = crate::allocations::count();
        let answer = work();

        (answer, crate::allocations::count() - before)
    }

    #[test]
    fn literal_and_suffix_rules_match_without_allocating() {
        for (pattern, hit) in [("absent000", "absent000"), ("*.absent000", "x.absent000")] {
            let rules = parse_ignore(pattern);
            let paths = [hit.as_bytes(), b"keep.ts", b"d00/keep.ts"];

            for (path, expected) in paths.into_iter().zip([Some(true), None, None]) {
                let (answer, allocated) = allocations(|| ignored_by_rules(&rules, path, false));
                assert_eq!(answer, expected, "{pattern}");
                assert_eq!(allocated, 0, "{pattern}");
            }
        }
    }

    #[test]
    fn wildcard_rules_allocate_three_buffers_per_match() {
        let path = vec![b'a'; 256];
        for repetitions in [50, 200] {
            let rules = parse_ignore("[ab]*".repeat(repetitions));
            let (answer, allocated) = allocations(|| ignored_by_rules(&rules, &path, false));
            assert_eq!(answer, Some(true), "{repetitions}");
            assert!(
                allocated <= 3,
                "{repetitions} repetitions allocated {allocated}"
            );
        }
    }

    #[test]
    fn directory_rules_need_a_directory() {
        let rules = parse_ignore("logs.ts/\n");
        assert_eq!(ignored_by_rules(&rules, "z/logs.ts", false), None);
        assert_eq!(ignored_by_rules(&rules, "logs.ts", true), Some(true));
    }
}
