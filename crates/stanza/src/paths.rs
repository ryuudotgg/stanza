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

#[cfg(test)]
mod tests {
    use super::*;

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
