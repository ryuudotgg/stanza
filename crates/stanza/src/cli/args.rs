use std::collections::HashSet;
use std::ffi::OsString;
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStrExt;

use super::Program;
use crate::engine::types::{Braces, Mode};
use crate::paths::{invalid_name, quoted, shown};
use crate::usage::{BOTH_BRACES, did_you_mean};

pub(crate) fn texts(args: &[OsString]) -> Result<Vec<String>, String> {
    args.iter()
        .map(|arg| {
            arg.to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_name(arg.as_bytes()))
        })
        .collect()
}

pub struct Arguments {
    pub(crate) mode: Mode,
    pub(crate) json: bool,
    pub(crate) braces: Option<Braces>,
    pub(crate) source: Source,
}

pub enum Source {
    Paths {
        paths: Vec<String>,
        invalid: Vec<OsString>,
    },
    Changed {
        hunks: bool,
    },
    Staged {
        hunks: bool,
    },
    Stdin(String),
}

const SWITCHES: [&str; 6] = [
    "--braces",
    "--changed",
    "--hunks",
    "--json",
    "--no-braces",
    "--staged",
];

const STANDALONE: [&str; 3] = ["--help", "-h", "--version"];

pub(crate) fn parse_arguments(args: &[OsString]) -> Result<Arguments, String> {
    let mut paths = Vec::new();
    let mut invalid_paths = Vec::new();
    let mut seen = HashSet::new();
    let mut mode = None;
    let mut stdin = None;
    let mut queue = args.iter();
    while let Some(arg) = queue.next() {
        let Some(arg) = arg.to_str() else {
            if arg.as_bytes().starts_with(b"-") {
                return Err(format!("unknown flag {}", quoted(arg.as_bytes())));
            }

            invalid_paths.push(arg.clone());
            continue;
        };

        if arg == "--" {
            for path in queue.by_ref() {
                match path.to_str() {
                    Some(path) => paths.push(path.to_owned()),
                    None => invalid_paths.push(path.clone()),
                }
            }

            break;
        }

        if arg == "--fix" || arg == "--check" {
            if mode.is_some() {
                return Err("use one of --fix or --check".to_owned());
            }

            mode = Some(if arg == "--fix" {
                Mode::Fix
            } else {
                Mode::Check
            });
            continue;
        }

        if SWITCHES.contains(&arg) {
            if !seen.insert(arg) {
                return Err(format!("{arg} given twice"));
            }

            continue;
        }

        if arg == "--stdin" {
            let path = queue.next();
            if stdin.is_some() {
                return Err("--stdin given twice".to_owned());
            }

            match path.map(|path| path.to_str().ok_or(path)) {
                Some(Ok(path)) if !path.starts_with('-') => stdin = Some(path.to_owned()),
                Some(Err(path)) => return Err(invalid_name(path.as_bytes())),
                _ => return Err("--stdin needs a path".to_owned()),
            }

            continue;
        }

        if STANDALONE.contains(&arg) {
            return Err(format!("{arg} takes no other arguments"));
        }

        if arg.starts_with('-') {
            let suggestion = did_you_mean(arg, |_| true);
            return Err(format!("unknown flag {}{suggestion}", shown(arg)));
        }

        paths.push(arg.to_owned());
    }

    let changed = seen.contains("--changed");
    let hunks = seen.contains("--hunks");
    let staged = seen.contains("--staged");
    let named = !paths.is_empty() || !invalid_paths.is_empty();
    let sources = [changed, staged, named, stdin.is_some()]
        .into_iter()
        .filter(|&given| given)
        .count();

    let Some(mode) = mode else {
        return Err("--fix or --check is required".to_owned());
    };

    if sources == 0 {
        return Err(
            "nothing to format: give paths, --changed, --staged or --stdin <path>".to_owned(),
        );
    }

    if sources > 1 {
        return Err("use only one of --changed, --staged, --stdin or paths".to_owned());
    }

    if staged && mode == Mode::Fix {
        return Err("--staged works only with --check".to_owned());
    }

    if hunks && !changed && !staged {
        return Err("--hunks needs --changed or --staged".to_owned());
    }

    if seen.contains("--braces") && seen.contains("--no-braces") {
        return Err(BOTH_BRACES.to_owned());
    }

    let source = match stdin {
        Some(path) => Source::Stdin(path),
        None if changed => Source::Changed { hunks },
        None if staged => Source::Staged { hunks },
        None => Source::Paths {
            paths,
            invalid: invalid_paths,
        },
    };

    Ok(Arguments {
        mode,
        json: seen.contains("--json"),
        braces: if seen.contains("--braces") {
            Some(Braces::On)
        } else if seen.contains("--no-braces") {
            Some(Braces::Off)
        } else {
            None
        },
        source,
    })
}

