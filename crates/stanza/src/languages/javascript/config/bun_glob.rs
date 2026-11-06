// Ported from Bun 1.4.0's src/glob/matcher.rs; its MIT notice, which the licence requires to ship with this copy, is in bun_glob.LICENSE.

const BRACE_DEPTH: usize = 10;
const BRACE_BRANCH_BUDGET: u32 = 10_000;

#[derive(Clone, Copy)]
struct Brace {
    open: usize,
    branch: usize,
    close: usize,
}

#[derive(Clone, Copy, Default)]
struct Wildcard {
    glob_index: usize,
    path_index: usize,
    brace_depth: usize,
}

#[derive(Clone, Copy, Default)]
struct State {
    path_index: usize,
    glob_index: usize,
    wildcard: Wildcard,
    globstar: Wildcard,
    brace_depth: usize,
}

struct Search<'a> {
    glob: &'a [u8],
    path: &'a [u8],
    braces: Vec<Brace>,
    budget: u32,
}

impl State {
    fn backtrack(&mut self) {
        self.path_index = self.wildcard.path_index;
        self.glob_index = self.wildcard.glob_index;
        self.brace_depth = self.wildcard.brace_depth;
    }

    fn skip_to_separator(&mut self, path: &[u8], is_end_invalid: bool) {
        if self.path_index == path.len() {
            self.wildcard.path_index += 1;
            return;
        }

        let mut path_index = self.path_index;
        while path_index < path.len() && path[path_index] != b'/' {
            path_index += 1;
        }

        if is_end_invalid || path_index != path.len() {
            path_index += 1;
        }

        self.wildcard.path_index = path_index;
        self.globstar = self.wildcard;
    }
}

