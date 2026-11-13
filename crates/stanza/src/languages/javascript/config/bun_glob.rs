// Ported from Bun 1.4.4's src/glob/matcher.rs; its MIT notice, which the licence requires to ship with this copy, is in bun_glob.LICENSE.

pub fn bun_match(glob: &str, path: &str) -> bool {
    match_from_first_byte(glob.as_bytes(), path.as_bytes())
}

#[derive(Copy, Clone)]
struct State {
    path_index: u32,
    glob_index: u32,

    wildcard: Wildcard,
    globstar: Wildcard,

    seen: Seen,
    no_class_from: u32,
    not_group_at: u32,
    outcome: Outcome,
}

impl State {
    #[inline(always)]
    fn at(glob_index: u32, path_index: u32, seen: Seen) -> State {
        State {
            path_index,
            glob_index,
            wildcard: Wildcard::default(),
            globstar: Wildcard::default(),
            seen,
            no_class_from: u32::MAX,
            not_group_at: u32::MAX,
            outcome: Outcome::No,
        }
    }

    #[inline(always)]
    fn goes_back(&mut self, path: &[u8]) -> bool {
        let can = self.wildcard.path_index > 0 && self.wildcard.path_index as usize <= path.len();
        if can {
            self.backtrack();
        }

        can
    }

    #[inline(always)]
    fn backtrack(&mut self) {
        self.path_index = self.wildcard.path_index;
        self.glob_index = self.wildcard.glob_index;
    }

    #[inline(always)]
    fn skip_to_separator(&mut self, path: &[u8], is_end_invalid: bool) {
        if self.path_index as usize == path.len() {
            self.wildcard.path_index += 1;
            self.globstar = self.wildcard;
            return;
        }

        let mut path_index = self.path_index;
        while (path_index as usize) < path.len() && !is_separator(path[path_index as usize]) {
            path_index += 1;
        }

        if is_end_invalid || path_index as usize != path.len() {
            path_index += 1;
        }

        self.wildcard.path_index = path_index;
        self.globstar = self.wildcard;
    }
}

#[derive(Copy, Clone, Default)]
struct Wildcard {
    glob_index: u32,
    path_index: u32,
}

#[inline(always)]
fn match_from_first_byte(glob: &[u8], path: &[u8]) -> bool {
    let Some(&first) = glob.first() else {
        return path.is_empty();
    };

    let is_plain = IS_PLAIN[first as usize];
    if is_plain && path.first() != Some(&first) {
        return false;
    }

    let differs_at_once = || {
        first == b'\\'
            && glob.get(1).is_some_and(|it| {
                path.first() != Some(it)
                    && !matches!(it, b'b' | b'n' | b'r' | b't' | b'/' | b'\\' | 0x80..)
            })
    };

    match first {
        _ if is_plain => match_literal(glob, path),
        b'\\' if differs_at_once() => false,
        b'*' => match glob.get(1) {
            Some(b'*') if is_globstar_before_text(glob, 0, 0) => {
                match_after_globstar(glob, path, Start::of())
            }
            Some(b'?' | b'[') if is_last_star(glob, 0) => match_last_star(glob, path, Start::of()),
            _ => match_more(glob, path, Start::of(), None),
        },
        b'!' => match_negated(glob, path),
        b'\\' | b'?' | b'[' | 0x80.. => match_escapes_at_start(glob, path),
        b'{' => match_choices(glob, path, Start::of()),
        _ => match_more(glob, path, Start::of(), None),
    }
}

#[derive(Copy, Clone)]
struct Start {
    glob_start: u32,
    glob_index: u32,
    path_index: u32,
    seen: Seen,
}

impl Start {
    #[inline(always)]
    fn of() -> Start {
        Start {
            glob_start: 0,
            glob_index: 0,
            path_index: 0,
            seen: Seen::OnlyRegular,
        }
    }
}

#[inline(never)]
fn match_literal(glob: &[u8], path: &[u8]) -> bool {
    let mut taken = 0;
    while let Some(&wanted) = glob.get(taken) {
        let is_same = match path.get(taken) {
            Some(&found) if IS_PLAIN[wanted as usize] => found == wanted,
            Some(&found) if wanted == b'/' => is_separator(found),
            None if IS_PLAIN[wanted as usize] || wanted == b'/' => false,
            _ => break,
        };

        if !is_same {
            return false;
        }

        taken += 1;
    }

    if taken == glob.len() {
        return taken == path.len();
    }

    let start = Start {
        glob_index: taken as u32,
        path_index: taken as u32,
        ..Start::of()
    };

    if is_globstar_before_text(glob, 0, taken) {
        return match_after_globstar(glob, path, start);
    }

    if glob[taken] == b'{' {
        return match_choices(glob, path, start);
    }

    if is_last_star(glob, taken) {
        return match_last_star(glob, path, start);
    }

    match matches!(glob[taken], b'\\' | b'?' | b'[' | 0x80..) {
        true => match_escapes(glob, path, start),
        false => match_more(glob, path, start, None),
    }
}

#[inline(always)]
fn is_globstar_before_text(glob: &[u8], glob_start: usize, index: usize) -> bool {
    matches!(glob[index..], [b'*', b'*', b'/', after, ..] if IS_TEXT[after as usize] || after == b'{')
        && (index == glob_start || glob[index - 1] == b'/')
}

#[inline(always)]
fn plain_segments(tail: &[u8]) -> (usize, usize, bool) {
    let (mut end, mut at, mut has_group) = (0, 0, false);
    loop {
        if tail.get(at) == Some(&b'{') {
            let mut commas = 0;
            loop {
                let first = at + 1;
                at = first;
                while tail.get(at).is_some_and(|it| IS_TEXT[*it as usize]) {
                    at += 1;
                }

                match tail.get(at) {
                    Some(b',') if at > first => commas += 1,
                    Some(b'}') if at > first && commas > 0 => break,
                    _ => return (end, end, has_group),
                }
            }

            at += 1;
            if tail.get(at) != Some(&b'/') {
                return (end, end, has_group);
            }

            has_group = true;
        } else {
            let first = at;
            while tail.get(at).is_some_and(|it| IS_TEXT[*it as usize]) {
                at += 1;
            }

            if at == first || tail.get(at) != Some(&b'/') {
                return (end, at, has_group);
            }
        }

        at += 1;
        end = at;
    }
}

#[inline(always)]
fn take_plain_segments(wanted: &[u8], text: &[u8]) -> Option<usize> {
    let (mut at, mut taken) = (0, 0);
    while let Some(&ch) = wanted.get(at) {
        if ch != b'{' {
            let found = *text.get(taken)?;
            if found != ch && !(ch == b'/' && is_separator(found)) {
                return None;
            }

            at += 1;
            taken += 1;
            continue;
        }

        loop {
            at += 1;
            let mut len = 0;
            while IS_TEXT[wanted[at] as usize] && text.get(taken + len) == Some(&wanted[at]) {
                at += 1;
                len += 1;
            }

            let fits = !IS_TEXT[wanted[at] as usize]
                && text.get(taken + len).is_some_and(|it| is_separator(*it));
            while fits && wanted[at] != b'}' || IS_TEXT[wanted[at] as usize] {
                at += 1;
            }

            if fits {
                taken += len;
                break;
            }

            if wanted[at] == b'}' {
                return None;
            }
        }

        at += 1;
    }

    Some(taken)
}

#[inline(never)]
fn match_after_globstar(glob: &[u8], path: &[u8], mut start: Start) -> bool {
    #[cfg(debug_assertions)]
    let expected = match_more(glob, path, start, None);
    let matched = loop {
        let tail = &glob[start.glob_index as usize + 3..];
        let rest = &path[start.path_index as usize..];
        let (segments, literal, has_group) = plain_segments(tail);
        if literal == tail.len() && literal > segments && !has_group {
            let before = rest.len().checked_sub(literal);
            break before.is_some_and(|before| {
                take_plain_segments(tail, &rest[before..]).is_some()
                    && (before == 0 || is_separator(rest[before - 1]))
            });
        }

        if segments == 0 || !matches!(tail[segments..], [b'*', b'*'] | [b'*', b'*', b'/', ..]) {
            return match_more(glob, path, start, None);
        }

        let mut from = 0;
        let taken = loop {
            if let Some(taken) = take_plain_segments(&tail[..segments], &rest[from..]) {
                break Some(from + taken);
            }

            while let Some(&found) = rest.get(from)
                && !is_separator(found)
            {
                from += 1;
            }

            if from == rest.len() {
                break None;
            }

            from += 1;
        };

        let Some(taken) = taken else {
            break false;
        };

        if tail.len() == segments + 2 {
            break true;
        }

        start.glob_index += 3 + segments as u32;
        start.path_index += taken as u32;
        if has_group {
            start.seen = Seen::Choice;
        }
    };

    let negated = start.glob_start % 2 == 1;
    let result = matched != negated;

    #[cfg(debug_assertions)]
    debug_assert!(result == expected);
    result
}

#[inline(always)]
fn is_last_star(glob: &[u8], index: usize) -> bool {
    match glob[index..] {
        [b'*'] => true,
        [b'*', b'?' | b'[', ..] => !matches!(glob[glob.len() - 1], b'}' | b')' | b'*'),
        _ => false,
    }
}

#[inline(never)]
fn match_last_star(glob: &[u8], path: &[u8], mut start: Start) -> bool {
    #[cfg(debug_assertions)]
    let expected = match_more(glob, path, start, None);
    let mut at = start.glob_index as usize + 1;
    let mut width = 0;
    let mut is_fixed = start.seen == Seen::OnlyRegular;
    while is_fixed && let Some(&ch) = glob.get(at) {
        at = match ch {
            _ if IS_PLAIN[ch as usize] => at + 1,
            b'?' if glob.get(at + 1) != Some(&b'(') => at + 1,
            b'\\' if glob.get(at + 1).is_some_and(|it| *it < 128 && *it != b'/') => at + 2,
            b'[' if let Some((_, close, _)) = ascii_class::<false>(glob, at, 0) => close + 1,
            _ => {
                is_fixed = false;
                at
            }
        };

        width += 1;
    }

    let rest = &path[start.path_index as usize..];
    let before = rest.len().saturating_sub(width);
    if !is_fixed || !rest[before..].is_ascii() {
        return match_more(glob, path, start, None);
    }

    let fits = rest.len() >= width && !rest[..before].iter().any(|it| is_separator(*it));
    let result = if !fits || width == 0 {
        let negated = start.glob_start % 2 == 1;
        fits != negated
    } else {
        start.glob_index += 1;
        start.path_index += before as u32;
        match_escapes(glob, path, start)
    };

    #[cfg(debug_assertions)]
    debug_assert!(result == expected);
    result
}

#[inline(never)]
fn match_branches(glob: &[u8], path: &[u8], start: &Start) -> Option<bool> {
    let open = start.glob_index as usize;
    let starts_segment = open == start.glob_start as usize || glob[open - 1] == b'/';
    let mut ends = [0; 4];
    let (mut count, mut close) = (0, 0);
    let mut ends_with_globstar = false;
    let mut at = open + 1;
    while let Some(&ch) = glob.get(at) {
        if !IS_TEXT[ch as usize] && ch != b'/' {
            let is_inside = close == 0;
            match ch {
                b',' | b'}' if is_inside => {
                    *ends.get_mut(count)? = at;
                    count += 1;
                    if ch == b'}' {
                        close = at;
                    }
                }
                b'*' if glob.get(at + 1) == Some(&b'*') => {
                    let before = glob[at - 1];
                    let is_first = is_inside && (at - 1 == open || before == b',');
                    if before != b'/' && !(is_first && starts_segment) {
                        return None;
                    }

                    at += 1;
                    match glob.get(at + 1) {
                        None | Some(b'/') => {}
                        Some(b',' | b'}') if is_inside => ends_with_globstar = true,
                        _ => return None,
                    }
                }
                b'*' => {}
                _ => return None,
            }
        }

        at += 1;
    }

    let after = &glob[close + 1..];
    if count < 2
        || close == 0
        || after.first() == Some(&b'*')
        || ends_with_globstar && !matches!(after.first(), None | Some(b'/'))
    {
        return None;
    }

    let rest = &path[start.path_index as usize..];
    let mut written = [0; 64];
    let mut from = open + 1;
    let mut matched = false;
    for &to in &ends[..count] {
        let alternative = &glob[from..to];
        from = to + 1;
        if let Some(first) = alternative.first()
            && IS_PLAIN[*first as usize]
            && rest.first() != Some(first)
        {
            continue;
        }

        let mut pattern = alternative;
        if !after.is_empty() {
            let it = written.get_mut(..alternative.len() + after.len())?;
            for (to, from) in std::iter::zip(&mut *it, alternative.iter().chain(after)) {
                *to = *from;
            }

            pattern = it;
        }

        if pattern.ends_with(b"**/*") {
            return None;
        }

        if match_from_first_byte(pattern, rest) {
            matched = true;
            break;
        }
    }

    #[cfg(debug_assertions)]
    {
        let mut parsed = Parsed::new();
        parsed.read(glob);
        debug_assert!(matched == match_groups(glob, start.glob_start, path, &parsed));
    }

    Some(matched)
}

#[inline(never)]
fn match_choices(glob: &[u8], path: &[u8], mut start: Start) -> bool {
    let negated = start.glob_start % 2 == 1;
    let found = |matched: bool| matched != negated;
    loop {
        let taken = match glob.ends_with(b"*}") {
            true => None,
            false => take_plain_choice(
                glob,
                start.glob_index,
                path,
                start.path_index,
                Pending::Nothing,
            ),
        };

        match taken {
            Some(Taken::To(glob_index, path_index)) => {
                (start.glob_index, start.path_index) = (glob_index, path_index);
                start.seen = Seen::Choice;
            }
            Some(Taken::Nothing) => return found(false),
            None => {
                if let Some(matched) = match_branches(glob, path, &start) {
                    return found(matched);
                }

                if start.seen == Seen::OnlyRegular {
                    start.seen = Seen::NoPlainChoice;
                }

                return match_more(glob, path, start, None);
            }
        }

        let (mut index, mut path_index) = (start.glob_index as usize, start.path_index as usize);
        while let Some(&wanted) = glob.get(index) {
            let is_same = match path.get(path_index) {
                Some(&found) if IS_PLAIN[wanted as usize] => found == wanted,
                Some(&found) if wanted == b'/' => is_separator(found),
                None if IS_PLAIN[wanted as usize] || wanted == b'/' => false,
                _ => break,
            };

            if !is_same {
                return found(false);
            }

            index += 1;
            path_index += 1;
        }

        if index == glob.len() {
            return found(path_index == path.len());
        }

        (start.glob_index, start.path_index) = (index as u32, path_index as u32);
        if glob[index] != b'{' {
            return match is_globstar_before_text(glob, start.glob_start as usize, index) {
                true => match_after_globstar(glob, path, start),
                false => match_more(glob, path, start, None),
            };
        }
    }
}

