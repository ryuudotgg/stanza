use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    Parse,
    Error,
    Write,
    AfterMultiline,
    SwitchClauses,
    EdgeBlank,
    GuardJoin,
    ConsumeJoin,
    UseJoin,
    GuardChain,
    LetStep,
    AfterGuard,
    ShortBody,
    Braces,
    BlockSpacing,
    Wall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Want {
    None,
    AtLeastOne,
}

pub struct Entry {
    pub rule: Rule,
    pub fixable: bool,
    pub gap: Option<Want>,
    pub summary: &'static str,
    pub message: &'static str,
}

const JOIN_MESSAGE: &str =
    "remove the blank line between `{name}` and the `{reader}` that reads it";

pub static RULES: [Entry; 13] = [
    Entry {
        rule: Rule::AfterMultiline,
        fixable: true,
        gap: Some(Want::AtLeastOne),
        summary: "a statement that spans several lines is followed by a blank line",
        message: "add a blank line after the multi-line statement above",
    },
    Entry {
        rule: Rule::SwitchClauses,
        fixable: true,
        gap: Some(Want::AtLeastOne),
        summary: "a blank line goes in between switch clauses and none is ever removed; the gap under a fall through label with an empty body is left as it is",
        message: "add a blank line between these switch clauses",
    },
    Entry {
        rule: Rule::EdgeBlank,
        fixable: true,
        gap: None,
        summary: "a block has no blank line right after `{` or right before `}`",
        message: "remove the blank line right {side}",
    },
    Entry {
        rule: Rule::GuardJoin,
        fixable: true,
        gap: Some(Want::None),
        summary: "a single-line declaration or assignment is followed directly by an `if` that references what it binds, whatever the body size and with or without `else`",
        message: JOIN_MESSAGE,
    },
    Entry {
        rule: Rule::ConsumeJoin,
        fixable: true,
        gap: Some(Want::None),
        summary: "a single-line declaration or assignment is followed directly by a `return` or `switch` that references what it binds",
        message: JOIN_MESSAGE,
    },
    Entry {
        rule: Rule::UseJoin,
        fixable: true,
        gap: Some(Want::None),
        summary: "a single-line declaration is followed directly by a loop, `try` or function declaration that references what it binds",
        message: JOIN_MESSAGE,
    },
    Entry {
        rule: Rule::GuardChain,
        fixable: true,
        gap: Some(Want::None),
        summary: "consecutive compact guards (`if` with a single statement body and no `else`) have no blank line between them",
        message: "remove the blank line between these compact guards",
    },
    Entry {
        rule: Rule::LetStep,
        fixable: true,
        gap: Some(Want::AtLeastOne),
        summary: "a `let` that joins the block below it gets a blank line above it, so it starts its own step",
        message: "add a blank line above this `let`, the block below assigns or uses it",
    },
    Entry {
        rule: Rule::AfterGuard,
        fixable: true,
        gap: Some(Want::AtLeastOne),
        summary: "a single-line guard that returns, throws, continues or breaks is followed by a blank line, unless the next statement is an `if` or a jump (`return`, `throw`, `break`, `continue`)",
        message: "add a blank line after the guard above, a new step starts here",
    },
    Entry {
        rule: Rule::ShortBody,
        fixable: true,
        gap: Some(Want::None),
        summary: "a block of 2 or 3 compact statements has no blank lines, whether it is a function body, an arrow body, a method, a nested block or a switch clause body",
        message: "remove the blank lines between these 2 or 3 compact statements",
    },
    Entry {
        rule: Rule::Braces,
        fixable: true,
        gap: None,
        summary: "a control flow body that is a single statement has no braces",
        message: "remove the braces around this single statement body",
    },
    Entry {
        rule: Rule::BlockSpacing,
        fixable: false,
        gap: None,
        summary: "a multi-line block directly under a statement, when no join rule explains it",
        message: "add a blank line above this block, or join it to the step above",
    },
    Entry {
        rule: Rule::Wall,
        fixable: false,
        gap: None,
        summary: "6 or more consecutive single-line statements with no blank line, reported only where a blank line `--fix` keeps can split the run",
        message: "split these {n} statements into steps with a blank line",
    },
];

fn render(template: &str, args: &[(&str, &str)]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        rest = &rest[open..];

        let value = rest.find('}').and_then(|close| {
            let key = &rest[1..close];
            args.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (close, *value))
        });

        let Some((close, value)) = value else {
            text.push('{');
            rest = &rest[1..];
            continue;
        };

        text.push_str(value);
        rest = &rest[close + 1..];
    }

    text.push_str(rest);
    text
}