const fn sequence_length(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn rune_at(bytes: &[u8], index: usize) -> (u32, usize) {
    let length = sequence_length(bytes[index]);
    let point = bytes
        .get(index..index + length)
        .and_then(|slice| std::str::from_utf8(slice).ok())
        .and_then(|text| text.chars().next())
        .map_or(0xfffd, u32::from);

    (point, length)
}

fn unescape(char: &mut u8, glob: &[u8], glob_index: &mut usize) -> bool {
    if *char == b'\\' {
        *glob_index += 1;
        if *glob_index >= glob.len() {
            return false;
        }

        *char = escaped(glob[*glob_index]);
    }

    true
}

const fn escaped(char: u8) -> u8 {
    match char {
        b'b' => b'\x08',
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        other => other,
    }
}

fn class_char(glob: &[u8], glob_index: &mut usize) -> Option<(u32, usize)> {
    match glob[*glob_index] {
        b'\\' => {
            *glob_index += 1;
            if *glob_index >= glob.len() {
                return None;
            }

            let next = glob[*glob_index];
            Some(if matches!(next, b'a' | b'b' | b'n' | b'r' | b't') {
                (u32::from(escaped(next)), 1)
            } else {
                rune_at(glob, *glob_index)
            })
        }
        char @ 0..=0x7f => Some((u32::from(char), 1)),
        _ => Some(rune_at(glob, *glob_index)),
    }
}

fn skip_globstars(glob: &[u8], glob_index: &mut usize) {
    *glob_index += 2;
    while *glob_index + 4 <= glob.len() && &glob[*glob_index..*glob_index + 4] == b"/**/" {
        *glob_index += 3;
    }

    if *glob_index + 3 == glob.len() && &glob[*glob_index..*glob_index + 3] == b"/**" {
        *glob_index += 3;
    }

    *glob_index -= 2;
}

fn brace_end(glob: &[u8], open: usize) -> usize {
    let mut index = open;
    let mut depth = 0usize;
    let mut in_brackets = false;
    while index < glob.len() {
        match glob[index] {
            b'{' if !in_brackets => depth += 1,
            b'}' if !in_brackets => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            b'[' if !in_brackets => in_brackets = true,
            b']' => in_brackets = false,
            b'\\' => index += 1,
            _ => {}
        }

        index += 1;
    }

    glob.len()
}

enum Step {
    Advance,
    Literal,
    Miss,
    Fail,
}

impl Search<'_> {
    fn star(&self, state: &mut State, glob_start: usize) {
        let (glob, path) = (self.glob, self.path);
        let is_globstar = state.glob_index + 1 < glob.len() && glob[state.glob_index + 1] == b'*';
        if is_globstar {
            skip_globstars(glob, &mut state.glob_index);
        }

        state.wildcard.glob_index = state.glob_index;
        state.wildcard.path_index = state.path_index
            + if state.path_index < path.len() {
                sequence_length(path[state.path_index])
            } else {
                1
            };

        state.wildcard.brace_depth = state.brace_depth;

        let mut in_globstar = false;
        if is_globstar {
            state.glob_index += 2;

            let is_end_invalid = state.glob_index < glob.len();
            if is_end_invalid
                && state.path_index == path.len()
                && glob.len() - state.glob_index == 2
                && glob[state.glob_index] == b'/'
                && glob[state.glob_index + 1] == b'*'
            {
                return;
            }

            if (state.glob_index.saturating_sub(glob_start) < 3
                || glob[state.glob_index - 3] == b'/')
                && (!is_end_invalid || glob[state.glob_index] == b'/')
            {
                if is_end_invalid {
                    state.glob_index += 1;
                }

                state.skip_to_separator(path, is_end_invalid);
                in_globstar = true;
            }
        } else {
            state.glob_index += 1;
        }

        if !in_globstar && state.path_index < path.len() && path[state.path_index] == b'/' {
            state.wildcard = state.globstar;
        }
    }

    fn class(&self, state: &mut State) -> Step {
        let (glob, path) = (self.glob, self.path);
        state.glob_index += 1;

        let mut negated = false;
        if state.glob_index < glob.len() && matches!(glob[state.glob_index], b'^' | b'!') {
            negated = true;
            state.glob_index += 1;
        }

        let mut first = true;
        let mut is_match = false;
        let (char, length) = rune_at(path, state.path_index);
        while state.glob_index < glob.len() && (first || glob[state.glob_index] != b']') {
            let Some((low, low_length)) = class_char(glob, &mut state.glob_index) else {
                return Step::Fail;
            };

            state.glob_index += low_length;

            let high = if state.glob_index + 1 < glob.len()
                && glob[state.glob_index] == b'-'
                && glob[state.glob_index + 1] != b']'
            {
                state.glob_index += 1;
                let Some((high, high_length)) = class_char(glob, &mut state.glob_index) else {
                    return Step::Fail;
                };

                state.glob_index += high_length;
                high
            } else {
                low
            };

            if low <= char && char <= high {
                is_match = true;
            }

            first = false;
        }

        if state.glob_index >= glob.len() {
            return Step::Fail;
        }

        state.glob_index += 1;
        if is_match != negated {
            state.path_index += length;
            return Step::Advance;
        }

        Step::Miss
    }

    fn literal(&self, state: &mut State, char: u8) -> Option<bool> {
        let (glob, path) = (self.glob, self.path);
        let mut char = char;
        if !unescape(&mut char, glob, &mut state.glob_index) {
            return None;
        }

        let length = sequence_length(char);
        let (path_index, glob_index) = (state.path_index, state.glob_index);
        let is_match = if char == b'/' {
            path[path_index] == b'/'
        } else if length > 1 {
            path_index + length <= path.len()
                && glob_index + length <= glob.len()
                && path[path_index..path_index + length] == glob[glob_index..glob_index + length]
        } else {
            path[path_index] == char
        };

        if !is_match {
            return Some(false);
        }

        state.glob_index += length;
        state.path_index += length;
        if char == b'/' {
            state.wildcard = state.globstar;
        }

        Some(true)
    }

    fn run(&mut self, state: &mut State, glob_start: usize) -> bool {
        let (glob, path) = (self.glob, self.path);
        while state.glob_index < glob.len() || state.path_index < path.len() {
            if state.glob_index < glob.len() {
                let char = glob[state.glob_index];
                let step = match char {
                    b'*' => {
                        self.star(state, glob_start);
                        continue;
                    }
                    b'?' if state.path_index < path.len() => {
                        if path[state.path_index] != b'/' {
                            state.glob_index += 1;
                            state.path_index += sequence_length(path[state.path_index]);
                            continue;
                        }

                        Step::Miss
                    }
                    b'?' => Step::Literal,
                    b'[' if state.path_index < path.len() => self.class(state),
                    b'[' => Step::Literal,
                    b'{' => {
                        if let Some(brace) = self
                            .braces
                            .iter()
                            .find(|brace| brace.open == state.glob_index)
                        {
                            state.glob_index = brace.branch;
                            state.brace_depth += 1;
                            continue;
                        }

                        return self.brace(state);
                    }
                    b',' | b'}' if state.brace_depth > 0 && self.skip_branch(state) => continue,
                    _ => Step::Literal,
                };

                match step {
                    Step::Fail => return false,
                    Step::Advance => continue,
                    Step::Miss => {}
                    Step::Literal if state.path_index < path.len() => {
                        match self.literal(state, char) {
                            None => return false,
                            Some(true) => continue,
                            Some(false) => {}
                        }
                    }
                    Step::Literal => {}
                }
            }

            if state.wildcard.path_index > 0 && state.wildcard.path_index <= path.len() {
                state.backtrack();
                continue;
            }

            return false;
        }

        true
    }

    fn brace(&mut self, state: &mut State) -> bool {
        let glob = self.glob;
        let mut depth = 0i32;
        let mut in_brackets = false;
        let open = state.glob_index;
        let close = brace_end(glob, open);
        let mut branch = 0;
        while state.glob_index < glob.len() {
            match glob[state.glob_index] {
                b'{' if !in_brackets => {
                    depth += 1;
                    if depth == 1 {
                        branch = state.glob_index + 1;
                    }
                }
                b'}' if !in_brackets => {
                    depth -= 1;
                    if depth == 0 {
                        return self.branch(
                            state,
                            Brace {
                                open,
                                branch,
                                close,
                            },
                        );
                    }
                }
                b',' if depth == 1 && !in_brackets => {
                    if self.branch(
                        state,
                        Brace {
                            open,
                            branch,
                            close,
                        },
                    ) {
                        return true;
                    }

                    branch = state.glob_index + 1;
                }
                b'[' => in_brackets = true,
                b']' => in_brackets = false,
                b'\\' => state.glob_index += 1,
                _ => {}
            }

            state.glob_index += 1;
        }

        false
    }

    fn branch(&mut self, state: &State, brace: Brace) -> bool {
        if self.budget == 0 || self.braces.len() == BRACE_DEPTH {
            return false;
        }

        self.budget -= 1;
        self.braces.push(brace);

        let mut branch = *state;
        branch.glob_index = brace.branch;
        branch.brace_depth = self.braces.len();

        let matched = self.run(&mut branch, brace.branch);
        self.braces.pop();
        matched
    }

    fn skip_branch(&self, state: &mut State) -> bool {
        let at = state.glob_index;
        let Some(frame) = self
            .braces
            .iter()
            .rev()
            .find(|frame| frame.open < at && at <= frame.close)
        else {
            return false;
        };

        if frame.close < self.glob.len() {
            state.glob_index = frame.close + 1;
            state.brace_depth -= 1;
        } else {
            state.glob_index = frame.close;
        }

        true
    }
}

pub fn bun_match(glob: &str, path: &str) -> bool {
    let glob = glob.as_bytes();
    let mut state = State::default();
    let mut negated = false;
    while state.glob_index < glob.len() && glob[state.glob_index] == b'!' {
        negated = !negated;
        state.glob_index += 1;
    }

    let mut search = Search {
        glob,
        path: path.as_bytes(),
        braces: Vec::new(),
        budget: BRACE_BRANCH_BUDGET,
    };

    let start = state.glob_index;
    search.run(&mut state, start) != negated
}

#[cfg(test)]
mod tests {
    use super::bun_match;

    #[test]
    fn matches_bun_glob() {
        let rows: Vec<(String, String, bool)> =
            serde_json::from_str(include_str!("bun-glob.json")).expect("the table is JSON");

        let wrong: Vec<String> = rows
            .iter()
            .filter(|(pattern, path, expected)| bun_match(pattern, path) != *expected)
            .map(|(pattern, path, expected)| format!("{pattern:?} {path:?} expected {expected}"))
            .collect();

        assert!(
            wrong.is_empty(),
            "{} of {} rows differ:\n{}",
            wrong.len(),
            rows.len(),
            wrong.join("\n")
        );
    }
}
