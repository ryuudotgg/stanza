use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::rc::Rc;

use crate::paths::{basename, dirname, join};

struct Listing {
    names: HashSet<String>,
    folded: HashSet<String>,
}

#[derive(Default)]
struct Directories {
    real: HashMap<String, String>,
    listings: HashMap<String, Option<Rc<Listing>>>,
    existence: HashMap<String, HashMap<&'static str, bool>>,
}

thread_local! {
    static DIRECTORIES: RefCell<Directories> = RefCell::default();
}

pub fn real_directory(dir: &str) -> io::Result<String> {
    if let Some(cached) = DIRECTORIES.with_borrow(|directories| directories.real.get(dir).cloned())
    {
        return Ok(cached);
    }

    let parent = dirname(dir);
    let real = if fs::metadata(dir).is_ok() {
        fs::canonicalize(dir)?.to_string_lossy().into_owned()
    } else if parent == dir {
        dir.to_owned()
    } else {
        join(&real_directory(&parent)?, basename(dir))
    };

    DIRECTORIES.with_borrow_mut(|directories| {
        directories.real.insert(dir.to_owned(), real.clone());
        directories.real.insert(real.clone(), real.clone());
    });

    Ok(real)
}

fn listing(dir: &str) -> Option<Rc<Listing>> {
    if let Some(cached) =
        DIRECTORIES.with_borrow(|directories| directories.listings.get(dir).cloned())
    {
        return cached;
    }

    let listing = fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<io::Result<HashSet<String>>>()
        })
        .ok()
        .map(|names| {
            let folded = names.iter().map(|name| name.to_lowercase()).collect();
            Rc::new(Listing { names, folded })
        });

    DIRECTORIES.with_borrow_mut(|directories| {
        directories.listings.insert(dir.to_owned(), listing.clone());
    });

    listing
}

pub fn holds(dir: &str, name: &'static str) -> bool {
    if let Some(cached) = DIRECTORIES.with_borrow(|directories| {
        directories
            .existence
            .get(dir)
            .and_then(|names| names.get(name))
            .copied()
    }) {
        return cached;
    }

    let exists = listing(dir).is_none_or(|listing| listing.folded.contains(&name.to_lowercase()))
        && fs::metadata(join(dir, name)).is_ok();

    DIRECTORIES.with_borrow_mut(|directories| match directories.existence.get_mut(dir) {
        Some(names) => {
            names.insert(name, exists);
        }
        None => {
            directories
                .existence
                .insert(dir.to_owned(), HashMap::from([(name, exists)]));
        }
    });

    exists
}

pub fn listed(dir: &str, name: &str) -> bool {
    listing(dir).is_some_and(|listing| listing.names.contains(name))
}
