# Stanza

An opinionated code formatter for TypeScript and JavaScript. It handles the one thing formatters leave alone, the spacing that makes a function readable, and runs alongside them without fighting. It works from the oxc AST, so nothing outside a function body is ever touched.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

## Why

I miss when code was nice to look at. In the age of AI, code tends to arrive as one block: a forty line function without a single blank line, every statement pressed against the next, the guard for a value three lines away from it. It is correct, and it is painful to read, because the reader has to find the steps themselves.

Writing the rules down in an `AGENTS.md` helps less than you would think. Some agents follow them and some do not, the rules compete with everything else in the brief, and you only find out after the fact. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool. Stanza reads the AST, decides for every gap between two statements whether it wants a blank line, none, or does not care, and edits only empty lines and brace tokens. It is deterministic and idempotent, and it checks seven hundred files in about half a second on a laptop, so a hook can enforce the style at the end of every agent turn and nobody has to read for spacing again. What needs judgment stays a report: a block with no blank line above it, a wall of six statements. Everything else is fixed.

## 👥 Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## License

This project is licensed under the MIT License - see [LICENSE.md](LICENSE.md) for details.