#[inline(never)]
fn match_negated(glob: &[u8], path: &[u8]) -> bool {
    let glob_start = negations(glob);
    let negated = glob_start % 2 == 1;
    let mut taken = 0;
    while let Some(&wanted) = glob.get(glob_start + taken) {
        let is_same = match path.get(taken) {
            Some(&found) if IS_PLAIN[wanted as usize] => found == wanted,
            Some(&found) if wanted == b'/' => is_separator(found),
            None if IS_PLAIN[wanted as usize] || wanted == b'/' => false,
            _ => break,
        };

        if !is_same {
            return negated;
        }

        taken += 1;
    }

    let Some(next) = glob.get(glob_start + taken) else {
        return (taken == path.len()) != negated;
    };

    let start = Start {
        glob_start: glob_start as u32,
        glob_index: (glob_start + taken) as u32,
        path_index: taken as u32,
        ..Start::of()
    };

    if is_globstar_before_text(glob, glob_start, glob_start + taken) {
        return match_after_globstar(glob, path, start);
    }

    if *next == b'{' {
        return match_choices(glob, path, start);
    }

    if is_last_star(glob, glob_start + taken) {
        return match_last_star(glob, path, start);
    }

    match matches!(next, b'\\' | b'?' | b'[' | 0x80..) {
        true => match_escapes(glob, path, start),
        false => match_more(glob, path, start, None),
    }
}

#[inline(never)]
fn match_escapes(glob: &[u8], path: &[u8], start: Start) -> bool {
    match_escapes_from::<false>(glob, path, start)
}

#[inline(never)]
fn match_escapes_at_start(glob: &[u8], path: &[u8]) -> bool {
    match_escapes_from::<true>(glob, path, Start::of())
}

#[inline(always)]
fn match_escapes_from<const IS_AT_START: bool>(glob: &[u8], path: &[u8], start: Start) -> bool {
    let glob_start = match glob[0] {
        b'!' if !IS_AT_START => negations(glob),
        _ => 0,
    };

    let negated = glob_start % 2 == 1;
    let (mut index, mut path_index) = (
        glob_start.max(start.glob_index as usize),
        start.path_index as usize,
    );
    let mut seen = Seen::OnlyRegular;
    let mut first_class = usize::MAX;
    while let Some(&wanted) = glob.get(index) {
        let found = path.get(path_index).copied();
        let is_same = if IS_PLAIN[wanted as usize] {
            found == Some(wanted)
        } else if wanted == b'/' {
            found.is_some_and(is_separator)
        } else if wanted >= 128 {
            let len = usize::from(wtf8_byte_sequence_length(wanted));
            let Some(bytes) = glob
                .get(index..index + len)
                .filter(|it| !it.iter().any(u8::is_ascii))
            else {
                break;
            };

            index += len - 1;
            path_index += len - 1;
            path.get(path_index + 1 - len..=path_index) == Some(bytes)
        } else if wanted == b'\\'
            && glob
                .get(index + 1)
                .is_some_and(|it| *it < 128 && *it != b'/')
        {
            let (mut escaped, mut at) = (wanted, index as u32);
            unescape(&mut escaped, glob, &mut at);
            index += 1;
            if glob[index] == b'\\' {
                seen = Seen::Odd;
            }

            found == Some(escaped)
        } else if found.is_some_and(|it| it >= 128) {
            break;
        } else if wanted == b'?' && glob.get(index + 1) != Some(&b'(') {
            found.is_some_and(|it| !is_separator(it))
        } else if wanted == b'['
            && let Some((has, close, _)) = ascii_class::<false>(glob, index, found.unwrap_or(0))
        {
            first_class = first_class.min(index);
            index = close;
            has && found.is_some()
        } else {
            break;
        };

        if !is_same {
            return negated;
        }

        index += 1;
        path_index += 1;
    }

    if index == glob.len() {
        return (path_index == path.len()) != negated;
    }

    if first_class != usize::MAX && !has_regular_classes(&glob[first_class..index]) {
        seen = Seen::Odd;
    }

    let start = Start {
        glob_start: glob_start as u32,
        glob_index: index as u32,
        path_index: path_index as u32,
        seen,
    };

    match is_last_star(glob, index) {
        true => match_last_star(glob, path, start),
        false => match_more(glob, path, start, None),
    }
}

#[inline(always)]
fn ascii_class<const ASKS_IF_REGULAR: bool>(
    glob: &[u8],
    open: usize,
    c: u8,
) -> Option<(bool, usize, bool)> {
    let mut at = open + 1;
    let negated = matches!(glob.get(at), Some(b'^' | b'!'));
    at += usize::from(negated);
    let first = at;
    let mut has = false;
    let mut is_regular = !negated && glob.get(at) != Some(&b'(');
    loop {
        let low = *glob.get(at)?;
        if low == b']' && at > first {
            return Some((has != negated, at, is_regular));
        }

        if low == b'\\' || low >= 128 {
            return None;
        }

        if glob.get(at + 1) == Some(&b'-')
            && let Some(&high) = glob.get(at + 2)
            && high != b']'
        {
            if high == b'\\' || high >= 128 {
                return None;
            }

            has |= low <= c && c <= high;
            if ASKS_IF_REGULAR {
                is_regular &= !(low <= b'/' && b'/' <= high) && !(low <= b'\\' && b'\\' <= high);
            }

            at += 3;
        } else {
            has |= low == c;
            if ASKS_IF_REGULAR {
                is_regular &= low != b'/';
            }

            at += 1;
        }
    }
}

#[inline(never)]
fn has_regular_classes(tokens: &[u8]) -> bool {
    let mut at = 0;
    while let Some(&ch) = tokens.get(at) {
        at = match ch {
            b'\\' => at + 2,
            b'[' => match ascii_class::<true>(tokens, at, 0) {
                Some((_, close, true)) => close + 1,
                _ => return false,
            },
            _ => at + 1,
        };
    }

    true
}

#[inline(never)]
fn match_more(glob: &[u8], path: &[u8], start: Start, resumed: Option<&mut State>) -> bool {
    let glob_start = start.glob_start;
    let negated = glob_start % 2 == 1;
    let found = |matched: bool| matched != negated;
    let mut state = State::at(start.glob_index, start.path_index, start.seen);
    match &resumed {
        Some(it) => state = **it,
        None => {
            if path.len() >= Program::IS_LONG
                && let Some(matched) = match_at_once(glob, glob_start, path)
            {
                return found(matched);
            }
        }
    }

    let outcome = run(glob, glob_start, path, &mut state);
    if let Some(it) = resumed {
        state.outcome = outcome;
        *it = state;
        return false;
    }

    let matched = match outcome {
        Outcome::No => false,
        Outcome::Yes => true,
        _ => {
            state.outcome = outcome;
            match_slowly(glob, glob_start, path, state)
        }
    };

    #[cfg(debug_assertions)]
    if path.len() < Program::IS_LONG && std::str::from_utf8(glob).is_ok() {
        let mut parsed = Parsed::new();
        parsed.read(glob);
        debug_assert!(
            !(parsed.is_regular || parsed.is_fixed)
                || matched == match_groups(glob, glob_start, path, &parsed),
            "the loop and the walk differ"
        );
    }

    found(matched)
}

#[inline(never)]
fn match_at_once(glob: &[u8], glob_start: u32, path: &[u8]) -> Option<bool> {
    let mut groups = Groups::new();
    let parsed = groups.parsed(glob);
    let walks = parsed.may_walk();
    walks.then(|| match_groups(glob, glob_start, path, parsed))
}

#[cold]
#[inline(never)]
fn match_slowly(glob: &[u8], glob_start: u32, path: &[u8], mut state: State) -> bool {
    let mut groups = Groups::new();
    let start = Start {
        glob_start,
        ..Start::of()
    };

    loop {
        let goes_on = match state.outcome {
            Outcome::No => return false,
            Outcome::Yes => return true,
            Outcome::Odd => {
                if !groups.parsed(glob).is_regular {
                    break;
                }

                state.seen = Seen::Odd;
                true
            }
            Outcome::Unclosed => {
                state.no_class_from = state.glob_index;
                true
            }
            Outcome::Group => {
                let (index, path_index) = (state.glob_index, state.path_index);
                let tried = try_group(glob, index, path, path_index, state.seen, &mut groups);
                if groups.is_parsed() {
                    state.seen = Seen::Odd;
                }

                match tried {
                    None => {
                        state.not_group_at = state.glob_index;
                        true
                    }
                    Some(Tried::IsGroup) => break,
                    Some(Tried::Failed) => state.goes_back(path),
                    Some(Tried::Took(glob_index, path_index)) => {
                        (state.glob_index, state.path_index) = (glob_index, path_index);
                        true
                    }
                    Some(Tried::TookSegment(glob_index, path_index)) => {
                        (state.glob_index, state.path_index) = (glob_index, path_index);
                        (state.wildcard, state.seen) = (state.globstar, Seen::Choice);
                        true
                    }
                    Some(Tried::FailedSegment) => {
                        (state.wildcard, state.seen) = (state.globstar, Seen::Choice);
                        state.goes_back(path)
                    }
                }
            }
        };

        if !goes_on {
            return false;
        }

        match_more(glob, path, start, Some(&mut state));
    }

    match_groups(glob, glob_start, path, groups.parsed(glob))
}

#[inline(never)]
fn match_groups(glob: &[u8], glob_start: u32, path: &[u8], parsed: &Parsed) -> bool {
    let Some(after_front) = parsed.take_front(glob, glob_start, path) else {
        return false;
    };

    if parsed.first_group != u32::MAX
        && !parsed.may_start_at(parsed.first_group, glob, path, after_front)
    {
        return false;
    }

    if path.len() >= Program::IS_LONG && !parsed.may_pass_first_group(glob, path, after_front) {
        return false;
    }

    let mut program = Program::new();
    program.read(glob, parsed, parsed.front_end as usize);
    program.on(glob, path, after_front as usize).walk()
}

static IS_PLAIN: [bool; 256] = {
    let mut table = [false; 256];
    let mut byte = 0;
    while byte < 128 {
        table[byte] = !matches!(
            byte as u8,
            b'*' | b'?' | b'[' | b'{' | b'\\' | b'/' | b'!' | b'+' | b'@'
        );
        byte += 1;
    }

    table
};

