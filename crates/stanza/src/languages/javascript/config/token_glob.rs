pub(super) const ALTERNATIVE_LIMIT: usize = 256;
pub(super) const EXPANDED_LIMIT: usize = 65_536;
pub(super) const STATE_LIMIT: usize = 4_194_304;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Units {
    Utf16,
    Bytes,
}
pub(super) fn encode(text: &str, units: Units) -> Vec<u16> {
    match units {
        Units::Utf16 => text.encode_utf16().collect(),
        Units::Bytes => text.bytes().map(u16::from).collect(),
    }
}

pub(super) enum Atom {
    Unit(u16),
    One,
    Class(Vec<(u16, u16)>),
    Star,
}

impl Atom {
    pub(super) fn accepts(&self, unit: u16) -> bool {
        match self {
            Self::Unit(expected) => *expected == unit,
            Self::One => true,
            Self::Class(ranges) => ranges
                .iter()
                .any(|&(first, last)| (first..=last).contains(&unit)),
            Self::Star => unreachable!("wildcard steps over stars"),
        }
    }

    pub(super) fn cost(&self) -> usize {
        match self {
            Self::Class(ranges) => ranges.len(),
            _ => 1,
        }
    }
}

pub(super) enum Segment {
    Any,
    Atoms(Vec<Atom>),
}

pub(super) fn reached(segments: &[Segment], path: &[Vec<u16>]) -> Vec<bool> {
    let mut reachable = vec![false; path.len() + 1];
    reachable[0] = true;
    for segment in segments {
        let mut next = vec![false; path.len() + 1];
        for at in 0..=path.len() {
            if !reachable[at] {
                continue;
            }

            match segment {
                Segment::Any => {
                    next[at..].fill(true);
                    break;
                }
                Segment::Atoms(atoms) => {
                    if at < path.len() && wildcard(atoms, &path[at]) {
                        next[at + 1] = true;
                    }
                }
            }
        }

        reachable = next;
    }

    reachable
}

pub(super) fn wildcard(atoms: &[Atom], text: &[u16]) -> bool {
    let (mut at, mut from) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while from < text.len() {
        match atoms.get(at) {
            Some(Atom::Star) => {
                star = Some((at, from));
                at += 1;
            }
            Some(atom) if atom.accepts(text[from]) => {
                at += 1;
                from += 1;
            }
            _ => {
                let Some((star_at, star_from)) = star else {
                    return false;
                };

                at = star_at + 1;
                from = star_from + 1;
                star = Some((star_at, star_from + 1));
            }
        }
    }

    atoms[at..].iter().all(|atom| matches!(atom, Atom::Star))
}
