pub const VERSION: &str = env!("STANZA_VERSION");

pub const COMMIT: Option<&str> = option_env!("STANZA_COMMIT");

pub fn version_line(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("stanza {version} ({commit})\n"),
        None => format!("stanza {version}\n"),
    }
}

pub fn run(args: &[String]) -> (String, String, i32) {
    match args {
        [only] if only == "--version" => (version_line(VERSION, COMMIT), String::new(), 0),
        _ => (
            String::new(),
            "stanza: this build only supports --version\n".to_owned(),
            2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_without_commit() {
        assert_eq!(version_line("1.2.3", None), "stanza 1.2.3\n");
    }

    #[test]
    fn version_line_with_commit() {
        assert_eq!(
            version_line("1.2.3", Some("abc1234")),
            "stanza 1.2.3 (abc1234)\n"
        );
    }

    #[test]
    fn version_matches_package_json() {
        let text = include_str!("../../../package.json");
        assert!(text.contains(&format!("\"version\": \"{VERSION}\"")));
    }

    #[test]
    fn refuses_everything_but_version() {
        let (stdout, stderr, code) = run(&["--check".to_owned()]);
        assert_eq!((stdout.as_str(), code), ("", 2));
        assert!(!stderr.is_empty());
    }
}