#[inline(never)]
fn last_segments(tail: &[u8], rest: &[u8]) -> Result<Option<usize>, ()> {
    if tail == b"*" {
        return Err(());
    }

    let mut slashes = 0;
    let (mut braces, mut parentheses) = (0_u32, 0_u32);
    let mut at = 0;
    while let Some(&ch) = tail.get(at) {
        at += 1;
        match ch {
            _ if IS_TEXT[ch as usize] => {}
            b'/' if braces + parentheses > 0 => return Err(()),
            b'/' => slashes += 1,
            b'{' => braces += 1,
            b'(' => parentheses += 1,
            b'}' => braces = braces.saturating_sub(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'*' if tail.get(at) == Some(&b'*') => return Err(()),
            b'[' => match AsciiClass::read(tail, at - 1) {
                Some(class) if class.is_regular(tail, at - 1) => at = class.end as usize,
                _ => return Err(()),
            },
            b'\\' => match tail.get(at) {
                Some(b'/' | b'\\' | 0x80..) | None => return Err(()),
                Some(_) => at += 1,
            },
            0x80.. => return Err(()),
            _ => {}
        }
    }

    for (index, &found) in rest.iter().enumerate().rev() {
        if found == b'/' {
            if slashes == 0 {
                return Ok(Some(index + 1));
            }

            slashes -= 1;
        }
    }

    Ok((slashes == 0).then_some(0))
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Outcome {
    No,
    Yes,
    Group,
    Odd,
    Unclosed,
}

#[inline(always)]
fn run(glob: &[u8], glob_start: u32, path: &[u8], state: &mut State) -> Outcome {
    let (no_class_from, not_group_at) = (state.no_class_from, state.not_group_at);
    let mut kept = Kept::NOTHING;

    macro_rules! stop {
        ($outcome:expr) => {
            return $outcome
        };
    }

    macro_rules! see_odd_token {
        () => {{
            if state.seen == Seen::Choice {
                stop!(Outcome::Odd);
            }

            state.seen = Seen::Odd;
        }};
    }
    'main_loop: while (state.glob_index as usize) < glob.len()
        || (state.path_index as usize) < path.len()
    {
        if (state.glob_index as usize) < glob.len() {
            'fallthrough: {
                let ch = glob[state.glob_index as usize];
                if IS_PLAIN[ch as usize] {
                    let (mut index, mut path_index) =
                        (state.glob_index as usize, state.path_index as usize);
                    while let Some(&wanted) = glob.get(index)
                        && IS_PLAIN[wanted as usize]
                    {
                        if path.get(path_index) != Some(&wanted) {
                            break 'fallthrough;
                        }

                        index += 1;
                        path_index += 1;
                    }

                    state.glob_index = index as u32;
                    state.path_index = path_index as u32;
                    continue 'main_loop;
                }

                if ch == b'*'
                    && let Some(&wanted) = glob.get(state.glob_index as usize + 1)
                    && IS_TEXT[wanted as usize]
                {
                    let mut index = state.path_index as usize;
                    while let Some(&found) = path.get(index)
                        && found != wanted
                        && !is_separator(found)
                    {
                        index += 1;
                    }

                    match path.get(index) {
                        Some(&found) if found == wanted => {
                            state.wildcard.glob_index = state.glob_index;
                            state.wildcard.path_index = index as u32 + 1;
                            state.glob_index += 2;
                            state.path_index = index as u32 + 1;
                            continue 'main_loop;
                        }
                        Some(_) => {
                            state.wildcard = state.globstar;
                            break 'fallthrough;
                        }
                        None => return Outcome::No,
                    }
                }

                if ch == b'*'
                    && glob
                        .get(state.glob_index as usize + 1)
                        .is_none_or(|it| *it == b'/')
                {
                    let mut index = state.path_index as usize;
                    while let Some(&found) = path.get(index)
                        && !is_separator(found)
                    {
                        index += 1;
                    }

                    let is_last = state.glob_index as usize + 1 == glob.len();
                    if index == path.len() {
                        return match is_last {
                            true => Outcome::Yes,
                            false => Outcome::No,
                        };
                    }

                    state.wildcard = state.globstar;
                    if is_last {
                        break 'fallthrough;
                    }

                    state.glob_index += 2;
                    state.path_index = index as u32 + 1;
                    continue 'main_loop;
                }

                if ch == b'*'
                    && state.globstar.glob_index == state.glob_index
                    && state.globstar.path_index == state.path_index
                    && state.path_index > 0
                    && (state.path_index as usize) < path.len()
                    && let Some(&wanted) = glob.get(state.glob_index as usize + 3)
                {
                    let is_literal = IS_PLAIN[wanted as usize] || wanted >= 128;
                    let mut start = state.path_index as usize;
                    let next = loop {
                        let mut end = start;
                        while let Some(&found) = path.get(end)
                            && !is_separator(found)
                        {
                            end += 1;
                        }

                        if !is_literal || end >= path.len() || path[start] == wanted {
                            break end + 1;
                        }

                        start = end + 1;
                    };

                    state.glob_index += 3;
                    state.path_index = start as u32;
                    state.globstar.path_index = next as u32;
                    state.wildcard = state.globstar;
                    continue 'main_loop;
                }
                'to_else: {
                    match ch {
                        b'*' | b'?' | b'+' | b'@' | b'!' | b'{'
                            if (ch == b'{'
                                || glob.get(state.glob_index as usize + 1) == Some(&b'('))
                                && state.glob_index != not_group_at =>
                        {
                            if !matches!(state.seen, Seen::Odd | Seen::NoPlainChoice)
                                && !(kept.layout.open == state.glob_index && kept.layout.has_star)
                                && let Some(taken) = take_plain_choice(
                                    glob,
                                    state.glob_index,
                                    path,
                                    state.path_index,
                                    if state.wildcard.path_index == 0 {
                                        Pending::Nothing
                                    } else if state.wildcard.path_index == state.globstar.path_index
                                    {
                                        Pending::Globstar
                                    } else if state.wildcard.glob_index + 1 == state.glob_index {
                                        Pending::StarBefore
                                    } else {
                                        Pending::Star
                                    },
                                )
                            {
                                state.seen = Seen::Choice;
                                match taken {
                                    Taken::To(glob_index, path_index) => {
                                        state.glob_index = glob_index;
                                        state.path_index = path_index;
                                        continue 'main_loop;
                                    }
                                    Taken::Nothing => break 'fallthrough,
                                }
                            }

                            if state.seen == Seen::NoPlainChoice {
                                state.seen = Seen::OnlyRegular;
                            }

                            if state.seen != Seen::Odd
                                && let Some(written) = match_written_out(
                                    glob,
                                    state.glob_index as usize,
                                    &Text {
                                        path,
                                        path_index: state.path_index as usize,
                                        is_committed: state.wildcard.path_index == 0,
                                    },
                                    &mut kept.layout,
                                )
                            {
                                #[cfg(debug_assertions)]
                                written.check(glob, state.glob_index as usize, path);
                                state.seen = Seen::Choice;
                                state.wildcard = state.globstar;
                                if !written.is_match {
                                    break 'fallthrough;
                                }

                                state.glob_index = written.glob_index;
                                state.path_index = written.path_index;
                                continue 'main_loop;
                            }

                            stop!(Outcome::Group);
                        }
                        b'*' => {
                            let is_globstar = (state.glob_index as usize) + 1 < glob.len()
                                && glob[state.glob_index as usize + 1] == b'*'
                                && glob.get(state.glob_index as usize + 2) != Some(&b'(');
                            let mut is_whole = false;
                            if is_globstar {
                                let at = state.glob_index as usize;
                                is_whole = (at <= glob_start as usize || glob[at - 1] == b'/')
                                    && matches!(glob.get(at + 2), None | Some(b'/'));
                                if !is_whole {
                                    see_odd_token!();
                                }

                                if glob.get(at + 3) == Some(&b'*') {
                                    skip_globstars(glob, &mut state.glob_index);
                                }

                                if is_whole
                                    && state.seen != Seen::Odd
                                    && (state.globstar.path_index == 0
                                        || state.globstar.glob_index != state.glob_index)
                                    && !glob.ends_with(b"**")
                                    && let Some(tail) = glob.get(state.glob_index as usize + 3..)
                                    && let Ok(start) =
                                        last_segments(tail, &path[state.path_index as usize..])
                                {
                                    let Some(start) = start else {
                                        return Outcome::No;
                                    };

                                    state.glob_index += 3;
                                    state.path_index += start as u32;
                                    state.wildcard = Wildcard::default();
                                    state.globstar = Wildcard::default();
                                    continue 'main_loop;
                                }
                            }

                            let before = state.wildcard;

                            let mut can_start = true;

                            if (!is_globstar || !is_whole)
                                && let Some(&wanted) = glob
                                    .get(state.glob_index as usize + 1 + usize::from(is_globstar))
                                && IS_PLAIN[wanted as usize]
                            {
                                while let Some(&found) = path.get(state.path_index as usize)
                                    && found != wanted
                                    && !is_separator(found)
                                {
                                    state.path_index += 1;
                                }
                            } else if !is_globstar
                                && glob
                                    .get(state.glob_index as usize + 1)
                                    .is_none_or(|next| *next == b'/')
                            {
                                while let Some(&found) = path.get(state.path_index as usize)
                                    && !is_separator(found)
                                {
                                    state.path_index += 1;
                                }
                            } else if !is_globstar {
                                let skipped = skip_ahead(
                                    glob,
                                    state.glob_index + 1,
                                    path,
                                    state.path_index,
                                    before.path_index == 0 || before.glob_index != state.glob_index,
                                    state.seen == Seen::Odd,
                                    &mut kept,
                                );
                                state.path_index = skipped.path_index;
                                can_start = skipped.can_start;
                            }

                            state.wildcard.glob_index = state.glob_index;
                            state.wildcard.path_index = state.path_index
                                + if (state.path_index as usize) < path.len() {
                                    u32::from(rune_len_at(path, state.path_index as usize))
                                } else {
                                    1
                                };

                            let mut in_globstar = false;
                            if is_globstar {
                                state.glob_index += 2;

                                let is_end_invalid = (state.glob_index as usize) < glob.len();

                                if is_end_invalid
                                    && state.path_index as usize == path.len()
                                    && glob.len() - state.glob_index as usize == 2
                                    && is_separator(glob[state.glob_index as usize])
                                    && glob[state.glob_index as usize + 1] == b'*'
                                {
                                    continue 'main_loop;
                                }

                                if is_whole
                                    || (state.glob_index.saturating_sub(glob_start) < 3
                                        || glob[state.glob_index as usize - 3] == b'/')
                                        && (!is_end_invalid
                                            || glob[state.glob_index as usize] == b'/')
                                {
                                    if is_end_invalid {
                                        state.glob_index += 1;
                                    }

                                    if !is_end_invalid {
                                        return Outcome::Yes;
                                    }

                                    state.skip_to_separator(path, is_end_invalid);
                                    in_globstar = true;

                                    if let Some(&wanted) = glob.get(state.glob_index as usize)
                                        && (IS_PLAIN[wanted as usize] || wanted >= 128)
                                    {
                                        while let Some(&found) = path.get(state.path_index as usize)
                                            && found != wanted
                                            && state.wildcard.path_index as usize <= path.len()
                                        {
                                            state.path_index = state.wildcard.path_index;
                                            state.wildcard.path_index += 1;
                                            state.skip_to_separator(path, is_end_invalid);
                                        }
                                    }
                                }
                            } else {
                                state.glob_index += 1;
                            }

                            if !in_globstar
                                && (state.path_index as usize) < path.len()
                                && is_separator(path[state.path_index as usize])
                            {
                                state.wildcard = state.globstar;
                            }

                            if !can_start {
                                break 'fallthrough;
                            }

                            continue 'main_loop;
                        }
                        b'?' => {
                            if (state.path_index as usize) < path.len() {
                                if !is_separator(path[state.path_index as usize]) {
                                    state.glob_index += 1;
                                    state.path_index +=
                                        u32::from(rune_len_at(path, state.path_index as usize));
                                    continue 'main_loop;
                                }

                                break 'fallthrough;
                            } else {
                                break 'to_else;
                            }
                        }
                        b'[' if state.glob_index < no_class_from => {
                            if (state.path_index as usize) < path.len() {
                                let (c, len) = decode_rune_at(path, state.path_index as usize);
                                let (class, is_regular) = if kept.class_seen_at != state.glob_index
                                    && c < 128
                                    && let Some((has, close, is_regular)) = ascii_class::<true>(
                                        glob,
                                        state.glob_index as usize,
                                        c as u8,
                                    ) {
                                    kept.class_seen_at = state.glob_index;
                                    (Some((has, close as u32 + 1)), is_regular)
                                } else {
                                    let class = match kept.class_at(glob, state.glob_index) {
                                        Some(class) => Some((class.has(c), class.end)),
                                        None => class_has_at(glob, state.glob_index, c),
                                    };

                                    let is_kept = kept.class_index == state.glob_index;
                                    (class, is_kept && kept.is_regular_class)
                                };

                                let Some((has, end)) = class else {
                                    see_odd_token!();
                                    stop!(Outcome::Unclosed);
                                };

                                if !is_regular {
                                    see_odd_token!();
                                }

                                if !has {
                                    break 'fallthrough;
                                }

                                state.glob_index = end;
                                state.path_index += u32::from(len);
                                continue 'main_loop;
                            } else {
                                see_odd_token!();
                                break 'to_else;
                            }
                        }
                        _ => break 'to_else,
                    }
                }

                if (state.path_index as usize) < path.len() {
                    if ch == b'\\'
                        && glob
                            .get(state.glob_index as usize + 1)
                            .is_none_or(|it| *it == b'\\')
                    {
                        see_odd_token!();
                    }

                    let mut cc: u8 = ch;
                    unescape(&mut cc, glob, &mut state.glob_index);
                    let cc_len = wtf8_byte_sequence_length(cc);

                    let is_match = if cc == b'/' {
                        is_separator(path[state.path_index as usize])
                    } else if cc_len > 1 {
                        let pi = state.path_index as usize;
                        let gi = state.glob_index as usize;
                        let n = cc_len as usize;
                        pi + n <= path.len()
                            && gi + n <= glob.len()
                            && path[pi..pi + n] == glob[gi..gi + n]
                    } else {
                        path[state.path_index as usize] == cc
                    };

                    if is_match {
                        state.glob_index += u32::from(cc_len);
                        state.path_index += u32::from(cc_len);

                        if cc == b'/' {
                            state.wildcard = state.globstar;
                        }

                        continue 'main_loop;
                    }
                }
            }
        }

        if state.goes_back(path) {
            continue;
        }

        return Outcome::No;
    }

    Outcome::Yes
}

#[inline(always)]
fn match_class(glob: &[u8], open: u32, c: u32) -> Option<(bool, u32)> {
    let mut glob_index = open + 1;

    let mut negated = false;
    if (glob_index as usize) < glob.len()
        && (glob[glob_index as usize] == b'^' || glob[glob_index as usize] == b'!')
    {
        negated = true;
        glob_index += 1;
    }

    let mut first = true;
    let mut is_match = false;

    while (glob_index as usize) < glob.len() && (first || glob[glob_index as usize] != b']') {
        let mut low: u32 = glob[glob_index as usize] as u32;
        let mut low_len: u8 = 1;
        if !get_unicode(&mut low, &mut low_len, glob, &mut glob_index) {
            return None;
        }

        glob_index += u32::from(low_len);

        let high = if (glob_index as usize) + 1 < glob.len()
            && glob[glob_index as usize] == b'-'
            && glob[glob_index as usize + 1] != b']'
        {
            glob_index += 1;

            let mut high: u32 = glob[glob_index as usize] as u32;
            let mut high_len: u8 = 1;
            if !get_unicode(&mut high, &mut high_len, glob, &mut glob_index) {
                return None;
            }

            glob_index += u32::from(high_len);
            high
        } else {
            low
        };

        if low <= c && c <= high {
            is_match = true;
        }

        first = false;
    }

    if glob_index as usize >= glob.len() {
        return None;
    }

    Some((is_match != negated, glob_index + 1))
}

#[inline(always)]
fn class_shape(glob: &[u8], open: usize) -> Option<(usize, bool)> {
    let mut at = open + 1;
    while let Some(&ch) = glob.get(at)
        && !matches!(ch, b'^' | b'!' | b'\\' | b'-' | b'/' | 0x80..)
    {
        if ch == b']' && at > open + 1 {
            return Some((at + 1, false));
        }

        at += 1;
    }

    class_shape_slowly(glob, open)
}

#[cold]
#[inline(never)]
fn class_shape_slowly(glob: &[u8], open: usize) -> Option<(usize, bool)> {
    let has = |it: u8| class_has(glob, open as u32, u32::from(it)) == Some(true);
    Some((class_end(glob, open)?, has(b'/') || has(b'\\')))
}

#[inline(never)]
fn class_has_at(glob: &[u8], open: u32, c: u32) -> Option<(bool, u32)> {
    match_class(glob, open, c)
}

#[inline]
fn class_has(glob: &[u8], open: u32, c: u32) -> Option<bool> {
    class_has_at(glob, open, c).map(|it| it.0)
}

#[inline(always)]
pub(crate) fn negations(glob: &[u8]) -> usize {
    let bangs = glob.iter().take_while(|it| **it == b'!').count();
    bangs - usize::from(bangs > 0 && glob.get(bangs) == Some(&b'('))
}

