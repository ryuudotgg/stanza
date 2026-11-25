use std::collections::HashSet;
use std::rc::Rc;

use crate::paths::{basename, dirname};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    On,
    Off,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    All,
    None,
    Some,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Name {
    File(String),
    Extension(String),
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub dir: String,
    pub name: Name,
}

impl Target {
    pub fn file(path: &str) -> Self {
        Self {
            dir: dirname(path).to_owned(),
            name: Name::File(basename(path).to_owned()),
        }
    }

    pub fn files_in(dir: &str, extension: Option<&str>) -> Self {
        Self {
            dir: dir.to_owned(),
            name: extension.map_or(Name::Any, |extension| Name::Extension(extension.to_owned())),
        }
    }
}

pub type ReachAt = Rc<dyn Fn(&Target) -> Reach>;

#[derive(Clone)]
pub enum Coverage {
    Fixed(Reach),
    At(ReachAt),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Rank {
    RootPreset,
    GroupPreset,
    Explicit,
    Unread,
}

pub struct Layer {
    pub reach: Coverage,
    pub setting: Setting,
    pub rank: Rank,
    pub overriding: bool,
}

pub type Layers = Vec<Rc<Layer>>;

pub fn layer(setting: Setting) -> Rc<Layer> {
    ranked(setting, Rank::Explicit)
}

pub fn ranked(setting: Setting, rank: Rank) -> Rc<Layer> {
    Rc::new(Layer {
        reach: Coverage::Fixed(Reach::All),
        setting,
        rank,
        overriding: false,
    })
}

pub fn last_layers(layers: Layers) -> Layers {
    let mut seen: HashSet<*const Layer> = HashSet::new();
    let mut result: Layers = Vec::new();
    for layer in layers.into_iter().rev() {
        if seen.insert(Rc::as_ptr(&layer)) {
            result.push(layer);
        }
    }

    result.reverse();
    result
}

pub fn reach_at(coverage: &Coverage, target: &Target) -> Reach {
    match coverage {
        Coverage::Fixed(reach) => *reach,
        Coverage::At(at) => at(target),
    }
}

const fn intersect(left: Reach, right: Reach) -> Reach {
    match (left, right) {
        (Reach::None, _) | (_, Reach::None) => Reach::None,
        (Reach::All, other) | (other, Reach::All) => other,
        _ => Reach::Some,
    }
}

pub fn intersect_coverage(left: &Coverage, right: &Coverage) -> Coverage {
    match (left, right) {
        (Coverage::Fixed(Reach::All), other) | (other, Coverage::Fixed(Reach::All)) => {
            other.clone()
        }
        (Coverage::Fixed(Reach::None), _) | (_, Coverage::Fixed(Reach::None)) => {
            Coverage::Fixed(Reach::None)
        }
        _ => {
            let (left, right) = (left.clone(), right.clone());
            Coverage::At(Rc::new(move |target| {
                intersect(reach_at(&left, target), reach_at(&right, target))
            }))
        }
    }
}

pub fn scoped(layers: &[Rc<Layer>], reach: &Coverage) -> Layers {
    layers
        .iter()
        .map(|layer| {
            let reach = if layer.rank == Rank::Unread {
                layer.reach.clone()
            } else {
                intersect_coverage(&layer.reach, reach)
            };

            Rc::new(Layer {
                reach,
                setting: layer.setting,
                rank: layer.rank,
                overriding: layer.overriding,
            })
        })
        .collect()
}

pub(super) fn overriding(layer: &Layer) -> Rc<Layer> {
    Rc::new(Layer {
        reach: layer.reach.clone(),
        setting: layer.setting,
        rank: layer.rank,
        overriding: true,
    })
}

pub(super) fn fold(layers: &[Rc<Layer>], target: &Target) -> Option<Setting> {
    let mut chosen: Option<&Layer> = None;
    let top = layers.iter().filter(|layer| !layer.overriding);
    let late = layers.iter().filter(|layer| layer.overriding);
    for layer in top.chain(late) {
        let reach = reach_at(&layer.reach, target);
        let applies =
            reach == Reach::All || (reach == Reach::Some && layer.setting != Setting::Off);

        if applies && chosen.is_none_or(|chosen| layer.rank >= chosen.rank) {
            chosen = Some(layer);
        }
    }

    chosen.map(|layer| layer.setting)
}

pub(super) fn excluding(reach: Coverage, excluded: Coverage) -> Coverage {
    Coverage::At(Rc::new(move |target| {
        match (reach_at(&reach, target), reach_at(&excluded, target)) {
            (Reach::None, _) | (_, Reach::All) => Reach::None,
            (included, Reach::None) => included,
            (_, Reach::Some) => Reach::Some,
        }
    }))
}
