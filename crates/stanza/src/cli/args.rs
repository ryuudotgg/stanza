use std::ffi::{OsStr, OsString};
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStrExt;

use super::Program;
use super::usage::did_you_mean;
use crate::engine::types::{Braces, Mode};
use crate::paths::{invalid_name, quoted, shown};

struct Grammar {
    flags: &'static [&'static str],
    operands: bool,
}

const FORMAT: Grammar = Grammar {
    flags: &[
        "--braces",
        "--changed",
        "--check",
        "--fix",
        "--help",
        "--hunks",
        "--json",
        "--no-braces",
        "--staged",
        "--stdin",
        "--version",
        "-h",
    ],
    operands: true,
};

const EXPLAIN: Grammar = Grammar {
    flags: &["--braces", "--no-braces"],
    operands: true,
};

const HOOK: Grammar = Grammar {
    flags: &["--braces", "--hunks", "--no-braces"],
    operands: false,
};

const ALONE: [&str; 3] = ["--help", "-h", "--version"];

const VALUED: [&str; 1] = ["--stdin"];

const EXCLUSIVE: [[&str; 2]; 2] = [["--fix", "--check"], ["--braces", "--no-braces"]];

fn named(arg: &OsStr) -> String {
    arg.to_str()
        .map_or_else(|| quoted(arg.as_bytes()), |text| shown(text).into_owned())
}

fn unknown(arg: &OsStr, flags: &[&str]) -> String {
    let suggestion = arg.to_str().map_or_else(String::new, |text| {
        did_you_mean(text, |flag| flags.contains(&flag))
    });

    format!("unknown flag {}{suggestion}", named(arg))
}

fn path_after<'a>(flag: &str, value: Option<&'a OsStr>) -> Result<&'a str, String> {
    match value.map(|path| path.to_str().ok_or(path)) {
        Some(Ok(path)) if !path.starts_with('-') => Ok(path),
        Some(Err(path)) => Err(invalid_name(path.as_bytes())),
        _ => Err(format!("{flag} needs a path")),
    }
}

struct Scan<'a> {
    given: Vec<&'static str>,
    path: Option<&'a str>,
    operands: Vec<&'a OsStr>,
}

impl Scan<'_> {
    fn has(&self, flag: &str) -> bool {
        self.given.contains(&flag)
    }

    fn braces(&self) -> Option<Braces> {
        if self.has("--braces") {
            Some(Braces::On)
        } else if self.has("--no-braces") {
            Some(Braces::Off)
        } else {
            None
        }
    }
}

fn scan<'a>(args: &'a [OsString], grammar: &Grammar) -> Result<Scan<'a>, String> {
    let mut found = Scan {
        given: Vec::new(),
        path: None,
        operands: Vec::new(),
    };

    let mut queue = args.iter().map(OsString::as_os_str);
    let mut switches = true;
    while let Some(arg) = queue.next() {
        if switches && arg == "--" {
            switches = false;
            continue;
        }

        if !switches || !arg.as_bytes().starts_with(b"-") {
            if !grammar.operands {
                return Err(format!("unexpected argument {}", named(arg)));
            }

            found.operands.push(arg);
            continue;
        }

        let Some(&flag) = grammar.flags.iter().find(|flag| arg == **flag) else {
            return Err(unknown(arg, grammar.flags));
        };

        if ALONE.contains(&flag) {
            return Err(format!("{flag} takes no other arguments"));
        }

        if found.has(flag) {
            return Err(format!("{flag} given twice"));
        }

        let partner = EXCLUSIVE
            .iter()
            .find(|pair| pair.contains(&flag) && pair.iter().any(|other| found.has(other)));

        if let Some([first, second]) = partner {
            return Err(format!("use one of {first} or {second}"));
        }

        if VALUED.contains(&flag) {
            found.path = Some(path_after(flag, queue.next())?);
        }

        found.given.push(flag);
    }

    Ok(found)
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