fn class_end(glob: &[u8], open: usize) -> Option<usize> {
    let mut at = open + 1;
    at += usize::from(matches!(glob.get(at), Some(b'^' | b'!')));
    let character = |at: usize| {
        let at = at + usize::from(glob.get(at) == Some(&b'\\'));
        glob.get(at)?;
        Some(at + usize::from(rune_len_at(glob, at)))
    };

    let mut is_first = true;
    while is_first || *glob.get(at)? != b']' {
        glob.get(at)?;
        at = character(at)?;
        if glob.get(at) == Some(&b'-') && glob.get(at + 1).is_some_and(|it| *it != b']') {
            at = character(at + 1)?;
        }

        is_first = false;
    }

    Some(at + 1)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Seen {
    OnlyRegular,
    Odd,
    Choice,
    NoPlainChoice,
}

struct Groups {
    parsed: Option<Parsed>,
    segment: Option<Option<Box<Segment>>>,
}

impl Groups {
    #[inline(always)]
    fn new() -> Groups {
        Groups {
            parsed: None,
            segment: None,
        }
    }

    fn is_parsed(&self) -> bool {
        self.parsed.is_some()
    }

    fn segment_at(&mut self, glob: &[u8], index: u32) -> Option<&Segment> {
        let it = self
            .segment
            .get_or_insert_with(|| Segment::of(glob, index as usize));

        it.as_deref()
            .filter(|it| it.start <= index && index < it.end)
    }

    #[inline(always)]
    fn parsed(&mut self, glob: &[u8]) -> &Parsed {
        self.parsed.get_or_insert_with(|| {
            let mut parsed = Parsed::new();
            parsed.read(glob);
            parsed
        })
    }
}

enum Tried {
    IsGroup,
    Failed,
    Took(u32, u32),
    TookSegment(u32, u32),
    FailedSegment,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Choice {
    No,
    OneLength,
    RestOfSegment,
    RestOfPath,
}

#[cold]
#[inline(never)]
fn try_group(
    glob: &[u8],
    index: u32,
    path: &[u8],
    path_index: u32,
    seen: Seen,
    groups: &mut Groups,
) -> Option<Tried> {
    if seen != Seen::Odd && !groups.is_parsed() {
        let is_inside = |it: &&u8| !is_separator(**it);
        let (before, after) = path.split_at(path_index as usize);
        let from = before.len() - before.iter().rev().take_while(is_inside).count();
        let to = before.len() + after.iter().take_while(is_inside).count();
        let walks = |it: &Segment| {
            let pattern = &glob[it.start as usize..it.end as usize];
            let on = (it.program).on(pattern, &path[..to], from);
            may_end_with(pattern, &path[from..to]) && on.walk()
        };

        let found = groups.segment_at(glob, index).map(|it| (walks(it), it.end));
        if let Some((is_match, end)) = found {
            return Some(match is_match {
                true => Tried::TookSegment(end, to as u32),
                false => Tried::FailedSegment,
            });
        }
    }

    let start = index + if glob[index as usize] == b'{' { 1 } else { 2 };
    let shape = groups.parsed(glob);
    if seen == Seen::Choice && !shape.is_regular {
        return Some(Tried::IsGroup);
    }

    let first = shape.group_at(start)?;
    if shape.has_group {
        return Some(match shape.may_start_at(first, glob, path, path_index) {
            true => Tried::IsGroup,
            false => Tried::Failed,
        });
    }

    Some(
        match take_choice(&shape.alternatives, first, glob, path, path_index) {
            Some((glob_index, path_index)) => Tried::Took(glob_index, path_index),
            None => Tried::Failed,
        },
    )
}

struct Text<'a> {
    path: &'a [u8],
    path_index: usize,
    is_committed: bool,
}

#[derive(Copy, Clone)]
struct Written {
    is_match: bool,
    glob_index: u32,
    path_index: u32,
}

#[cfg(debug_assertions)]
impl Written {
    fn check(self, glob: &[u8], open: usize, path: &[u8]) {
        let Some(it) = Segment::of(glob, open) else {
            return;
        };

        let pattern = &glob[it.start as usize..it.end as usize];
        let to = self.path_index as usize;
        let inside = path[..to].iter().rev();
        let from = to - inside.take_while(|it| !is_separator(**it)).count();
        let on = (it.program).on(pattern, &path[..to], from);
        let walked = may_end_with(pattern, &path[from..to]) && on.walk();
        debug_assert!((self.is_match, self.glob_index) == (walked, it.end));
    }
}

#[derive(Copy, Clone)]
struct Layout {
    open: u32,
    close: u32,
    commas: [u32; 4],
    count: u8,
    end: u32,
    has_star: bool,
}

impl Layout {
    const NONE: Layout = Layout {
        open: u32::MAX,
        close: 0,
        commas: [0; 4],
        count: 0,
        end: 0,
        has_star: false,
    };

    #[inline(always)]
    fn read(glob: &[u8], open: usize) -> Option<Layout> {
        if glob[open] != b'{' {
            return None;
        }

        let mut commas = [0; 4];
        let (mut count, mut close) = (0, 0);
        let (mut depth, mut has_comma, mut groups) = (1, 0_u32, 1);
        let mut has_star = false;
        let mut end = open + 1;
        while let Some(&ch) = glob.get(end) {
            match ch {
                _ if IS_TEXT[ch as usize] => {}
                0x80.. => {
                    let len = usize::from(wtf8_byte_sequence_length(ch));
                    if glob.get(end..end + len)?.iter().any(u8::is_ascii) {
                        return None;
                    }

                    end += len - 1;
                }
                b'/' if depth == 0 => break,
                b'/' => return None,
                b',' if depth > 0 => {
                    has_comma |= 1 << depth;
                    if close == 0 && depth == 1 {
                        *commas[..3].get_mut(count)? = end as u32;
                        count += 1;
                    }
                }
                b'{' if groups < 3 => {
                    groups += 1;
                    depth += 1;
                    has_comma &= !(1 << depth);
                }
                b'}' if has_comma & 1 << depth != 0 => {
                    depth -= 1;
                    if close == 0 && depth == 0 {
                        close = end;
                    }
                }
                b'*' => {
                    has_star |= close == 0;
                    if glob.get(end + 1) == Some(&b'*') {
                        return None;
                    }
                }
                b'?' => {}
                b'[' => {
                    let (after, may_be_separator) = class_shape(glob, end)?;
                    if may_be_separator || glob[end + 1] == b'(' {
                        return None;
                    }

                    end = after - 1;
                }
                _ => return None,
            }

            end += 1;
        }

        if depth > 0 {
            return None;
        }

        commas[count] = close as u32;
        Some(Layout {
            open: open as u32,
            close: close as u32,
            commas,
            count: count as u8,
            end: end as u32,
            has_star,
        })
    }
}

#[inline(never)]
fn match_written_out(
    glob: &[u8],
    open: usize,
    text: &Text<'_>,
    layout: &mut Layout,
) -> Option<Written> {
    if layout.open != open as u32 {
        *layout = Layout::read(glob, open).unwrap_or(Layout {
            open: open as u32,
            ..Layout::NONE
        });
    }

    let Layout { commas, count, .. } = *layout;
    let (close, end) = (layout.close as usize, layout.end as usize);
    if close == 0 {
        return None;
    }

    let is_inside = |it: &&u8| **it != b'/';
    let (left, right) = text.path.split_at(text.path_index);
    let (start, path_start) = match text.is_committed {
        true => (open, left.len()),
        false => {
            let before = glob[negations(glob)..open].iter().rev();
            (
                open - before.take_while(|it| **it != b'/').count(),
                left.len() - left.iter().rev().take_while(is_inside).count(),
            )
        }
    };

    let path_end = left.len() + right.iter().take_while(is_inside).count();
    let path = &text.path[path_start..path_end];
    let found = |is_match: bool| Written {
        is_match,
        glob_index: end as u32,
        path_index: path_end as u32,
    };

    let (before, after) = (&glob[start..open], &glob[close + 1..end]);
    let is_written = end - start > close + 1 - open;
    let mut written = [0; 64];
    if is_written && end - start > written.len() {
        return None;
    }

    if !after.is_empty() && !may_end_with(after, path) {
        return Some(found(false));
    }

    let seam = written.len() - after.len();
    if is_written {
        written[seam..].copy_from_slice(after);
    }

    let mut from = open + 1;
    for &to in &commas[..=usize::from(count)] {
        let alternative = &glob[from..to as usize];
        from = to as usize + 1;
        if before.is_empty()
            && let Some(first) = alternative.first()
            && IS_PLAIN[*first as usize]
            && path.first() != Some(first)
        {
            continue;
        }

        if after.is_empty()
            && let Some(last) = alternative.last()
            && IS_TEXT[*last as usize]
            && *last != b']'
            && path.last() != Some(last)
        {
            continue;
        }

        let mut pattern = alternative;
        if is_written {
            let other_seam = seam - alternative.len();
            written[other_seam..seam].copy_from_slice(alternative);
            let start = other_seam - before.len();
            written[start..other_seam].copy_from_slice(before);
            let is_globstar =
                |at: usize| at > start && written[at - 1] == b'*' && written.get(at) == Some(&b'*');
            if is_globstar(seam) || is_globstar(other_seam) {
                return None;
            }

            pattern = &written[start..];
        }

        if pattern.first() == Some(&b'!') {
            return None;
        }

        if match_from_first_byte(pattern, path) {
            return Some(found(true));
        }
    }

    Some(found(false))
}

fn may_end_with(pattern: &[u8], text: &[u8]) -> bool {
    let literal = pattern.iter().rev();
    let len = literal
        .take_while(|it| IS_TEXT[**it as usize] && **it != b']')
        .count();
    let (before, mut literal) = pattern.split_at(pattern.len() - len);
    if before.last() == Some(&b'\\') && !literal.is_empty() {
        literal = &literal[1..];
    }

    text.ends_with(literal) || before.iter().rev().take(3).any(|it| *it >= 128)
}

struct Firsts {
    index: u32,
    count: u8,
    bytes: [u8; 8],
    longest_at_end: Option<u32>,
}

impl Firsts {
    const NONE: Firsts = Firsts {
        index: u32::MAX,
        count: 0,
        bytes: [0; 8],
        longest_at_end: None,
    };

    #[inline]
    fn read(&mut self, glob: &[u8], index: u32) {
        *self = Firsts {
            index,
            ..Firsts::NONE
        };

        let Some(Brackets {
            first,
            separator,
            closer,
        }) = brackets_at(glob, index as usize)
        else {
            return;
        };

        if glob[index as usize] == b'?' {
            return;
        }

        let (mut count, mut has_firsts) = (0, true);
        let (mut start, mut longest) = (first, 0);
        let mut at = first;
        loop {
            let Some(&ch) = glob.get(at) else {
                return;
            };

            if ch == separator || ch == closer {
                has_firsts &= at > start;
                longest = longest.max(at - start);
                if ch == closer {
                    break;
                }

                start = at + 1;
            } else if !IS_PLAIN[ch as usize] {
                return;
            } else if at == start {
                match self.bytes.get_mut(count) {
                    Some(it) if IS_TEXT[ch as usize] => *it = ch,
                    _ => has_firsts = false,
                }

                count += 1;
            }

            at += 1;
        }

        if start == first {
            return;
        }

        if has_firsts {
            self.count = count as u8;
        }

        if matches!(glob.get(at + 1), None | Some(b'/')) {
            self.longest_at_end = Some(longest as u32);
        }
    }
}

#[inline(never)]
fn skip_ahead(
    glob: &[u8],
    index: u32,
    path: &[u8],
    path_index: u32,
    is_first_time: bool,
    has_seen_odd_token: bool,
    kept: &mut Kept,
) -> Skipped {
    let next = glob[index as usize];
    if matches!(next, b'{' | b'@' | b'+') {
        if has_seen_odd_token {
            return Skipped {
                path_index,
                can_start: true,
            };
        }

        return skip_to_start(glob, index, path, path_index, &mut kept.firsts);
    }

    let mut at = path_index as usize;
    if is_first_time && let Some((width, may_take_separator)) = fixed_width(&glob[index as usize..])
    {
        let rest = &path[at..];
        let len = rest
            .iter()
            .position(|it| is_separator(*it))
            .unwrap_or(rest.len());
        if rest[..len].is_ascii() && (!may_take_separator || len == rest.len()) {
            at += len - width.min(len);
        }
    } else if next >= 128 {
        while let Some(&found) = path.get(at)
            && found != next
            && !is_separator(found)
        {
            at += usize::from(rune_len_at(path, at));
        }
    } else if next == b'[' {
        let class = kept.class_at(glob, index);
        while let Some(&found) = path.get(at)
            && !is_separator(found)
        {
            let (c, len) = decode_rune_at(path, at);
            let has = match &class {
                Some(class) => Some(class.has(c)),
                None => class_has(glob, index, c),
            };

            if has != Some(false) {
                break;
            }

            at += usize::from(len);
        }
    } else if next == b'?' {
        let rest = &glob[index as usize..];
        let mut count = rest.iter().take_while(|it| **it == b'?').count();
        count -= usize::from(rest.get(count) == Some(&b'('));
        let wanted = rest.get(count).copied();
        if wanted.is_none_or(|it| it == b'/' || IS_PLAIN[it as usize]) {
            while let Some(window) = path.get(at..at + count)
                && window.is_ascii()
                && !is_separator(path[at])
            {
                let found = path.get(at + count).copied();
                let fits = match wanted {
                    Some(b'/') | None => found.is_none_or(is_separator),
                    Some(_) => found == wanted,
                };

                if fits || found.is_some_and(|it| it >= 128) {
                    break;
                }

                at += 1;
            }
        }
    }

    Skipped {
        path_index: at as u32,
        can_start: true,
    }
}

#[inline(always)]
fn fixed_width(tokens: &[u8]) -> Option<(usize, bool)> {
    let mut is_escaped = false;
    for &ch in tokens {
        if ch == b'/' {
            break;
        }

        if matches!(ch, b'*' | b'{' | 0x80..) && !is_escaped {
            return None;
        }

        is_escaped = ch == b'\\' && !is_escaped;
    }

    let (mut at, mut width) = (0, 0);
    let mut may_take_separator = false;
    while let Some(&ch) = tokens.get(at)
        && ch != b'/'
    {
        at = match ch {
            b'?' | b'+' | b'@' | b'!' if tokens.get(at + 1) == Some(&b'(') => return None,
            b'*' | b'{' | 0x80.. => return None,
            b'[' => {
                let class = AsciiClass::read(tokens, at)?;
                may_take_separator |= !class.is_regular(tokens, at);
                class.end as usize
            }
            b'\\' => match tokens.get(at + 1) {
                Some(b'/' | b'\\' | 0x80..) | None => return None,
                Some(_) => at + 2,
            },
            _ => at + 1,
        };

        width += 1;
    }

    (!may_take_separator || at == tokens.len()).then_some((width, may_take_separator))
}

struct AsciiClass {
    members: u128,
    negated: bool,
    end: u32,
}

impl AsciiClass {
    #[inline(always)]
    fn read(glob: &[u8], open: usize) -> Option<AsciiClass> {
        let mut at = open + 1;
        let negated = matches!(glob.get(at), Some(b'^' | b'!'));
        at += usize::from(negated);
        let first = at;
        let mut members = 0_u128;
        loop {
            let low = *glob.get(at)?;
            if low == b']' && at > first {
                break;
            }

            if low == b'\\' || low >= 128 {
                return None;
            }

            if glob.get(at + 1) == Some(&b'-')
                && let Some(&high) = glob.get(at + 2)
                && high != b']'
            {
                if high == b'\\' || high >= 128 {
                    return None;
                }

                if low <= high {
                    members |= (u128::MAX >> (127 - (high - low))) << low;
                }

                at += 3;
            } else {
                members |= 1 << low;
                at += 1;
            }
        }

        Some(AsciiClass {
            members: if negated { !members } else { members },
            negated,
            end: at as u32 + 1,
        })
    }

    #[inline(always)]
    fn has(&self, c: u32) -> bool {
        match c < 128 {
            true => self.members >> c & 1 == 1,
            false => self.negated,
        }
    }

    #[inline(always)]
    fn is_regular(&self, glob: &[u8], open: usize) -> bool {
        self.members & (1 << b'/' | 1 << b'\\') == 0 && glob[open + 1] != b'('
    }
}

struct Kept {
    firsts: Firsts,
    layout: Layout,
    class_seen_at: u32,
    class_index: u32,
    class: AsciiClass,
    is_regular_class: bool,
}

impl Kept {
    const NOTHING: Kept = Kept {
        firsts: Firsts::NONE,
        layout: Layout::NONE,
        class_seen_at: u32::MAX,
        class_index: u32::MAX,
        class: AsciiClass {
            members: 0,
            negated: false,
            end: 0,
        },
        is_regular_class: false,
    };

    #[inline(always)]
    fn class_at(&mut self, glob: &[u8], open: u32) -> Option<&AsciiClass> {
        if self.class_index != open {
            self.read_class(glob, open)?;
        }

        Some(&self.class)
    }

    #[inline(never)]
    fn read_class(&mut self, glob: &[u8], open: u32) -> Option<()> {
        self.class = AsciiClass::read(glob, open as usize)?;
        self.is_regular_class = self.class.is_regular(glob, open as usize);
        self.class_index = open;
        Some(())
    }
}

struct Skipped {
    path_index: u32,
    can_start: bool,
}

#[inline(never)]
fn skip_to_start(
    glob: &[u8],
    index: u32,
    path: &[u8],
    mut path_index: u32,
    firsts: &mut Firsts,
) -> Skipped {
    if firsts.index != index {
        firsts.read(glob, index);
    }

    if let Some(longest) = firsts.longest_at_end {
        let rest = &path[path_index as usize..];
        let len = rest
            .iter()
            .position(|it| is_separator(*it))
            .unwrap_or(rest.len());
        path_index += (len as u32).saturating_sub(longest);
    }

    if firsts.count == 0 {
        return Skipped {
            path_index,
            can_start: true,
        };
    }

    let firsts = &firsts.bytes[..usize::from(firsts.count)];
    while let Some(found) = path.get(path_index as usize)
        && !is_separator(*found)
        && !firsts.contains(found)
    {
        path_index += u32::from(rune_len_at(path, path_index as usize));
    }

    let can_start = path
        .get(path_index as usize)
        .is_some_and(|it| firsts.contains(it));
    Skipped {
        path_index,
        can_start,
    }
}

#[derive(Copy, Clone)]
struct Alternative {
    start: u32,
    end: u32,
    next: u32,
    close: u32,
    choice: Choice,
}

impl Alternative {
    #[inline(always)]
    fn may_start_at(&self, glob: &[u8], path: &[u8], path_index: u32) -> bool {
        let first = glob[self.start as usize];
        if self.start == self.end {
            return true;
        }

        match path.get(path_index as usize) {
            Some(found) if IS_PLAIN[first as usize] => *found == first,
            Some(_) if first == b'[' => {
                let c = decode_rune_at(path, path_index as usize).0;
                class_has(glob, self.start, c) != Some(false)
            }
            Some(_) => true,
            None => !IS_PLAIN[first as usize],
        }
    }

    #[inline(always)]
    fn may_go_on_at(&self, glob: &[u8], path: &[u8], path_index: u32) -> bool {
        let text = &glob[self.start as usize..self.end as usize];
        let literal = text.iter().take_while(|it| IS_PLAIN[**it as usize]).count();
        path[path_index as usize..].starts_with(&text[..literal])
    }

    #[inline(always)]
    fn is_optional(&self, glob: &[u8]) -> bool {
        glob[self.start as usize - 1] == b'(' && glob[self.start as usize - 2] == b'?'
    }

    const IS_SEQUENCE: u32 = u32::MAX - 1;
}

struct Sequence {
    from: i64,
    to: i64,
    step: i64,
    of: SequenceOf,
}

enum SequenceOf {
    Letters,
    Numbers { width: usize, longest: u32 },
}

impl Sequence {
    fn longest(&self) -> u32 {
        match self.of {
            SequenceOf::Letters => 1,
            SequenceOf::Numbers { longest, .. } => longest,
        }
    }

    fn read(written: &[u8]) -> Option<Sequence> {
        let mut parts = std::str::from_utf8(written)
            .ok()?
            .split("..")
            .map(str::as_bytes);
        let (from, to, step) = (parts.next()?, parts.next()?, parts.next());
        if parts.next().is_some() {
            return None;
        }

        let number = |text: &[u8]| -> Option<i64> {
            let digits = text.strip_prefix(b"-").unwrap_or(text);
            let is_number = !digits.is_empty() && digits.iter().all(u8::is_ascii_digit);
            std::str::from_utf8(text)
                .ok()
                .filter(|_| is_number)?
                .parse()
                .ok()
        };

        let is_padded = |text: &[u8]| matches!(text.strip_prefix(b"-").unwrap_or(text), [b'0', next, ..] if next.is_ascii_digit());
        let step = match step {
            Some(step) => number(step)?.checked_abs()?.max(1),
            None => 1,
        };

        match (from, to) {
            ([from], [to]) if from.is_ascii_alphabetic() && to.is_ascii_alphabetic() => {
                Some(Sequence {
                    from: i64::from(*from),
                    to: i64::from(*to),
                    step,
                    of: SequenceOf::Letters,
                })
            }
            _ => Some(Sequence {
                from: number(from)?,
                to: number(to)?,
                step,
                of: SequenceOf::Numbers {
                    width: match is_padded(from) || is_padded(to) {
                        true => from.len().max(to.len()),
                        false => 0,
                    },
                    longest: from.len().max(to.len()) as u32,
                },
            }),
        }
    }

    fn has_value(&self, value: i64) -> bool {
        (self.from.min(self.to)..=self.from.max(self.to)).contains(&value)
            && i128::from(value).abs_diff(i128::from(self.from)) % self.step as u128 == 0
    }

    fn has_nothing(&self) -> bool {
        matches!(self.of, SequenceOf::Letters) && self.has_value(i64::from(b'\\'))
    }

    fn has(&self, text: &[u8]) -> bool {
        let SequenceOf::Numbers { width, .. } = self.of else {
            return text[0] != b'\\' && self.has_value(i64::from(text[0]));
        };

        let digits = text.strip_prefix(b"-").unwrap_or(text);
        if digits.is_empty() || digits[0] == b'0' && digits.len() > 1 && text.len() != width {
            return false;
        }

        let zeros = (digits.iter().take_while(|it| **it == b'0').count()).min(digits.len() - 1);
        let Some(value) = std::str::from_utf8(text)
            .ok()
            .and_then(|it| it.parse::<i64>().ok())
        else {
            return false;
        };

        self.has_value(value)
            && !(value == 0 && digits.len() < text.len())
            && match zeros {
                0 => text.len() >= width,
                _ => text.len() == width,
            }
    }
}

struct Brackets {
    first: usize,
    separator: u8,
    closer: u8,
}

#[inline(always)]
fn brackets_at(glob: &[u8], open: usize) -> Option<Brackets> {
    match glob[open] {
        b'{' => Some(Brackets {
            first: open + 1,
            separator: b',',
            closer: b'}',
        }),
        b'@' | b'?' if glob.get(open + 1) == Some(&b'(') => Some(Brackets {
            first: open + 2,
            separator: b'|',
            closer: b')',
        }),
        _ => None,
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Pending {
    Nothing,
    Globstar,
    StarBefore,
    Star,
}

#[derive(Copy, Clone)]
enum Taken {
    To(u32, u32),
    Nothing,
}

#[inline(never)]
fn take_plain_choice(
    glob: &[u8],
    open: u32,
    path: &[u8],
    path_index: u32,
    pending: Pending,
) -> Option<Taken> {
    let Brackets {
        first,
        separator,
        closer,
    } = brackets_at(glob, open as usize)?;
    let is_optional = glob[open as usize] == b'?';
    let rest = &path[path_index as usize..];
    let mut lens = u64::from(is_optional);
    let mut has_slash = false;
    let (mut shortest, mut longest) = (if is_optional { 0 } else { usize::MAX }, 0);
    let mut start = first;
    let mut at = first;
    'group: loop {
        while let Some(&ch) = glob.get(at)
            && IS_TEXT[ch as usize]
            && rest.get(at - start) == Some(&ch)
        {
            at += 1;
        }

        let mut is_start = true;
        loop {
            let ch = *glob.get(at)?;
            if ch == separator || ch == closer {
                shortest = shortest.min(at - start);
                longest = longest.max(at - start);
                if is_start {
                    lens |= 1_u64.checked_shl((at - start) as u32)?;
                }

                at += 1;
                if ch == closer {
                    break 'group;
                }

                start = at;
                continue 'group;
            }

            if IS_PLAIN[ch as usize] {
                is_start = is_start && rest.get(at - start) == Some(&ch);
            } else if ch == b'/' && pending == Pending::Nothing {
                has_slash = true;
                is_start = is_start && rest.get(at - start).is_some_and(|it| is_separator(*it));
            } else if matches!(ch, b'?' | b'[' | b'{')
                && matches!(pending, Pending::Nothing | Pending::Globstar)
            {
                let is_committed = pending == Pending::Nothing;
                return take_fixed_choice(glob, open, path, path_index, is_committed);
            } else {
                return None;
            }

            at += 1;
            while !is_start
                && let Some(&ch) = glob.get(at)
                && IS_TEXT[ch as usize]
            {
                at += 1;
            }
        }
    }

    if !is_optional && start == first {
        return None;
    }

    let after = at;
    let mut taken = None;
    match glob.get(after) {
        None => {
            if rest.len() < 64 && lens >> rest.len() & 1 == 1 {
                taken = Some(rest.len());
            }
        }
        Some(b'/') => {
            while lens != 0 {
                let len = lens.trailing_zeros() as usize;
                lens &= lens - 1;
                if rest.get(len).is_none_or(|it| is_separator(*it)) {
                    if taken.is_some() {
                        return None;
                    }

                    taken = Some(len);
                }
            }
        }
        Some(_) if has_slash => return None,
        Some(_) if shortest == longest && shortest > 0 => taken = (lens != 0).then_some(shortest),
        Some(_) if pending == Pending::Star => return None,
        Some(b'*') if !matches!(glob.get(after + 1), Some(b'*' | b'(')) => {
            taken = (lens != 0).then(|| lens.trailing_zeros() as usize);
            if pending == Pending::StarBefore
                && let Some(len) = taken
                && len > shortest + 1
            {
                return None;
            }
        }
        Some(_) if pending == Pending::StarBefore => return None,
        Some(_) => {
            while lens != 0 {
                let len = lens.trailing_zeros() as usize;
                lens &= lens - 1;
                let mut index = 0;
                let fits = loop {
                    match glob.get(after + index) {
                        Some(wanted) if IS_PLAIN[*wanted as usize] || *wanted >= 128 => {
                            if rest.get(len + index) != Some(wanted) {
                                break false;
                            }

                            index += 1;
                        }
                        _ => break true,
                    }
                };

                if fits {
                    if taken.is_some() {
                        return None;
                    }

                    taken = Some(len);
                }
            }
        }
    }

    Some(match taken {
        Some(len) => Taken::To(after as u32, path_index + len as u32),
        None => Taken::Nothing,
    })
}

#[derive(Copy, Clone, Default)]
struct Level {
    starts: u64,
    ends: u64,
    before: (u32, u32),
    widths: (u32, u32),
    has_separator: bool,
}

#[inline(never)]
fn take_fixed_choice(
    glob: &[u8],
    open: u32,
    path: &[u8],
    path_index: u32,
    is_committed: bool,
) -> Option<Taken> {
    let Brackets {
        first,
        separator,
        closer,
    } = brackets_at(glob, open as usize)?;
    let rest = &path[path_index as usize..];
    let window = &rest[..rest.len().min(u64::BITS as usize - 1)];
    let mut levels = [Level::default(); 4];
    let mut depth = 0;
    levels[0] = Level {
        starts: 1,
        widths: (u32::MAX, 0),
        ..Level::default()
    };

    if glob[open as usize] == b'?' {
        levels[0].ends = 1;
        levels[0].widths = (0, 0);
        levels[0].has_separator = true;
    }

    let mut ends = 1_u64;
    let mut widths = (0, 0);
    let mut has_slash = false;
    let mut at = first;
    loop {
        let ch = *glob.get(at)?;
        at += 1;
        if IS_TEXT[ch as usize] || ch == b'/' || ch == b'?' || ch == b'[' {
            let mut class_end = 0;
            match ch {
                b'/' if !is_committed || depth > 0 => return None,
                b'/' => has_slash = true,
                b'?' if glob.get(at) == Some(&b'(') => return None,
                b'[' => {
                    let (_, close, is_regular) = ascii_class::<true>(glob, at - 1, 0)?;
                    if !is_regular {
                        return None;
                    }

                    class_end = close + 1;
                }
                _ => {}
            }

            let (mut from, mut to) = (ends, 0);
            while from != 0 {
                let len = from.trailing_zeros() as usize;
                from &= from - 1;
                let Some(&found) = window.get(len) else {
                    if len < rest.len() {
                        return None;
                    }

                    continue;
                };

                let is_same = match ch {
                    b'/' => is_separator(found),
                    b'?' | b'[' if found >= 128 => return None,
                    b'?' => !is_separator(found),
                    b'[' => ascii_class::<false>(glob, at - 1, found)?.0,
                    _ => found == ch,
                };

                if is_same {
                    to |= 2_u64 << len;
                }
            }

            ends = to;
            widths = (widths.0 + 1, widths.1 + 1);
            if ch == b'[' {
                at = class_end;
            }

            continue;
        }

        let level = &mut levels[depth];
        let (separator, closer) = match depth {
            0 => (separator, closer),
            _ => (b',', b'}'),
        };

        if ch == separator || ch == closer {
            level.ends |= ends;
            level.widths = (level.widths.0.min(widths.0), level.widths.1.max(widths.1));
            if ch == separator {
                level.has_separator = true;
                ends = level.starts;
                widths = (0, 0);
                continue;
            }

            if !level.has_separator {
                return None;
            }

            ends = level.ends;
            widths = (
                level.before.0 + level.widths.0,
                level.before.1 + level.widths.1,
            );
            if depth == 0 {
                break;
            }

            depth -= 1;
        } else if ch == b'{' && closer == b'}' {
            depth += 1;
            *levels.get_mut(depth)? = Level {
                starts: ends,
                before: widths,
                widths: (u32::MAX, 0),
                ..Level::default()
            };

            widths = (0, 0);
        } else {
            return None;
        }
    }

    let after = at;
    let shortest = widths.0 as usize;
    let mut taken = None;
    let mut lens = ends;
    match glob.get(after) {
        None => {
            if rest.len() < 64 && ends >> rest.len() & 1 == 1 {
                taken = Some(rest.len());
            }
        }
        Some(b'/') => {
            while lens != 0 {
                let len = lens.trailing_zeros() as usize;
                lens &= lens - 1;
                if rest.get(len).is_none_or(|it| is_separator(*it)) {
                    if taken.is_some() {
                        return None;
                    }

                    taken = Some(len);
                }
            }
        }
        Some(_) if has_slash => return None,
        Some(_) if widths.0 == widths.1 && widths.0 > 0 => {
            taken = (ends != 0).then_some(shortest);
        }
        Some(b'*') if !matches!(glob.get(after + 1), Some(b'*' | b'(')) => {
            taken = (ends != 0).then(|| ends.trailing_zeros() as usize);
        }
        Some(_) => {
            while lens != 0 {
                let len = lens.trailing_zeros() as usize;
                lens &= lens - 1;
                let mut index = 0;
                let fits = loop {
                    match glob.get(after + index) {
                        Some(wanted) if IS_PLAIN[*wanted as usize] || *wanted >= 128 => {
                            if rest.get(len + index) != Some(wanted) {
                                break false;
                            }

                            index += 1;
                        }
                        _ => break true,
                    }
                };

                if fits {
                    if taken.is_some() {
                        return None;
                    }

                    taken = Some(len);
                }
            }
        }
    }

    Some(match taken {
        Some(len) => Taken::To(after as u32, path_index + len as u32),
        None => Taken::Nothing,
    })
}

struct Segment {
    start: u32,
    end: u32,
    program: Program,
}

impl Segment {
    #[cold]
    #[inline(never)]
    fn of(glob: &[u8], index: usize) -> Option<Box<Segment>> {
        let before = glob[negations(glob)..index].iter().rev();
        let start = index - before.take_while(|it| **it != b'/').count();
        let end = index + glob[index..].iter().take_while(|it| **it != b'/').count();
        let pattern = &glob[start..end];
        if pattern[0] == b'!' || glob[index] == b'!' {
            return None;
        }

        let mut parsed = Parsed::new();
        parsed.read(pattern);
        if !parsed.is_regular || !parsed.is_one_segment {
            return None;
        }

        let mut it = Box::new(Segment {
            start: start as u32,
            end: end as u32,
            program: Program::new(),
        });
        it.program.read(pattern, &parsed, 0);
        Some(it)
    }
}

struct Parsed {
    alternatives: Vec<Alternative>,
    is_regular: bool,
    is_fixed: bool,
    is_plain_text: bool,
    has_group: bool,
    is_one_segment: bool,
    is_decided_at_once: bool,
    front_end: u32,
    first_group: u32,
}

struct Open {
    is_brace: bool,
    is_plain: bool,
    has_one_length: bool,
    first: u32,
    last: u32,
    first_len: u32,
    last_start: u32,
    starts_segment: bool,
}

static IS_TEXT: [bool; 256] = {
    let mut table = IS_PLAIN;
    let mut others = b"(),|}".as_slice();
    while let [first, rest @ ..] = others {
        table[*first as usize] = false;
        others = rest;
    }

    table
};

impl Parsed {
    #[inline(always)]
    fn new() -> Parsed {
        Parsed {
            alternatives: Vec::new(),
            is_regular: false,
            is_fixed: false,
            is_plain_text: false,
            has_group: false,
            is_one_segment: false,
            is_decided_at_once: false,
            front_end: 0,
            first_group: u32::MAX,
        }
    }

    fn may_walk(&self) -> bool {
        (self.is_regular || self.is_fixed) && !self.is_decided_at_once && !self.is_plain_text
    }

    #[inline(never)]
    fn read(&mut self, glob: &[u8]) {
        let found = &mut self.alternatives;
        let mut group = Open {
            is_brace: false,
            is_plain: false,
            has_one_length: false,
            first: 0,
            last: 0,
            first_len: 0,
            last_start: 0,
            starts_segment: false,
        };

        let mut depth = 0_usize;
        let mut outer: Vec<Open> = Vec::new();
        let top = negations(glob);
        let mut is_regular = true;
        let (mut has_group, mut has_choice) = (false, false);
        let mut count = [0_usize; 2];
        let mut no_class_from = usize::MAX;
        let mut escaped = usize::MAX;
        let mut i = top;
        while let Some(&ch) = glob.get(i)
            && (IS_PLAIN[ch as usize] || ch == b'/')
        {
            i += 1;
        }

        let mut front_end = i;
        self.is_plain_text = i == glob.len();
        let (mut has_star, mut has_separator_class) = (false, false);
        let mut is_one_segment = !glob[top..i].contains(&b'/');
        while let Some(&ch) = glob.get(i) {
            if IS_TEXT[ch as usize] {
                i += 1;
                continue;
            }

            let starts_segment = |at: usize, group: &Open| {
                at <= top
                    || glob[at - 1] == b'/' && escaped != at - 1
                    || depth > 0 && group.starts_segment && group.last_start as usize == at
            };

            let alternative = || Alternative {
                start: i as u32 + 1,
                end: 0,
                next: u32::MAX,
                close: 0,
                choice: Choice::No,
            };

            group.is_plain &= IS_PLAIN[ch as usize];
            has_star |= ch == b'*';
            match ch {
                b'/' => is_one_segment = false,
                b'?' | b'!' | b'+' | b'@' => {}
                b',' | b'|' => {
                    if depth > 0 && group.is_brace == (ch == b',') {
                        let next = found.len() as u32;
                        let ended = &mut found[group.last as usize];
                        (ended.end, ended.next) = (i as u32, next);
                        let len = i as u32 - group.last_start;
                        match group.first == group.last {
                            true => group.first_len = len,
                            false => group.has_one_length &= len == group.first_len,
                        }

                        (group.last, group.last_start) = (next, i as u32 + 1);
                        found.push(alternative());
                    }
                }

                b'{' | b'('
                    if ch == b'{'
                        || i > 0
                            && escaped != i - 1
                            && matches!(glob[i - 1], b'?' | b'*' | b'+' | b'@' | b'!') =>
                {
                    let at = if ch == b'(' { i - 1 } else { i };
                    is_one_segment &= glob[at] != b'!';
                    let inner = Open {
                        is_brace: ch == b'{',
                        is_plain: ch == b'{' || matches!(glob[i - 1], b'@' | b'?'),
                        has_one_length: true,
                        first: found.len() as u32,
                        last: found.len() as u32,
                        first_len: 0,
                        last_start: i as u32 + 1,
                        starts_segment: starts_segment(at, &group),
                    };

                    if depth > 0 {
                        outer.push(std::mem::replace(&mut group, inner));
                    } else {
                        group = inner;
                    }

                    depth += 1;
                    count[usize::from(ch == b'{')] += 1;
                    found.push(alternative());
                }
                b'}' | b')' if count[usize::from(ch == b'}')] > 0 => {
                    while group.is_brace != (ch == b'}') {
                        count[usize::from(group.is_brace)] -= 1;
                        is_regular = false;
                        depth -= 1;
                        if let Some(it) = outer.pop() {
                            group = it;
                        }
                    }

                    count[usize::from(group.is_brace)] -= 1;
                    depth -= 1;
                    found[group.last as usize].end = i as u32;
                    let is_innermost = group.first as usize + 1 == found.len();
                    let it = &mut found[group.first as usize];
                    it.close = i as u32;
                    let is_optional = it.is_optional(glob);
                    if group.is_plain && (group.first != group.last || is_optional) {
                        it.choice = match glob.get(i + 1) {
                            _ if depth > 0 => Choice::No,
                            None => Choice::RestOfPath,
                            Some(b'/') => Choice::RestOfSegment,
                            Some(_)
                                if !is_optional
                                    && group.has_one_length
                                    && group.first_len > 0
                                    && group.first_len == i as u32 - group.last_start =>
                            {
                                Choice::OneLength
                            }
                            Some(_) => Choice::No,
                        };
                    }

                    has_group |= it.choice == Choice::No;
                    has_choice |= it.choice != Choice::No;
                    if group.is_brace
                        && is_innermost
                        && Sequence::read(&glob[it.start as usize..i]).is_some()
                    {
                        it.next = Alternative::IS_SEQUENCE;
                    }

                    if let Some(it) = outer.pop() {
                        group = it;
                        group.is_plain = false;
                    }
                }
                b'*' if glob.get(i + 1) == Some(&b'*') => {
                    is_regular &=
                        matches!(glob.get(i + 2), None | Some(b'/')) && starts_segment(i, &group);
                    is_one_segment = false;
                    i += 1;
                }
                b'\\' => {
                    i += 1;
                    escaped = i;
                    is_regular &= glob.get(i).is_some_and(|it| *it != b'\\');
                    is_one_segment &= glob.get(i) != Some(&b'/');
                    if let Some(it) = glob.get(i) {
                        i += usize::from(wtf8_byte_sequence_length(*it)) - 1;
                    }
                }
                0x80.. => i += usize::from(wtf8_byte_sequence_length(ch)) - 1,
                b'[' if i < no_class_from => match class_shape(glob, i) {
                    Some((end, may_be_separator)) => {
                        is_regular &= glob[i + 1] != b'(';
                        has_separator_class |= may_be_separator;
                        i = end - 1;
                    }
                    None => {
                        no_class_from = i;
                        is_regular = false;
                    }
                },
                _ => {}
            }

            i += 1;
        }

        self.is_fixed = is_regular && depth == 0 && !has_star && found.is_empty();
        self.is_regular = is_regular && depth == 0 && !has_separator_class;
        self.has_group = has_group || has_choice && !self.is_regular;
        self.is_one_segment = is_one_segment;
        let first = glob.iter().position(|it| matches!(it, b'*' | b'?' | b'['));
        self.is_decided_at_once = !has_group
            && !has_choice
            && first.is_some_and(|it| matches!(&glob[it..], b"*" | b"**"));
        let mut group = 0;
        while let Some(it) = found.get(group)
            && it.close > 0
            && it.start as usize
                - if glob[it.start as usize - 1] == b'{' {
                    1
                } else {
                    2
                }
                == front_end
        {
            if it.choice == Choice::No || !self.is_regular {
                self.first_group = group as u32;
                break;
            }

            front_end = it.close as usize + 1;
            while let Some(&ch) = glob.get(front_end)
                && (IS_PLAIN[ch as usize] || ch == b'/')
            {
                front_end += 1;
            }

            while found
                .get(group)
                .is_some_and(|it| (it.start as usize) < front_end)
            {
                group += 1;
            }
        }

        self.front_end = front_end as u32;
    }

    #[inline(always)]
    fn take_front(&self, glob: &[u8], glob_start: u32, path: &[u8]) -> Option<u32> {
        let (mut i, mut path_index) = (glob_start, 0);
        while i < self.front_end {
            let wanted = glob[i as usize];
            if !IS_PLAIN[wanted as usize] && wanted != b'/' {
                let start = i + if wanted == b'{' { 1 } else { 2 };
                let first = self.group_at(start)?;
                (i, path_index) = take_choice(&self.alternatives, first, glob, path, path_index)?;
                continue;
            }

            let found = *path.get(path_index as usize)?;
            if wanted != found && !(wanted == b'/' && is_separator(found)) {
                return None;
            }

            i += 1;
            path_index += 1;
        }

        Some(path_index)
    }

    fn may_start_at(&self, first: u32, glob: &[u8], path: &[u8], path_index: u32) -> bool {
        let Some(it) = self.alternatives.get(first as usize) else {
            return true;
        };

        if it.next == Alternative::IS_SEQUENCE
            || glob[it.start as usize - 1] == b'('
                && !matches!(glob[it.start as usize - 2], b'@' | b'+')
        {
            return true;
        }

        let mut next = first as usize;
        while let Some(it) = self.alternatives.get(next) {
            if it.may_start_at(glob, path, path_index) && it.may_go_on_at(glob, path, path_index) {
                return true;
            }

            next = it.next as usize;
        }

        false
    }

    #[cold]
    #[inline(never)]
    fn may_pass_first_group(&self, glob: &[u8], path: &[u8], path_index: u32) -> bool {
        let Some(group) = self.alternatives.get(self.first_group as usize) else {
            return true;
        };

        if group.next == Alternative::IS_SEQUENCE
            || glob[group.start as usize - 1] == b'(' && glob[group.start as usize - 2] != b'@'
        {
            return true;
        }

        let literal_at = |text: &[u8]| text.iter().take_while(|it| IS_PLAIN[**it as usize]).count();
        let after = &glob[group.close as usize + 1..];
        let after = &after[..literal_at(after)];
        let mut next = self.first_group as usize;
        while let Some(it) = self.alternatives.get(next) {
            let text = &glob[it.start as usize..it.end as usize];
            let literal = literal_at(text);
            if path[path_index as usize..]
                .strip_prefix(&text[..literal])
                .is_some_and(|rest| literal < text.len() || rest.starts_with(after))
            {
                return true;
            }

            next = it.next as usize;
        }

        false
    }

    #[inline]
    fn group_at(&self, start: u32) -> Option<u32> {
        let at = self
            .alternatives
            .binary_search_by_key(&start, |it| it.start)
            .ok()?;
        (self.alternatives[at].close > 0).then_some(at as u32)
    }
}

#[inline(never)]
fn take_choice(
    alternatives: &[Alternative],
    first: u32,
    glob: &[u8],
    path: &[u8],
    path_index: u32,
) -> Option<(u32, u32)> {
    let group = alternatives[first as usize];
    let rest = &path[path_index as usize..];
    let taken = match group.choice {
        Choice::RestOfSegment => {
            let len = rest.iter().take_while(|it| !is_separator(**it));
            &rest[..len.count()]
        }
        Choice::RestOfPath => rest,
        _ => rest.get(..(group.end - group.start) as usize)?,
    };

    let mut next = first as usize;
    while let Some(it) = alternatives.get(next) {
        if *taken == glob[it.start as usize..it.end as usize] {
            return Some((group.close + 1, path_index + taken.len() as u32));
        }

        next = it.next as usize;
    }

    (taken.is_empty() && group.is_optional(glob)).then_some((group.close + 1, path_index))
}

#[inline(always)]
fn is_separator(c: u8) -> bool {
    c == b'/'
}

#[inline(always)]
fn unescape(c: &mut u8, glob: &[u8], glob_index: &mut u32) {
    if *c == b'\\' && *glob_index as usize + 1 < glob.len() {
        *glob_index += 1;

        *c = match glob[*glob_index as usize] {
            b'a' => b'\x61',
            b'b' => b'\x08',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            cc => cc,
        };
    }
}

#[inline(always)]
fn rune_len_at(bytes: &[u8], idx: usize) -> u8 {
    decode_rune_at(bytes, idx).1
}

#[inline(always)]
fn decode_rune_at(bytes: &[u8], idx: usize) -> (u32, u8) {
    let lead = bytes[idx];
    if lead < 0x80 {
        return (u32::from(lead), 1);
    }

    let sequence = &bytes[idx..bytes.len().min(idx + 4)];
    let chunk = sequence
        .utf8_chunks()
        .next()
        .expect("a sequence is never empty");
    match chunk.valid().chars().next() {
        Some(character) => (u32::from(character), character.len_utf8() as u8),
        None => (0xfffd, chunk.invalid().len() as u8),
    }
}

const fn wtf8_byte_sequence_length(first: u8) -> u8 {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[inline(always)]
fn get_unicode(c: &mut u32, clen: &mut u8, glob: &[u8], glob_index: &mut u32) -> bool {
    debug_assert!(*clen == 1);

    const BACKSLASH: u32 = b'\\' as u32;
    match *c {
        0x00..=0x5B | 0x5D..=0x7F => {
            return true;
        }
        BACKSLASH => {
            *glob_index += 1;
            if *glob_index as usize >= glob.len() {
                return false;
            }

            *c = match glob[*glob_index as usize] {
                b'a' => b'\x61' as u32,
                b'b' => b'\x08' as u32,
                b'n' => b'\n' as u32,
                b'r' => b'\r' as u32,
                b't' => b'\t' as u32,
                _ => 'brk: {
                    let (cp, len) = decode_rune_at(glob, *glob_index as usize);
                    *clen = len;
                    break 'brk cp;
                }
            };
        }
        _ => {
            let (cp, len) = decode_rune_at(glob, *glob_index as usize);
            *clen = len;
            *c = cp;
        }
    }

    true
}

#[inline(always)]
fn skip_globstars(glob: &[u8], glob_index: &mut u32) {
    *glob_index += 2;

    while *glob_index as usize + 4 <= glob.len()
        && &glob[*glob_index as usize..*glob_index as usize + 4] == b"/**/"
    {
        *glob_index += 3;
    }

    if *glob_index as usize + 3 == glob.len()
        && &glob[*glob_index as usize..*glob_index as usize + 3] == b"/**"
    {
        *glob_index += 3;
    }

    *glob_index -= 2;
}

#[derive(Copy, Clone, Default)]
struct Masks {
    bytes: u64,
    escapes: u64,
    separators: u64,
    digits: u64,
    anys: u64,
    classes: u64,
    loops: u64,
    skips: u64,
    trailing_skips: u64,
    not_at_end: u64,
    unsettled: u64,
    nots: u64,
    sequences: u64,
    repeats: u64,
    listed: u64,
    nots_before: u32,
    sequences_before: u32,
    repeats_before: u32,
    listed_before: u32,
}

#[derive(Copy, Clone)]
struct Nodes {
    word: u32,
    mask: u64,
}

impl Nodes {
    const IS_LAST: u32 = 1 << 31;

    #[inline(always)]
    fn word(self) -> usize {
        (self.word & !Nodes::IS_LAST) as usize
    }
}

struct Program {
    masks: Vec<Masks>,
    implied: Vec<u64>,
    lists: Vec<u32>,
    far: Vec<Nodes>,
    repeats: Vec<u32>,
    ends: Vec<Nodes>,
    sequences: Vec<Sequence>,
    start: u32,
}

struct Unfinished {
    kind: u8,
    starts_segment: bool,
    has_empty_alternative: bool,
    node: u32,
    any: u32,
    repeat: u32,
    ends: Nodes,
    earlier_ends: u32,
    alternative: u32,
    close: u32,
    outside: u32,
}

struct Reader<'p> {
    program: &'p mut Program,
    masks: Masks,
    word: usize,
    next_implies: Nodes,
    listed: usize,
}

impl Reader<'_> {
    #[inline(always)]
    fn at(&mut self, node: usize) -> u64 {
        if node / 64 != self.word {
            self.store(node / 64, false);
        }

        let bit = 1 << (node % 64);
        if self.next_implies.mask != 0 {
            self.masks.unsettled |= bit;
            if self.next_implies.word as usize == self.word && self.listed == self.program.far.len()
            {
                self.program.implied[node] = self.next_implies.mask;
            } else {
                self.list(bit);
            }

            self.next_implies.mask = 0;
        }

        bit
    }

    #[inline(never)]
    fn not_byte_at(&mut self, node: usize) -> u64 {
        let bit = self.at(node);
        self.masks.bytes |= bit;
        bit
    }

    #[cold]
    #[inline(never)]
    fn list(&mut self, bit: u64) {
        self.masks.listed |= bit;
        self.program.far.push(Nodes {
            word: self.next_implies.word | Nodes::IS_LAST,
            mask: self.next_implies.mask,
        });
        self.program.lists.push(self.listed as u32);
        self.listed = self.program.far.len();
    }

    #[cold]
    #[inline(never)]
    fn store(&mut self, word: usize, is_last: bool) {
        while self.word < word {
            let it = self.masks;
            self.program.masks.push(Masks {
                bytes: !it.bytes,
                ..it
            });
            if is_last && self.word + 1 == word {
                return;
            }

            self.program.implied.extend_from_slice(&[0; 64]);
            self.masks = Masks {
                nots_before: it.nots_before + it.nots.count_ones(),
                sequences_before: it.sequences_before + it.sequences.count_ones(),
                repeats_before: it.repeats_before + it.repeats.count_ones(),
                listed_before: it.listed_before + it.listed.count_ones(),
                ..Masks::default()
            };

            self.word += 1;
        }
    }

    #[inline(always)]
    fn imply(&mut self, nodes: Nodes) {
        if self.next_implies.mask == 0 {
            self.next_implies = nodes;
        } else if self.next_implies.word == nodes.word {
            self.next_implies.mask |= nodes.mask;
        } else {
            self.program.far.push(nodes);
        }
    }

    #[inline(never)]
    fn imply_node(&mut self, node: u32) {
        self.imply(Nodes {
            word: node / 64,
            mask: 1 << (node % 64),
        });
    }

    #[inline(never)]
    fn leave_out(&mut self, from: usize, to: usize) {
        if to - from == 1 && from / 64 == self.word {
            self.masks.bytes |= 1 << (from % 64);
        } else {
            self.leave_out_many(from, to);
        }
    }

    #[cold]
    #[inline(never)]
    fn leave_out_many(&mut self, mut from: usize, to: usize) {
        while from < to {
            self.store(from / 64, false);
            let end = to.min((from / 64 + 1) * 64);
            self.masks.bytes |= (!0 >> (64 - (end - from))) << (from % 64);
            from = end;
        }
    }

    #[cold]
    #[inline(never)]
    fn sequence(&mut self, bit: u64, written: &[u8], after: usize) {
        self.masks.bytes |= bit;
        let first = after - written.len() - 2;
        let Some(sequence) = Sequence::read(written) else {
            return self.leave_out_many(first, after);
        };

        self.masks.sequences |= bit;
        self.masks.unsettled |= bit;
        let digits = after - sequence.longest() as usize;
        self.program.sequences.push(sequence);
        self.leave_out_many(first + 1, digits);
        for digit in digits..after {
            self.imply_node(first as u32);
            let bit = self.at(digit);
            self.masks.bytes |= bit;
            self.masks.digits |= bit;
        }

        self.imply_node(first as u32);
    }
}

