fn segments(path: &str) -> Vec<&str> {
    let mut kept: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                kept.pop();
            }
            _ => kept.push(segment),
        }
    }

    kept
}

fn absolute(segments: &[&str]) -> String {
    format!("/{}", segments.join("/"))
}

pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

pub fn resolve(cwd: &str, path: &str) -> String {
    if is_absolute(path) {
        return absolute(&segments(path));
    }

    absolute(&segments(&format!("{cwd}/{path}")))
}

pub fn join(directory: &str, name: &str) -> String {
    resolve(directory, name)
}

pub fn relative(from: &str, to: &str) -> String {
    let from = segments(from);
    let to = segments(to);
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

pub fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None if path.starts_with('/') => "/".to_owned(),
        None => ".".to_owned(),
        Some(index) => match trimmed[..index].trim_end_matches('/') {
            "" => "/".to_owned(),
            head => head.to_owned(),
        },
    }
}

pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

pub fn extname(path: &str) -> &str {
    let name = basename(path);
    match name.rfind('.') {
        Some(index) if index > 0 && name != ".." => &name[index..],
        _ => "",
    }
}

pub fn quoted(name: &[u8]) -> String {
    let mut text = String::from("\"");
    for &byte in name {
        match byte {
            b'"' => text.push_str("\\\""),
            b'\\' => text.push_str("\\\\"),
            0x07 => text.push_str("\\a"),
            0x08 => text.push_str("\\b"),
            b'\t' => text.push_str("\\t"),
            b'\n' => text.push_str("\\n"),
            0x0b => text.push_str("\\v"),
            0x0c => text.push_str("\\f"),
            b'\r' => text.push_str("\\r"),
            0x20..=0x7e => text.push(char::from(byte)),
            _ => text.push_str(&format!("\\{byte:03o}")),
        }
    }

    text.push('"');
    text
}

pub fn invalid_name(name: &[u8]) -> String {
    format!("name is not valid UTF-8: {}", quoted(name))
}

pub fn relative_bytes(from: &str, path: &[u8]) -> Vec<u8> {
    let valid = match std::str::from_utf8(path) {
        Ok(path) => return relative(from, path).into_bytes(),
        Err(error) => error.valid_up_to(),
    };

    let Some(slash) = path[..valid].iter().rposition(|&byte| byte == b'/') else {
        return path.to_vec();
    };

    let directory = match std::str::from_utf8(&path[..slash]) {
        Ok("") | Err(_) => "/",
        Ok(directory) => directory,
    };

    let base = relative(from, directory);
    let name = &path[slash + 1..];
    if base.is_empty() {
        name.to_vec()
    } else {
        [base.as_bytes(), b"/", name].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_names_as_git_does() {
        assert_eq!(quoted(b"bad\xff.ts"), r#""bad\377.ts""#);
        assert_eq!(
            quoted(b"d\xff/caf\xc3\xa9 \"x\"\\\t.ts"),
            r#""d\377/caf\303\251 \"x\"\\\t.ts""#
        );
        assert_eq!(quoted(b"\x01\x7f"), r#""\001\177""#);
    }

    #[test]
    fn relates_names_that_are_not_utf8() {
        assert_eq!(
            relative_bytes("/r", b"/r/d\xff/bad\xff.ts"),
            b"d\xff/bad\xff.ts"
        );
        assert_eq!(relative_bytes("/r/sub", b"/r/d\xff/x.ts"), b"../d\xff/x.ts");
        assert_eq!(relative_bytes("/r", b"/r/sub/x\xff.ts"), b"sub/x\xff.ts");
        assert_eq!(relative_bytes("/", b"/\xff/x.ts"), b"\xff/x.ts");
        assert_eq!(relative_bytes("/r", b"x\xff.ts"), b"x\xff.ts");
        assert_eq!(relative_bytes("/r", b"/r/x.ts"), b"x.ts");
    }

    #[test]
    fn resolves_like_node() {
        assert_eq!(resolve("/a/b", "../c/./d/"), "/a/c/d");
        assert_eq!(resolve("/a", "/x//y/.."), "/x");
    }

    #[test]
    fn relates_like_node() {
        assert_eq!(relative("/a/b", "/a/b"), "");
        assert_eq!(relative("/a/b", "/a/b/c.ts"), "c.ts");
        assert_eq!(relative("/a/bc", "/a/b/x.ts"), "../b/x.ts");
        assert_eq!(relative("/", "/a"), "a");
    }

    #[test]
    fn splits_like_node() {
        assert_eq!(dirname("/a/b.ts"), "/a");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("a.ts"), ".");
        assert_eq!(dirname("//nope"), "/");
        assert_eq!(basename("/a/b.ts/"), "b.ts");
        assert_eq!(extname("x.d.ts"), ".ts");
        assert_eq!(extname(".ts"), "");
    }
}
