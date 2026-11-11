use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, FileType};
use std::io;
use std::rc::Rc;

use crate::paths::{basename, dirname, join};

struct Listing {
    kinds: HashMap<String, Option<FileType>>,
    typed: bool,
    folded: OnceCell<HashSet<String>>,
    searchable: OnceCell<bool>,
}

impl Listing {
    fn folded(&self) -> &HashSet<String> {
        self.folded
            .get_or_init(|| self.kinds.keys().map(|name| name.to_lowercase()).collect())
    }

    // read_dir needs only read permission, so a listed name can still fail lstat with EACCES.
    fn searchable(&self, dir: &str) -> bool {
        *self.searchable.get_or_init(|| {
            CString::new(dir).is_ok_and(|text| unsafe {
                libc::faccessat(libc::AT_FDCWD, text.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0
            })
        })
    }
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
    let real = if parent != dir && listed_directory(&parent, basename(dir)) {
        join(&real_directory(&parent)?, basename(dir))
    } else if fs::metadata(dir).is_ok() {
        crate::files::realpath(dir)?
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

fn read_listing(dir: &str, typed: bool) -> Option<Rc<Listing>> {
    let entries = fs::read_dir(dir).ok()?;
    let mut kinds = HashMap::new();
    for entry in entries {
        let entry = entry.ok()?;
        match entry.file_name().into_string() {
            Ok(name) => {
                let kind = if typed { entry.file_type().ok() } else { None };
                kinds.entry(name).or_insert(kind);
            }
            Err(name) => {
                kinds.insert(name.to_string_lossy().into_owned(), None);
            }
        }
    }

    Some(Rc::new(Listing {
        kinds,
        typed,
        folded: OnceCell::new(),
        searchable: OnceCell::new(),
    }))
}

fn listing(dir: &str, typed: bool) -> Option<Rc<Listing>> {
    let cached = DIRECTORIES.with_borrow(|directories| directories.listings.get(dir).cloned());
    if let Some(cached) = cached
        && cached
            .as_ref()
            .is_none_or(|listing| listing.typed || !typed)
    {
        return cached;
    }

    let listing = read_listing(dir, typed);
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

    let exists = listing(dir, false)
        .is_none_or(|listing| listing.folded().contains(&name.to_lowercase()))
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

// Only a listing already taken: calling listing() here would read_dir every ancestor up to the root.
fn listed_directory(parent: &str, name: &str) -> bool {
    DIRECTORIES.with_borrow(|directories| {
        directories
            .listings
            .get(parent)
            .and_then(Option::as_ref)
            .and_then(|listing| *listing.kinds.get(name)?)
            .is_some_and(|kind| kind.is_dir())
    })
}

pub fn listed(dir: &str, name: &str) -> bool {
    listing(dir, false).is_some_and(|listing| listing.kinds.contains_key(name))
}

// A symlink in dir would let lstat's expansion pass PATH_MAX where this length check cannot see it.
pub fn listed_file(dir: &str, name: &str) -> Option<bool> {
    let listing = listing(dir, true)?;
    let kind = (*listing.kinds.get(name)?)?;
    let fits = dir.len() + 1 + name.len() < libc::PATH_MAX as usize;
    let unlinked = real_directory(dir).is_ok_and(|real| real == dir);
    (fits && unlinked && listing.searchable(dir)).then(|| kind.is_file())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> String {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("stanza-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn derived_real_directories_match_canonicalize() {
        let root = scratch("derived-real");
        fs::create_dir_all(join(&root, "Real/sub/deep")).unwrap();
        fs::write(join(&root, "Real/sub/a.ts"), "").unwrap();
        std::os::unix::fs::symlink(join(&root, "Real"), join(&root, "link")).unwrap();
        let folded = fs::metadata(join(&root, "real")).is_ok();

        let mut dirs = vec![
            "Real",
            "Real/sub",
            "Real/sub/deep",
            "link",
            "link/sub",
            "link/sub/deep",
        ];
        if folded {
            dirs.extend(["real", "real/sub", "REAL/SUB/deep"]);
        }

        for dir in [root.as_str()].into_iter().chain(dirs.iter().copied()) {
            let path = join(&root, dir);
            let _ = listed_file(&path, "a.ts");
        }

        for dir in dirs {
            let path = join(&root, dir);
            let expected = fs::canonicalize(&path)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(real_directory(&path).unwrap(), expected, "{dir}");
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_symlinked_directory_leaves_the_file_test_to_lstat() {
        let root = scratch("linked-file-test");
        fs::create_dir(join(&root, "real")).unwrap();
        fs::write(join(&root, "real/a.ts"), "").unwrap();
        std::os::unix::fs::symlink(join(&root, "real"), join(&root, "link")).unwrap();

        assert_eq!(listed_file(&join(&root, "real"), "a.ts"), Some(true));
        assert_eq!(listed_file(&join(&root, "link"), "a.ts"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_has_no_listed_type() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let root = scratch("lossy-name");
        let lossy = PathBuf::from(&root).join(OsStr::from_bytes(b"bad\xff.ts"));
        fs::write(lossy, "").unwrap();

        assert_eq!(listed_file(&root, "bad\u{fffd}.ts"), None);
        fs::remove_dir_all(root).unwrap();
    }
}