impl Program {
    #[inline(always)]
    fn new() -> Program {
        Program {
            masks: Vec::new(),
            implied: Vec::new(),
            lists: Vec::new(),
            far: Vec::new(),
            repeats: Vec::new(),
            ends: Vec::new(),
            sequences: Vec::new(),
            start: 0,
        }
    }

    #[inline(never)]
    fn read(&mut self, glob: &[u8], parsed: &Parsed, start: usize) {
        let alternatives = &*parsed.alternatives;
        let mut part_start = match start.checked_sub(1).map(|it| glob[it]) {
            Some(b'}' | b')') if parsed.is_regular => start - 1,
            _ => negations(glob),
        };

        self.start = start as u32;
        let mut i = start;
        self.implied.extend_from_slice(&[0; 64]);
        let mut to = Reader {
            program: self,
            masks: Masks::default(),
            word: 0,
            next_implies: Nodes { word: 0, mask: 0 },
            listed: 0,
        };

        let mut open: Vec<Unfinished> = Vec::new();
        let mut alternative_end = usize::MAX;
        let mut earlier_ends: Vec<Nodes> = Vec::new();
        let mut waiting: Vec<(u32, u32)> = Vec::new();
        let mut no_class_from = usize::MAX;
        let mut next_group = 0;
        let mut next_extra = (glob.len() - start + 1) as u32;

        macro_rules! node {
            ($at:expr $(, $mask:ident)*) => {{
                let node = $at - start;
                let _bit = to.not_byte_at(node);
                $(to.masks.$mask |= _bit;)*
                node as u32
            }};
        }

        loop {
            if i == alternative_end
                && let Some(group) = open.last_mut()
            {
                let it = alternatives[group.alternative as usize];
                let is_empty = it.start == it.end;
                if is_empty && std::mem::replace(&mut group.has_empty_alternative, true) {
                    to.leave_out(i - start, i - start + 1);
                } else {
                    let end = node!(i);
                    if group.ends.word != end / 64 {
                        if group.ends.mask != 0 {
                            earlier_ends.push(group.ends);
                        }

                        group.ends = Nodes {
                            word: end / 64,
                            mask: 0,
                        };
                    }

                    group.ends.mask |= 1 << (end % 64);
                }

                if i as u32 != group.close {
                    to.imply_node(group.any);
                    group.alternative = it.next;
                    alternative_end = alternatives[it.next as usize].end as usize;
                    i += 1;
                    part_start = i;
                    continue;
                }

                let earlier = &earlier_ends[group.earlier_ends as usize..];
                if matches!(group.kind, b'*' | b'+') {
                    to.program.repeats[group.repeat as usize] = to.program.ends.len() as u32;
                    to.program.ends.extend_from_slice(earlier);
                    to.program.ends.push(Nodes {
                        word: group.ends.word | Nodes::IS_LAST,
                        mask: group.ends.mask,
                    });
                }

                to.imply(group.ends);
                earlier.iter().for_each(|it| to.imply(*it));
                earlier_ends.truncate(group.earlier_ends as usize);
                match group.kind {
                    b'?' | b'*' => to.imply_node(group.node),
                    b'!' => to.imply_node(group.node + 1),
                    _ => {}
                }

                open.pop();
                alternative_end = open.last().map_or(usize::MAX, |it| {
                    alternatives[it.alternative as usize].end as usize
                });
                part_start = i;
                i += 1;
                continue;
            }

            let Some(&ch) = glob.get(i) else { break };
            if IS_PLAIN[ch as usize] {
                to.at(i - start);
                let end = alternative_end.min(glob.len());
                i += 1;
                while i < end && IS_PLAIN[glob[i] as usize] {
                    i += 1;
                }

                continue;
            }

            let starts_segment_at = |index: usize| match open.last() {
                Some(group) if index == part_start => group.starts_segment,
                _ => index <= part_start || glob[index - 1] == b'/',
            };

            let next = glob.get(i + 1).copied();
            if ch == b'{' || next == Some(b'(') && matches!(ch, b'*' | b'?' | b'+' | b'@' | b'!') {
                let first = i + if ch == b'{' { 1 } else { 2 };
                while alternatives
                    .get(next_group)
                    .is_some_and(|it| (it.start as usize) < first)
                {
                    next_group += 1;
                }

                if let Some(&it) = alternatives.get(next_group)
                    && it.start as usize == first
                    && it.close > 0
                {
                    if it.next == Alternative::IS_SEQUENCE {
                        let bit = to.at(i - start);
                        part_start = it.close as usize;
                        i = part_start + 1;
                        to.sequence(bit, &glob[first..part_start], i - start);
                        continue;
                    }

                    let starts_segment = starts_segment_at(i);
                    let node = node!(i);
                    let mut any = node;
                    match ch {
                        b'{' => {}
                        b'*' => {
                            to.imply_node(node);
                            any = node!(i + 1, repeats);
                        }
                        b'!' => {
                            let bit = 1 << (node % 64);
                            to.masks.unsettled |= bit;
                            to.masks.nots |= bit;
                            to.imply_node(node);
                            node!(i + 1, loops);
                            any = next_extra;
                            next_extra += 1;
                        }
                        _ => {
                            if ch == b'+' {
                                to.masks.repeats |= 1 << (node % 64);
                            }

                            to.leave_out(i + 1 - start, i + 2 - start);
                        }
                    }

                    if matches!(ch, b'*' | b'+') {
                        to.program.repeats.push(0);
                    }

                    to.imply_node(any);
                    let outside = match open.last() {
                        Some(outer) if alternative_end as u32 == it.close + 1 => outer.outside,
                        _ => it.close + 1,
                    };

                    open.push(Unfinished {
                        kind: ch,
                        starts_segment,
                        has_empty_alternative: false,
                        node,
                        any,
                        repeat: (to.program.repeats.len() as u32).wrapping_sub(1),
                        ends: Nodes { word: 0, mask: 0 },
                        earlier_ends: earlier_ends.len() as u32,
                        alternative: next_group as u32,
                        close: it.close,
                        outside,
                    });
                    alternative_end = it.end as usize;
                    i = first;
                    part_start = first;
                    continue;
                }
            }

            match ch {
                b'*' if next == Some(b'*') && glob.get(i + 2) != Some(&b'(') => {
                    let mut at = i as u32;
                    skip_globstars(glob, &mut at);
                    let at = at as usize;
                    let after = at + 2;
                    let starts_segment = !open.is_empty() && starts_segment_at(at);
                    let outside = match open.last() {
                        Some(group) if alternative_end == after => group.outside as usize,
                        _ => after,
                    };

                    if at > i {
                        let first = node!(i);
                        to.leave_out(i + 1 - start, at - start);
                        to.imply_node(first);
                    }

                    i = after;
                    if starts_segment && outside > after && glob.get(outside) == Some(&b'/') {
                        let node = node!(at);
                        waiting.push((outside as u32, node));
                        to.imply_node(node);
                        node!(at + 1, skips);
                        continue;
                    }

                    let is_last = after == glob.len() || starts_segment && outside == glob.len();
                    if (after.saturating_sub(part_start) < 3 || glob[after - 3] == b'/')
                        && (is_last || glob[after] == b'/')
                    {
                        let node = match !is_last && &glob[after..] == b"/*" {
                            true => node!(at, not_at_end),
                            false => node!(at),
                        };

                        to.imply_node(node);
                        match is_last {
                            true => node!(at + 1, skips, trailing_skips),
                            false => {
                                i += 1;
                                node!(at + 1, skips);
                                node!(at + 2)
                            }
                        };

                        to.imply_node(node);
                    } else {
                        let node = node!(at, loops);
                        to.leave_out(at + 1 - start, at + 2 - start);
                        to.imply_node(node);
                    }
                }
                b'*' => {
                    let node = node!(i, loops);
                    to.imply_node(node);
                    i += 1;
                }
                b'?' => {
                    node!(i, anys);
                    i += 1;
                }

                b'[' if i < no_class_from
                    && let Some((end, _)) = class_shape(glob, i) =>
                {
                    node!(i, classes);
                    to.leave_out(i + 1 - start, end - start);
                    to.imply_node((i + 1 - start) as u32);
                    i = end;
                }
                b'/' => {
                    node!(i, separators);
                    while let Some(it) = waiting.last().filter(|it| it.0 as usize == i) {
                        to.imply_node(it.1);
                        waiting.pop();
                    }

                    i += 1;
                }
                b'\\' if i + 1 < glob.len() => {
                    let len = usize::from(wtf8_byte_sequence_length(glob[i + 1]));
                    if i + 1 + len > glob.len() {
                        to.at(i - start);
                        to.leave_out(i - start, glob.len() - start);
                    } else {
                        match glob[i + 1] == b'/' {
                            true => node!(i, separators),
                            false => node!(i, escapes),
                        };

                        to.leave_out(i + 1 - start, i + 2 - start);
                        to.imply_node((i + 1 - start) as u32);
                        if len > 1 {
                            to.at(i + 2 - start);
                        }
                    }

                    i += 1 + len;
                }
                _ => {
                    if ch == b'[' {
                        no_class_from = no_class_from.min(i);
                    }

                    to.at(i - start);
                    let end = i + usize::from(wtf8_byte_sequence_length(ch));
                    if end > glob.len() {
                        to.leave_out(i - start, glob.len() - start);
                    }

                    i = end;
                }
            }
        }

        debug_assert!(open.is_empty() && waiting.is_empty());
        let end = glob.len() - start;
        to.at(end);
        let nodes = next_extra as usize;
        to.leave_out_many(end, nodes.next_multiple_of(64));
        to.store(nodes.div_ceil(64), true);
    }

