use std::collections::VecDeque;

#[derive(Default)]
struct Node {
    children: Vec<(u8, usize)>,
    fail: usize,
    depth: usize,
    ends: bool,
    shorter: Option<usize>,
}

pub(super) struct Codes(Vec<Node>);

impl Codes {
    pub(super) fn new() -> Self {
        Self(vec![Node::default()])
    }

    pub(super) fn insert(&mut self, code: &str, budget: &mut usize) -> Option<usize> {
        let mut at = 0;
        for &byte in code.as_bytes() {
            at = match self.child(at, byte) {
                Some(child) => child,
                None => {
                    *budget = budget.checked_sub(1)?;
                    let child = self.0.len();
                    let depth = self.0[at].depth + 1;
                    let children = &mut self.0[at].children;
                    let slot = children.partition_point(|&(other, _)| other < byte);
                    children.insert(slot, (byte, child));
                    self.0.push(Node {
                        depth,
                        ..Node::default()
                    });
                    child
                }
            };
        }

        self.0[at].ends = true;
        Some(at)
    }

    pub(super) fn link(&mut self) {
        let mut queue = VecDeque::from([0]);
        while let Some(parent) = queue.pop_front() {
            for index in 0..self.0[parent].children.len() {
                let (byte, child) = self.0[parent].children[index];
                let fail = match parent {
                    0 => 0,
                    _ => self.step(self.0[parent].fail, byte),
                };

                let shorter = match self.0[fail].ends && fail != 0 {
                    true => Some(fail),
                    false => self.0[fail].shorter,
                };

                self.0[child].fail = fail;
                self.0[child].shorter = shorter;
                queue.push_back(child);
            }
        }
    }

    fn child(&self, at: usize, byte: u8) -> Option<usize> {
        let children = &self.0[at].children;
        let found = children
            .binary_search_by_key(&byte, |&(byte, _)| byte)
            .ok()?;
        Some(children[found].1)
    }

    pub(super) fn step(&self, mut at: usize, byte: u8) -> usize {
        loop {
            if let Some(child) = self.child(at, byte) {
                return child;
            }

            if at == 0 {
                return 0;
            }

            at = self.0[at].fail;
        }
    }

    pub(super) fn matches(&self, at: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let longest = match self.0[at].ends && at != 0 {
            true => Some(at),
            false => self.0[at].shorter,
        };

        std::iter::successors(longest, |&at| self.0[at].shorter).map(|at| (at, self.0[at].depth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ending(codes: &[&'static str], text: &str) -> Vec<Vec<&'static str>> {
        let mut automaton = Codes::new();
        let mut names = std::collections::HashMap::new();
        let mut budget = usize::MAX;
        for &code in codes {
            names.insert(automaton.insert(code, &mut budget).unwrap(), code);
        }

        automaton.link();
        let mut state = 0;
        text.bytes()
            .map(|byte| {
                state = automaton.step(state, byte);
                automaton
                    .matches(state)
                    .map(|(node, _)| names[&node])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn every_code_ending_at_a_byte_is_reported_longest_first() {
        let codes = ["he", "she", "his", "hers", "e"];
        assert_eq!(
            ending(&codes, "ushers"),
            [
                vec![],
                vec![],
                vec![],
                vec!["she", "he", "e"],
                vec![],
                vec!["hers"],
            ]
        );
    }

    #[test]
    fn codes_overlapping_themselves_match_at_every_repeat() {
        assert_eq!(
            ending(&["aa", "a"], "aaa"),
            [vec!["a"], vec!["aa", "a"], vec!["aa", "a"]]
        );
    }

    #[test]
    fn the_empty_code_never_matches() {
        assert_eq!(ending(&["", "b"], "ab"), [vec![], vec!["b"]]);
    }

    #[test]
    fn a_spent_budget_stops_an_insert() {
        let mut automaton = Codes::new();
        let mut budget = 3;
        assert!(automaton.insert("abc", &mut budget).is_some());
        assert_eq!(budget, 0);
        assert!(automaton.insert("abd", &mut budget).is_none());
        assert!(automaton.insert("ab", &mut budget).is_some());
    }
}
