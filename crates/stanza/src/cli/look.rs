use std::ffi::OsString;
use std::fmt::{self, Display};

pub const AGENT_VARIABLES: [&str; 6] = [
    "CLAUDECODE",
    "AI_AGENT",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "CURSOR_AGENT",
    "GEMINI_CLI",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Pipe,
    Terminal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Off,
    On,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub layout: Layout,
    pub stdout: Color,
    pub stderr: Color,
}

impl Look {
    pub fn of(env: impl Fn(&str) -> Option<OsString>, stdout_tty: bool, stderr_tty: bool) -> Self {
        let non_empty = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        let agent = AGENT_VARIABLES.iter().any(|name| non_empty(name));
        let off = agent || non_empty("NO_COLOR") || env("TERM").is_some_and(|term| term == "dumb");
        let forced = env("FORCE_COLOR").is_some_and(|value| !value.is_empty() && value != "0");
        let color = |tty: bool| {
            if !off && (tty || forced) {
                Color::On
            } else {
                Color::Off
            }
        };

        Look {
            layout: if stdout_tty && !agent {
                Layout::Terminal
            } else {
                Layout::Pipe
            },
            stdout: color(stdout_tty),
            stderr: color(stderr_tty),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Style {
    Bold,
    Dim,
    Yellow,
    BoldRed,
    BoldYellow,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Style::Bold => "1",
            Style::Dim => "2",
            Style::Yellow => "33",
            Style::BoldRed => "1;31",
            Style::BoldYellow => "1;33",
        }
    }
}

pub struct Painted<T> {
    color: Color,
    style: Style,
    text: T,
}

impl<T: Display> Display for Painted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self.color {
            Color::Off => write!(formatter, "{}", self.text),
            Color::On => write!(formatter, "\x1b[{}m{}\x1b[0m", self.style.code(), self.text),
        }
    }
}

impl Color {
    pub fn paint<T: Display>(self, style: Style, text: T) -> Painted<T> {
        Painted {
            color: self,
            style,
            text,
        }
    }

    pub fn numbers(self, line: &str) -> String {
        if self == Color::Off {
            return line.to_owned();
        }

        let mut painted = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(start) = rest.find(|char: char| char.is_ascii_digit()) {
            let end = rest[start..]
                .find(|char: char| !char.is_ascii_digit())
                .map_or(rest.len(), |length| start + length);

            painted.push_str(&rest[..start]);
            painted.push_str(&self.paint(Style::Bold, &rest[start..end]).to_string());
            rest = &rest[end..];
        }

        painted.push_str(rest);
        painted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Vars<'a> = &'a [(&'a str, &'a str)];

    fn look(vars: Vars, stdout_tty: bool, stderr_tty: bool) -> Look {
        let env = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };

        Look::of(env, stdout_tty, stderr_tty)
    }

    #[test]
    fn a_terminal_takes_the_terminal_layout_and_color_on_each_terminal_stream() {
        use Color::{Off, On};
        use Layout::{Pipe, Terminal};

        let seen = |layout, stdout, stderr| Look {
            layout,
            stdout,
            stderr,
        };

        let cases: [(Vars, [bool; 2], Look); 9] = [
            (&[], [true, true], seen(Terminal, On, On)),
            (&[], [true, false], seen(Terminal, On, Off)),
            (&[], [false, true], seen(Pipe, Off, On)),
            (&[], [false, false], seen(Pipe, Off, Off)),
            (&[("NO_COLOR", "1")], [true, true], seen(Terminal, Off, Off)),
            (&[("NO_COLOR", "")], [true, true], seen(Terminal, On, On)),
            (&[("TERM", "dumb")], [true, true], seen(Terminal, Off, Off)),
            (
                &[("CLAUDECODE", "1"), ("FORCE_COLOR", "1")],
                [true, true],
                seen(Pipe, Off, Off),
            ),
            (&[("GEMINI_CLI", "")], [true, true], seen(Terminal, On, On)),
        ];

        for (vars, [stdout_tty, stderr_tty], expected) in cases {
            assert_eq!(look(vars, stdout_tty, stderr_tty), expected, "{vars:?}");
        }
    }

    #[test]
    fn force_color_colors_a_pipe_unless_empty_zero_or_switched_off() {
        let piped = |vars: Vars| {
            let look = look(vars, false, false);
            assert_eq!(look.layout, Layout::Pipe);
            assert_eq!(look.stdout, look.stderr);
            look.stdout
        };

        assert_eq!(piped(&[("FORCE_COLOR", "1")]), Color::On);
        assert_eq!(piped(&[("FORCE_COLOR", "3")]), Color::On);
        assert_eq!(piped(&[("FORCE_COLOR", "0")]), Color::Off);
        assert_eq!(piped(&[("FORCE_COLOR", "")]), Color::Off);
        assert_eq!(
            piped(&[("FORCE_COLOR", "1"), ("NO_COLOR", "1")]),
            Color::Off
        );
        assert_eq!(piped(&[("FORCE_COLOR", "1"), ("TERM", "dumb")]), Color::Off);

        for name in AGENT_VARIABLES {
            assert_eq!(
                piped(&[("FORCE_COLOR", "1"), (name, "1")]),
                Color::Off,
                "{name}"
            );
        }
    }

    #[test]
    fn painting_wraps_text_only_when_color_is_on() {
        assert_eq!(
            Color::Off.paint(Style::BoldRed, "stanza:").to_string(),
            "stanza:"
        );
        assert_eq!(
            Color::On.paint(Style::BoldRed, "stanza:").to_string(),
            "\x1b[1;31mstanza:\x1b[0m"
        );
        assert_eq!(
            Color::On.numbers("4 findings in 12 files: 2 fixable with --fix"),
            "\x1b[1m4\x1b[0m findings in \x1b[1m12\x1b[0m files: \x1b[1m2\x1b[0m fixable with --fix"
        );
        assert_eq!(
            Color::On.numbers("no changed files to check"),
            "no changed files to check"
        );
        assert_eq!(Color::Off.numbers("checked 3 files"), "checked 3 files");
    }
}