    #[inline(always)]
    fn on<'a>(&'a self, glob: &'a [u8], path: &'a [u8], first: usize) -> On<'a> {
        On {
            program: self,
            masks: &self.masks,
            implied: &self.implied,
            pattern: &glob[self.start as usize..],
            path,
            first,
        }
    }

    const IS_LONG: usize = 512;
}

struct On<'a> {
    program: &'a Program,
    masks: &'a [Masks],
    implied: &'a [u64],
    pattern: &'a [u8],
    path: &'a [u8],
    first: usize,
}

impl On<'_> {
    #[inline(always)]
    fn walk(&self) -> bool {
        match self.masks.len() {
            1 => Walk::<[u64; 1]>::go(self),
            _ => Walk::<Vec<u64>>::go(self),
        }
    }

    #[cold]
    #[inline(never)]
    fn takes(&self, node: usize, byte: u8, character: u32) -> bool {
        match self.pattern[node] {
            b'\\' => {
                let (mut wanted, mut at) = (b'\\', node as u32);
                unescape(&mut wanted, self.pattern, &mut at);
                wanted == byte
            }
            b'[' => class_has(self.pattern, node as u32, character) == Some(true),
            _ => matches!(byte, b'0'..=b'9' | b'-' | b'A'..=b'z'),
        }
    }

    #[cold]
    #[inline(never)]
    fn has_sequence(&self, index: usize, set: &[u64], node: usize, number: u32) -> bool {
        let has = |node: usize| set[node / 64] & 1 << (node % 64) != 0;
        let sequence = &self.program.sequences[number as usize];
        let close = self.pattern[node..]
            .iter()
            .position(|it| *it == b'}')
            .map_or(node, |it| node + it);
        let rest = &self.path[index..];
        sequence.has_nothing() && has(close + 1)
            || (1..=rest.len().min(sequence.longest() as usize))
                .any(|len| has(close + 1 - len) && sequence.has(&rest[..len]))
    }

    #[cold]
    #[inline(never)]
    fn add(&self, list: &[Nodes], is_at_end: bool, set: &mut [u64]) {
        for it in list {
            let masks = &self.masks[it.word()];
            let mut mask = it.mask;
            if is_at_end {
                mask &= !masks.not_at_end;
            }

            set[it.word()] |= mask;
            if it.word & Nodes::IS_LAST != 0 {
                break;
            }
        }
    }
}