impl Rule {
    pub fn id(self) -> &'static str {
        match self {
            Rule::Parse => "parse",
            Rule::Error => "error",
            Rule::Write => "write",
            Rule::AfterMultiline => "after-multiline",
            Rule::SwitchClauses => "switch-clauses",
            Rule::EdgeBlank => "edge-blank",
            Rule::GuardJoin => "guard-join",
            Rule::ConsumeJoin => "consume-join",
            Rule::UseJoin => "use-join",
            Rule::GuardChain => "guard-chain",
            Rule::LetStep => "let-step",
            Rule::AfterGuard => "after-guard",
            Rule::ShortBody => "short-body",
            Rule::Braces => "braces",
            Rule::BlockSpacing => "block-spacing",
            Rule::Wall => "wall",
        }
    }

    pub fn entry(self) -> Option<&'static Entry> {
        let index = match self {
            Rule::Parse | Rule::Error | Rule::Write => return None,
            Rule::AfterMultiline => 0,
            Rule::SwitchClauses => 1,
            Rule::EdgeBlank => 2,
            Rule::GuardJoin => 3,
            Rule::ConsumeJoin => 4,
            Rule::UseJoin => 5,
            Rule::GuardChain => 6,
            Rule::LetStep => 7,
            Rule::AfterGuard => 8,
            Rule::ShortBody => 9,
            Rule::Braces => 10,
            Rule::BlockSpacing => 11,
            Rule::Wall => 12,
        };

        Some(&RULES[index])
    }

    pub fn fixable(self) -> bool {
        self.entry().is_some_and(|entry| entry.fixable)
    }

    pub fn gap(self) -> Option<Want> {
        self.entry().and_then(|entry| entry.gap)
    }

    pub fn message(self, args: &[(&str, &str)]) -> String {
        render(self.entry().map_or("", |entry| entry.message), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_rule_has_one_entry_whose_id_serializes_as_named() {
        for entry in &RULES {
            let serialized = serde_json::to_string(&entry.rule).expect("a rule serializes");
            assert_eq!(serialized, format!("\"{}\"", entry.rule.id()));
            assert!(std::ptr::eq(entry.rule.entry().expect("listed"), entry));
        }

        assert!(Rule::Parse.entry().is_none());
    }

    #[test]
    fn the_catalog_keeps_its_rule_order() {
        let ids: Vec<&str> = RULES.iter().map(|entry| entry.rule.id()).collect();
        assert_eq!(
            ids,
            [
                "after-multiline",
                "switch-clauses",
                "edge-blank",
                "guard-join",
                "consume-join",
                "use-join",
                "guard-chain",
                "let-step",
                "after-guard",
                "short-body",
                "braces",
                "block-spacing",
                "wall",
            ]
        );
    }

    #[test]
    fn every_rule_message_is_an_instruction_in_its_own_words() {
        let join = "remove the blank line between `total` and the `for` that reads it";
        let args = [
            ("name", "total"),
            ("reader", "for"),
            ("side", "after `{`"),
            ("n", "8"),
        ];

        let messages: Vec<(&str, String)> = RULES
            .iter()
            .map(|entry| (entry.rule.id(), entry.rule.message(&args)))
            .collect();

        let expected = [
            (
                "after-multiline",
                "add a blank line after the multi-line statement above",
            ),
            (
                "switch-clauses",
                "add a blank line between these switch clauses",
            ),
            ("edge-blank", "remove the blank line right after `{`"),
            ("guard-join", join),
            ("consume-join", join),
            ("use-join", join),
            (
                "guard-chain",
                "remove the blank line between these compact guards",
            ),
            (
                "let-step",
                "add a blank line above this `let`, the block below assigns or uses it",
            ),
            (
                "after-guard",
                "add a blank line after the guard above, a new step starts here",
            ),
            (
                "short-body",
                "remove the blank lines between these 2 or 3 compact statements",
            ),
            (
                "braces",
                "remove the braces around this single statement body",
            ),
            (
                "block-spacing",
                "add a blank line above this block, or join it to the step above",
            ),
            (
                "wall",
                "split these 8 statements into steps with a blank line",
            ),
        ];

        assert_eq!(
            messages,
            expected.map(|(id, message)| (id, message.to_owned()))
        );
        assert_eq!(
            Rule::EdgeBlank.message(&[("side", "before `}`")]),
            "remove the blank line right before `}`"
        );
    }

    #[test]
    fn summaries_write_counts_as_digits_only_where_a_rule_counts() {
        let counted: Vec<&str> = RULES
            .iter()
            .filter(|entry| entry.summary.contains(char::is_numeric))
            .map(|entry| entry.rule.id())
            .collect();

        assert_eq!(counted, ["short-body", "wall"]);
        assert!(Rule::ShortBody.entry().unwrap().summary.contains("2 or 3"));
        assert!(Rule::Wall.entry().unwrap().summary.contains("6 or more"));
        assert!(
            Rule::GuardChain
                .entry()
                .unwrap()
                .summary
                .contains("a single statement body")
        );
    }

    #[test]
    fn templates_name_only_the_join_side_and_count_placeholders() {
        let args = [
            ("name", "x"),
            ("reader", "if"),
            ("side", "after `{`"),
            ("n", "6"),
        ];
        for entry in &RULES {
            let message = entry.rule.message(&args);
            assert!(!message.contains("{name}") && !message.contains("{reader}"));
            assert!(
                !message.contains("{side}") && !message.contains("{n}"),
                "{message}"
            );
        }
    }

    #[test]
    fn only_gap_rules_and_fixable_rules_carry_a_gap() {
        for entry in &RULES {
            assert!(entry.gap.is_none() || entry.fixable, "{}", entry.rule.id());
        }
    }

    #[test]
    fn placeholders_render_once_from_the_template() {
        assert_eq!(
            Rule::GuardJoin.message(&[("name", "a[\"{reader}\"]"), ("reader", "if")]),
            "remove the blank line between `a[\"{reader}\"]` and the `if` that reads it"
        );

        assert_eq!(
            Rule::EdgeBlank.message(&[("side", "after `{`")]),
            "remove the blank line right after `{`"
        );
    }
}