pub(crate) fn parse_arguments(args: &[OsString]) -> Result<Arguments, String> {
    let scan = scan(args, &FORMAT)?;
    let stdin = scan.path.map(str::to_owned);
    let mut paths = Vec::new();
    let mut invalid_paths = Vec::new();
    for operand in &scan.operands {
        match operand.to_str() {
            Some(path) => paths.push(path.to_owned()),
            None => invalid_paths.push(operand.to_os_string()),
        }
    }

    let changed = scan.has("--changed");
    let hunks = scan.has("--hunks");
    let staged = scan.has("--staged");
    let sources = [changed, staged, !scan.operands.is_empty(), stdin.is_some()]
        .into_iter()
        .filter(|&given| given)
        .count();

    let mode = if scan.has("--fix") {
        Mode::Fix
    } else if scan.has("--check") {
        Mode::Check
    } else {
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
        json: scan.has("--json"),
        braces: scan.braces(),
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

pub(crate) fn parse_explain(args: &[OsString]) -> Result<ExplainArguments, String> {
    let scan = scan(args, &EXPLAIN)?;
    if let [_, second, ..] = scan.operands[..] {
        let extra = scan.operands.iter().copied().find(|arg| *arg == "--");
        return Err(format!(
            "unexpected argument {}",
            named(extra.unwrap_or(second))
        ));
    }

    let target = match scan.operands.first() {
        Some(target) => target
            .to_str()
            .ok_or_else(|| invalid_name(target.as_bytes()))?,
        None => "",
    };

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
        braces: scan.braces(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct HookFlags {
    pub braces: Option<Braces>,
    pub hunks: bool,
}

pub fn hook_flags(args: &[OsString]) -> Result<HookFlags, String> {
    let scan = scan(args, &HOOK)?;
    Ok(HookFlags {
        braces: scan.braces(),
        hunks: scan.has("--hunks"),
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
        Some("hook") => hook_flags(&argv[1..]).map_or_else(usage(Program::Hook), Command::Hook),
        Some("explain") => {
            parse_explain(&argv[1..]).map_or_else(usage(Program::Stanza), Command::Explain)
        }
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

        let parsed = |target: &str| parse_explain(&os(&[target]));
        let huge = parsed("a.ts:99999999999999999999999:2").unwrap();
        assert_eq!(huge.line.get(), usize::MAX);
        assert_eq!(huge.typed_line, "99999999999999999999999");
        assert_eq!(
            parsed("n\nl.ts:0:1").map(|_| ()),
            Err(r#""n\nl.ts:0:1": the line must be 1 or more"#.to_owned())
        );
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn explain_takes_either_braces_switch_once() {
        let braces = |args: &[&str]| parse_explain(&os(args)).map(|parsed| parsed.braces);
        assert_eq!(braces(&["a.ts:1"]), Ok(None));
        assert_eq!(braces(&["--braces", "a.ts:1"]), Ok(Some(Braces::On)));
        assert_eq!(braces(&["--no-braces", "a.ts:1"]), Ok(Some(Braces::Off)));
        assert_eq!(
            braces(&["--braces", "--", "--braces.ts:1"]),
            Ok(Some(Braces::On))
        );
        assert_eq!(
            braces(&["--no-braces", "--braces", "--no-braces", "a.ts:1"]),
            Err("use one of --braces or --no-braces".to_owned())
        );
        assert_eq!(
            braces(&["--brace", "a.ts:1"]),
            Err("unknown flag --brace, did you mean --braces?".to_owned())
        );
        assert_eq!(
            braces(&["--", "--", "a.ts:1"]),
            Err("unexpected argument --".to_owned())
        );
    }

    #[test]
    fn hook_flags_take_no_operands_and_name_a_mistyped_switch() {
        let flags = |args: &[&str]| hook_flags(&os(args));
        assert_eq!(
            flags(&["--hunks", "--x"]),
            Err("unknown flag --x".to_owned())
        );
        assert_eq!(
            flags(&["--hunk"]),
            Err("unknown flag --hunk, did you mean --hunks?".to_owned())
        );
        assert_eq!(
            flags(&["--hu\nks"]),
            Err(r#"unknown flag "--hu\nks", did you mean --hunks?"#.to_owned())
        );
        assert_eq!(
            flags(&["file.ts", "--hunks"]),
            Err("unexpected argument file.ts".to_owned())
        );
        assert_eq!(
            flags(&["--no-braces", "--hunks"]),
            Ok(HookFlags {
                braces: Some(Braces::Off),
                hunks: true
            })
        );
    }

    #[test]
    fn shared_argument_scanner_gives_each_grammar_the_same_problem() {
        let raw = || OsString::from_vec(b"--x\xff".to_vec());
        let cases = [
            (
                os(&["--no-braces", "--no-braces"]),
                "--no-braces given twice",
            ),
            (os(&["--bogus"]), "unknown flag --bogus"),
            (vec![raw()], r#"unknown flag "--x\377""#),
            (
                os(&["--braces", "--no-braces"]),
                "use one of --braces or --no-braces",
            ),
        ];

        for (flags, problem) in cases {
            let format = [os(&["--check"]), flags.clone(), os(&["a.ts"])].concat();
            let explain = [flags.clone(), os(&["a.ts:1"])].concat();
            assert_eq!(parse_arguments(&format).err().as_deref(), Some(problem));
            assert_eq!(parse_explain(&explain).err().as_deref(), Some(problem));
            assert_eq!(hook_flags(&flags).err().as_deref(), Some(problem));
        }

        assert_eq!(
            hook_flags(&os(&["--"])),
            Ok(HookFlags {
                braces: None,
                hunks: false
            })
        );
        assert_eq!(
            hook_flags(&os(&["--", "--hunks"])),
            Err("unexpected argument --hunks".to_owned())
        );

        let first = |problem: Result<(), String>| problem.err();
        assert_eq!(
            first(hook_flags(&os(&["file.ts", "--bogus"])).map(|_| ())),
            Some("unexpected argument file.ts".to_owned())
        );
        assert_eq!(
            first(parse_arguments(&os(&["--check", "--stdin", "--json", "--bogus"])).map(|_| ())),
            Some("--stdin needs a path".to_owned())
        );
        assert_eq!(
            first(parse_arguments(&os(&["--fix", "--fix", "a.ts"])).map(|_| ())),
            Some("--fix given twice".to_owned())
        );

        let Source::Paths { paths, .. } = parse_arguments(&os(&["--check", "--", "-x.ts"]))
            .unwrap()
            .source
        else {
            panic!("not paths");
        };

        assert_eq!(paths, ["-x.ts"]);
        assert_eq!(
            parse_explain(&os(&["--", "-x.ts:1"])).unwrap().path,
            "-x.ts"
        );
    }
}