trait Set {
    fn empty(words: usize) -> Self;

    fn words(&self) -> &[u64];

    fn words_mut(&mut self) -> &mut [u64];

    fn masks<'a>(on: &On<'a>) -> &'a [Masks];

    #[inline(always)]
    fn take_simple_steps(&mut self, _on: &On<'_>, _index: &mut usize) -> bool {
        true
    }
}

#[inline(always)]
fn take_simple_steps(on: &On<'_>, set: &mut u64, index: &mut usize) -> bool {
    let (masks, Ok(implied)) = (&on.masks[0], <&[u64; 64]>::try_from(on.implied)) else {
        return true;
    };

    let mut pattern = [0; 64];
    let len = on.pattern.len().min(64);
    pattern[..len].copy_from_slice(&on.pattern[..len]);
    let not_simple = masks.escapes | masks.digits | masks.classes;
    let not_simply_settled = masks.nots | masks.sequences | masks.listed;
    'steps: while *index > on.first {
        let byte = on.path[*index - 1];
        let before = *set >> 1;
        if byte >= 128 || before & not_simple != 0 {
            break;
        }

        let mut now = match byte == b'/' {
            true => before & masks.separators | (*set << 1) & masks.skips,
            false => before & masks.anys | *set & (masks.skips | masks.loops),
        };

        let mut bytes = before & masks.bytes;
        while bytes != 0 {
            let node = bytes.trailing_zeros();
            if pattern[node as usize % 64] == byte {
                now |= 1 << node;
            }

            bytes &= bytes - 1;
        }

        let mut todo = now & masks.unsettled;
        while todo != 0 {
            let node = 63 - todo.leading_zeros();
            if not_simply_settled & 1 << node != 0 {
                break 'steps;
            }

            todo ^= 1 << node;
            now |= implied[node as usize % 64];
            todo |= implied[node as usize % 64] & masks.unsettled;
        }

        *set = now;
        *index -= 1;
        if now == 0 {
            return false;
        }
    }

    true
}