pub struct ExplainArguments {
    pub(crate) path: String,
    pub(crate) line: NonZeroUsize,
    pub(crate) typed_line: String,
    pub(crate) braces: Option<Braces>,
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn location(target: &str) -> Option<(&str, &str)> {
    let (rest, last) = target.rsplit_once(':').filter(|(_, last)| digits(last))?;
    let (path, line) = match rest.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() && digits(line) => (path, line),
        _ => (rest, last),
    };

    (!path.is_empty()).then_some((path, line))
}

const EXPLAIN_SWITCHES: [&str; 2] = ["--braces", "--no-braces"];

pub(crate) fn parse_explain(args: &[String]) -> Result<ExplainArguments, String> {
    let (option_args, literal) = match args.iter().position(|arg| arg == "--") {
        Some(delimiter) => (&args[..delimiter], &args[delimiter + 1..]),
        None => (args, &args[..0]),
    };

    let (switches, option_targets): (Vec<&String>, Vec<&String>) = option_args
        .iter()
        .partition(|arg| EXPLAIN_SWITCHES.contains(&arg.as_str()));

    for (index, switch) in switches.iter().enumerate() {
        if switches[..index].contains(switch) {
            return Err(format!("{switch} given twice"));
        }
    }

    if switches.len() > 1 {
        return Err(BOTH_BRACES.to_owned());
    }

    let targets: Vec<&String> = option_targets.iter().copied().chain(literal).collect();
    let extra = (targets.len() > 1).then(|| {
        targets
            .iter()
            .find(|arg| **arg == "--")
            .unwrap_or(&targets[1])
    });

    if let Some(flag) = option_targets.iter().find(|arg| arg.starts_with('-')) {
        let suggestion = did_you_mean(flag, |known| EXPLAIN_SWITCHES.contains(&known));
        return Err(format!("unexpected argument {}{suggestion}", shown(flag)));
    }

    if let Some(extra) = extra {
        return Err(format!("unexpected argument {}", shown(extra)));
    }

    let target = targets.first().map_or("", |target| target.as_str());
    let Some((path, typed_line)) = location(target) else {
        return Err("explain needs <file>:<line>[:<col>]".to_owned());
    };

    let line = typed_line.parse().unwrap_or(usize::MAX);
    let Some(line) = NonZeroUsize::new(line) else {
        return Err(format!("{}: the line must be 1 or more", shown(target)));
    };

    Ok(ExplainArguments {
        path: path.to_owned(),
        line,
        typed_line: typed_line.to_owned(),
        braces: switches.first().map(|switch| {
            if *switch == "--braces" {
                Braces::On
            } else {
                Braces::Off
            }
        }),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct HookFlags {
    pub braces: Option<Braces>,
    pub hunks: bool,
}

const HOOK_SWITCHES: [&str; 3] = ["--braces", "--hunks", "--no-braces"];

pub fn hook_flags(args: &[String]) -> Result<HookFlags, String> {
    let unexpected = args.iter().enumerate().find(|(index, arg)| {
        !HOOK_SWITCHES.contains(&arg.as_str())
            || args.iter().position(|other| other == *arg) != Some(*index)
    });

    if let Some((_, arg)) = unexpected {
        let suggestion = did_you_mean(arg, |flag| HOOK_SWITCHES.contains(&flag));
        return Err(format!("unexpected argument {}{suggestion}", shown(arg)));
    }

    let given = |flag: &str| args.iter().any(|arg| arg == flag);
    if given("--braces") && given("--no-braces") {
        return Err(BOTH_BRACES.to_owned());
    }

    Ok(HookFlags {
        braces: if given("--braces") {
            Some(Braces::On)
        } else if given("--no-braces") {
            Some(Braces::Off)
        } else {
            None
        },
        hunks: given("--hunks"),
    })
}

pub enum Command {
    Help,
    Version,
    HookOff,
    Usage { program: Program, error: String },
    Hook(HookFlags),
    Explain(ExplainArguments),
    Format(Arguments),
}

impl Command {
    pub fn parses_source(&self) -> bool {
        matches!(
            self,
            Command::Hook(_) | Command::Explain(_) | Command::Format(_)
        )
    }

    pub(crate) fn runs(&self) -> Program {
        match self {
            Command::Hook(_) => Program::Hook,
            Command::Help
            | Command::Version
            | Command::HookOff
            | Command::Usage { .. }
            | Command::Explain(_)
            | Command::Format(_) => Program::Stanza,
        }
    }
}

pub fn command(argv: &[OsString], agent_hooks: Option<&str>) -> Command {
    let usage = |program| move |error| Command::Usage { program, error };

    match argv.first().and_then(|arg| arg.to_str()) {
        Some("hook") if agent_hooks == Some("0") => Command::HookOff,
        Some("hook") => texts(&argv[1..])
            .and_then(|args| hook_flags(&args))
            .map_or_else(usage(Program::Hook), Command::Hook),
        Some("explain") => texts(&argv[1..])
            .and_then(|args| parse_explain(&args))
            .map_or_else(usage(Program::Stanza), Command::Explain),
        Some("--help" | "-h") if argv.len() == 1 => Command::Help,
        Some("--version") if argv.len() == 1 => Command::Version,
        _ => parse_arguments(argv).map_or_else(usage(Program::Stanza), Command::Format),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn arguments_that_are_not_utf8_are_kept_apart_from_paths() {
        let bad = || OsString::from_vec(b"bad\xff.ts".to_vec());
        let parse = |args: Vec<OsString>| parse_arguments(&args);

        let listed = |args: Vec<OsString>| match parse(args).unwrap().source {
            Source::Paths { paths, invalid } => (paths, invalid),
            _ => panic!("not paths"),
        };

        assert_eq!(
            listed(vec!["--check".into(), "a.ts".into(), bad()]),
            (vec!["a.ts".to_owned()], vec![bad()])
        );
        assert_eq!(
            listed(vec!["--check".into(), "--".into(), bad()]),
            (Vec::new(), vec![bad()])
        );

        let named = Err(r#"name is not valid UTF-8: "bad\377.ts""#.to_owned());
        assert_eq!(
            parse(vec!["--check".into(), "--stdin".into(), bad()]).map(|_| ()),
            named
        );

        assert_eq!(
            parse(vec!["--check".into(), "--che\nk".into()]).map(|_| ()),
            Err(r#"unknown flag "--che\nk", did you mean --check?"#.to_owned())
        );

        let flag = OsString::from_vec(b"--x\xff".to_vec());
        assert_eq!(
            parse(vec!["--check".into(), flag]).map(|_| ()),
            Err(r#"unknown flag "--x\377""#.to_owned())
        );

        let explained = command(&["explain".into(), bad()], None);
        assert!(matches!(
            explained,
            Command::Usage {
                program: Program::Stanza,
                ..
            }
        ));
    }

    #[test]
    fn explain_locations_read_an_optional_column_and_any_path() {
        assert_eq!(location("a.ts:3"), Some(("a.ts", "3")));
        assert_eq!(location("a.ts:3:0"), Some(("a.ts", "3")));
        assert_eq!(location("a.ts:3:17"), Some(("a.ts", "3")));
        assert_eq!(location("12:34:56"), Some(("12", "34")));
        assert_eq!(location("x:5:3:1"), Some(("x:5", "3")));
        assert_eq!(location(":3:4"), Some((":3", "4")));
        assert_eq!(location("n\nl.ts:4:1"), Some(("n\nl.ts", "4")));
        assert_eq!(location("l\u{2028}s.ts:4"), Some(("l\u{2028}s.ts", "4")));
        assert_eq!(location("c\r.ts:4"), Some(("c\r.ts", "4")));
        assert_eq!(location("a.ts:3:"), None);
        assert_eq!(location("a.ts:3:x"), None);
        assert_eq!(location("a.ts"), None);
        assert_eq!(location(":3"), None);

        let parsed = |target: &str| parse_explain(&[target.to_owned()]);
        let huge = parsed("a.ts:99999999999999999999999:2").unwrap();
        assert_eq!(huge.line.get(), usize::MAX);
        assert_eq!(huge.typed_line, "99999999999999999999999");
        assert_eq!(
            parsed("n\nl.ts:0:1").map(|_| ()),
            Err(r#""n\nl.ts:0:1": the line must be 1 or more"#.to_owned())
        );
    }

    #[test]
    fn explain_takes_either_braces_switch_once() {
        let braces = |args: &[&str]| {
            parse_explain(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
                .map(|parsed| parsed.braces)
        };
        assert_eq!(braces(&["a.ts:1"]), Ok(None));
        assert_eq!(braces(&["--braces", "a.ts:1"]), Ok(Some(Braces::On)));
        assert_eq!(braces(&["--no-braces", "a.ts:1"]), Ok(Some(Braces::Off)));
        assert_eq!(
            braces(&["--braces", "--", "--braces.ts:1"]),
            Ok(Some(Braces::On))
        );
        assert_eq!(
            braces(&["--braces", "--no-braces", "a.ts:1"]),
            Err(BOTH_BRACES.to_owned())
        );
        assert_eq!(
            braces(&["--no-braces", "--braces", "--no-braces", "a.ts:1"]),
            Err("--no-braces given twice".to_owned())
        );
        assert_eq!(
            braces(&["--braces", "--braces", "a.ts:1"]),
            Err("--braces given twice".to_owned())
        );
        assert_eq!(
            braces(&["--brace", "a.ts:1"]),
            Err("unexpected argument --brace, did you mean --braces?".to_owned())
        );
    }

    #[test]
    fn hook_flags_reject_unknown_and_repeated_switches_before_both_braces() {
        let flags = |args: &[&str]| {
            hook_flags(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            flags(&["--hunks", "--x"]),
            Err("unexpected argument --x".to_owned())
        );
        assert_eq!(
            flags(&["--hunk"]),
            Err("unexpected argument --hunk, did you mean --hunks?".to_owned())
        );
        assert_eq!(
            flags(&["--hu\nks"]),
            Err(r#"unexpected argument "--hu\nks", did you mean --hunks?"#.to_owned())
        );
        assert_eq!(
            flags(&["--braces", "--no-braces", "--braces"]),
            Err("unexpected argument --braces".to_owned())
        );
        assert_eq!(
            flags(&["--braces", "--no-braces"]),
            Err(BOTH_BRACES.to_owned())
        );
        assert_eq!(
            flags(&["--no-braces", "--hunks"]),
            Ok(HookFlags {
                braces: Some(Braces::Off),
                hunks: true
            })
        );
    }
}