impl Set for [u64; 1] {
    #[inline(always)]
    fn empty(_: usize) -> Self {
        [0]
    }

    #[inline(always)]
    fn words(&self) -> &[u64] {
        self
    }

    #[inline(always)]
    fn words_mut(&mut self) -> &mut [u64] {
        self
    }

    #[inline(always)]
    fn masks<'a>(on: &On<'a>) -> &'a [Masks] {
        &on.masks[..1]
    }

    #[inline(always)]
    fn take_simple_steps(&mut self, on: &On<'_>, index: &mut usize) -> bool {
        take_simple_steps(on, &mut self[0], index)
    }
}

impl Set for Vec<u64> {
    fn empty(words: usize) -> Self {
        vec![0; words]
    }

    #[inline(always)]
    fn words(&self) -> &[u64] {
        self
    }

    #[inline(always)]
    fn words_mut(&mut self) -> &mut [u64] {
        self
    }

    #[inline(always)]
    fn masks<'a>(on: &On<'a>) -> &'a [Masks] {
        on.masks
    }
}

struct Walk<S: Set> {
    sets: [S; 5],
    idle: S,
    index: usize,
}

impl<S: Set> Walk<S> {
    #[inline(always)]
    fn take_byte(on: &On<'_>, byte: u8, set: &mut [u64]) {
        let is_separator = byte == b'/';
        let mut below = 0;
        for (word, masks) in S::masks(on).iter().enumerate() {
            let after = set[word];
            let before = after >> 1 | set.get(word + 1).map_or(0, |it| it << 63);
            if before | after | below == 0 {
                continue;
            }

            let mut found = match is_separator {
                true => before & masks.separators | (after << 1 | below) & masks.skips,
                false => before & masks.anys | after & (masks.skips | masks.loops),
            };

            below = after >> 63;
            let mut bytes = before & masks.bytes;
            while bytes != 0 {
                let node = bytes.trailing_zeros() as usize;
                if on.pattern[word * 64 + node] == byte {
                    found |= 1 << node;
                }

                bytes &= bytes - 1;
            }

            let mut others = before & (masks.escapes | masks.digits | masks.classes);
            while others != 0 {
                let node = others.trailing_zeros() as usize;
                if on.takes(word * 64 + node, byte, u32::from(byte)) {
                    found |= 1 << node;
                }

                others &= others - 1;
            }

            set[word] = found;
        }
    }

    #[cold]
    #[inline(never)]
    fn take_character(&mut self, on: &On<'_>) {
        self.sets.rotate_right(1);
        let byte = on.path[self.index];
        let (character, len) = decode_rune_at(on.path, self.index);
        let [now, later @ ..] = &mut self.sets;
        let (after_byte, after_character) = (later[0].words(), later[len as usize - 1].words());
        for (word, masks) in S::masks(on).iter().enumerate() {
            let before = |set: &[u64]| set[word] >> 1 | set.get(word + 1).map_or(0, |it| it << 63);
            let mut found = before(after_character) & masks.anys
                | after_byte[word] & masks.skips
                | after_character[word] & masks.loops;
            let mut others = before(after_byte) & (masks.bytes | masks.escapes)
                | before(after_character) & masks.classes;
            while others != 0 {
                let node = others.trailing_zeros() as usize;
                if match masks.bytes & 1 << node != 0 {
                    true => on.pattern[word * 64 + node] == byte,
                    false => on.takes(word * 64 + node, byte, character),
                } {
                    found |= 1 << node;
                }

                others &= others - 1;
            }

            now.words_mut()[word] = found;
        }
    }

    #[inline(always)]
    fn settle(on: &On<'_>, index: usize, set: &mut [u64]) {
        let is_at_end = index == on.path.len();
        for (word, masks) in S::masks(on).iter().enumerate().rev() {
            if set[word] == 0 {
                continue;
            }

            let mut allowed = !0;
            if is_at_end {
                allowed &= !masks.not_at_end;
            }

            let mut now = set[word];
            let mut todo = now & masks.unsettled;
            while todo != 0 {
                let at = 63 - todo.leading_zeros() as usize;
                let bit = 1 << at;
                todo ^= bit;
                if (masks.nots | masks.sequences | masks.listed) & bit != 0 {
                    set[word] = now;
                    let before = |mask: u64| (mask & (bit - 1)).count_ones();
                    if masks.nots & bit != 0 {
                        let any = on.pattern.len()
                            + 1
                            + (masks.nots_before + before(masks.nots)) as usize;
                        if set[any / 64] & 1 << (any % 64) != 0 {
                            now ^= bit;
                            continue;
                        }
                    } else if masks.sequences & bit != 0 {
                        let number = masks.sequences_before + before(masks.sequences);
                        if !on.has_sequence(index, set, word * 64 + at, number) {
                            now ^= bit;
                            continue;
                        }
                    }

                    if masks.listed & bit != 0 {
                        let list = (masks.listed_before + before(masks.listed)) as usize;
                        let list = &on.program.far[on.program.lists[list] as usize..];
                        on.add(list, is_at_end, set);
                        now = set[word];
                        todo |= now & masks.unsettled & (bit - 1);
                    }
                }

                let implied = on.implied[word * 64 + at] & allowed;
                now |= implied;
                todo |= implied & masks.unsettled;
            }

            set[word] = now;
        }
    }

    #[inline(never)]
    fn settle_idle(&mut self, on: &On<'_>) {
        self.idle.words_mut().copy_from_slice(self.sets[0].words());
        Self::settle(on, self.index, self.idle.words_mut());
        for (word, masks) in S::masks(on).iter().enumerate() {
            let mut repeated = self.idle.words()[word] & masks.repeats;
            while repeated != 0 {
                let bit = repeated.isolate_lowest_one();
                repeated ^= bit;
                let number = masks.repeats_before + (masks.repeats & (bit - 1)).count_ones();
                let list = &on.program.ends[on.program.repeats[number as usize] as usize..];
                on.add(list, false, self.sets[0].words_mut());
            }
        }
    }

    #[inline(never)]
    fn go(on: &On<'_>) -> bool {
        let mut walk = Walk::<S> {
            sets: std::array::from_fn(|_| S::empty(on.masks.len())),
            idle: S::empty(on.masks.len()),
            index: on.path.len() + 1,
        };

        let has_repeats = !on.program.repeats.is_empty();
        while walk.index > on.first {
            if !has_repeats && walk.index <= on.path.len() {
                if !walk.sets[0].take_simple_steps(on, &mut walk.index) {
                    return false;
                }

                if walk.index == on.first {
                    break;
                }
            }

            walk.index -= 1;
            let byte = on.path.get(walk.index).copied();
            match byte {
                Some(byte @ ..128) => {
                    Self::take_byte(on, byte, walk.sets[0].words_mut());
                }
                Some(_) => walk.take_character(on),
                None => {
                    let end = on.pattern.len();
                    walk.sets[0].words_mut()[end / 64] |= 1 << (end % 64);
                }
            }

            if has_repeats {
                walk.settle_idle(on);
            }

            Self::settle(on, walk.index, walk.sets[0].words_mut());
            if byte.is_none() {
                let mut carry = 0;
                for (set, masks) in walk.sets[0].words_mut().iter_mut().zip(S::masks(on)) {
                    let globstars = *set << 1 | carry;
                    carry = *set >> 63;
                    *set |= globstars & masks.trailing_skips;
                }
            }

            let starts_character = !matches!(byte, Some(0x80..=0xBF));
            let sets = if starts_character { 1 } else { 4 };
            if walk.sets[..sets]
                .iter()
                .all(|it| it.words().iter().all(|it| *it == 0))
            {
                return false;
            }
        }

        walk.sets[0].words()[0] & 1 != 0
    }
}

#[cfg(test)]
#[path = "bun_glob_tests.rs"]
mod tests;
