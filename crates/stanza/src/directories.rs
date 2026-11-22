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

struct Unlisted {
    asked: u64,
    entries: u64,
}

#[derive(Default)]
struct Directories {
    real: HashMap<String, String>,
    listings: HashMap<String, Option<Rc<Listing>>>,
    unlisted: HashMap<String, Option<Unlisted>>,
    existence: HashMap<String, HashMap<&'static str, bool>>,
}

enum Entry {
    Absent,
    Stored(String),
}

// APFS spends 32 bytes of a directory's size per entry; other volumes spend fewer, past a one block floor.
const ENTRY_BYTES: u64 = 32;
const ENTRIES_PER_LOOKUP: u64 = 16;

thread_local! {
    static DIRECTORIES: RefCell<Directories> = RefCell::default();
}

pub fn real_directory(dir: &str) -> io::Result<String> {
    if let Some(cached) = DIRECTORIES.with_borrow(|directories| directories.real.get(dir).cloned())
    {
        return Ok(cached);
    }

    let parent = dirname(dir);
    let real = if parent != dir && listed_directory(parent, basename(dir)) {
        join(&real_directory(parent)?, basename(dir))
    } else if fs::metadata(dir).is_ok() {
        crate::files::realpath(dir)?
    } else if parent == dir {
        dir.to_owned()
    } else {
        join(&real_directory(parent)?, basename(dir))
    };

    DIRECTORIES.with_borrow_mut(|directories| {
        directories.real.insert(dir.to_owned(), real.clone());
        directories.real.insert(real.clone(), real.clone());
    });

    Ok(real)
}

#[cfg(test)]
thread_local! {
    static READS: RefCell<Vec<String>> = RefCell::default();
}

fn read_listing(dir: &str, typed: bool) -> Option<Rc<Listing>> {
    #[cfg(test)]
    READS.with_borrow_mut(|reads| reads.push(dir.to_owned()));

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

fn listing_due(dir: &str) -> bool {
    DIRECTORIES.with_borrow_mut(|directories| {
        if directories.listings.contains_key(dir) {
            return true;
        }

        let unlisted = match directories.unlisted.get_mut(dir) {
            Some(unlisted) => unlisted,
            None => directories
                .unlisted
                .entry(dir.to_owned())
                .or_insert_with(|| survey(dir)),
        };

        unlisted.as_mut().is_none_or(|unlisted| {
            unlisted.asked += 1;
            unlisted.asked * ENTRIES_PER_LOOKUP >= unlisted.entries
        })
    })
}

// A folder that fits one block lists in a single read, cheaper than any lookup.
fn survey(dir: &str) -> Option<Unlisted> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let size = fs::metadata(dir).ok()?;
    if size.len() <= size.blksize() {
        return None;
    }

    let folder = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(dir)
        .ok()?;

    answers_by_name(&folder).then_some(Unlisted {
        asked: 0,
        entries: size.len() / ENTRY_BYTES,
    })
}

fn statfs(folder: &fs::File) -> Option<libc::statfs> {
    use std::os::fd::AsRawFd;

    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    let status = unsafe { libc::fstatfs(folder.as_raw_fd(), stats.as_mut_ptr()) };
    (status == 0).then(|| unsafe { stats.assume_init() })
}

// exFAT and FAT hand getattrlist a normalized name instead of the stored one, so only these volumes answer by name.
#[cfg(target_os = "macos")]
fn answers_by_name(folder: &fs::File) -> bool {
    statfs(folder).is_some_and(|stats| {
        let kind = stats.f_fstypename.map(|byte| byte as u8);
        matches!(
            std::ffi::CStr::from_bytes_until_nul(&kind).map(std::ffi::CStr::to_bytes),
            Ok(b"apfs" | b"hfs")
        )
    })
}

// FUSE and CIFS can hand out an inode per spelling, which would fool the case probe in lookup.
#[cfg(not(target_os = "macos"))]
fn answers_by_name(folder: &fs::File) -> bool {
    statfs(folder).is_some_and(|stats| {
        [
            libc::EXT4_SUPER_MAGIC as u32,
            libc::BTRFS_SUPER_MAGIC as u32,
            libc::XFS_SUPER_MAGIC as u32,
            libc::TMPFS_MAGIC as u32,
        ]
        .contains(&(stats.f_type as u32))
    })
}

// A listing spells a name that is not UTF-8 with U+FFFD, a spelling no lookup can find.
fn by_name(dir: &str, name: &str) -> Option<Entry> {
    if name.contains(char::REPLACEMENT_CHARACTER) || listing_due(dir) {
        None
    } else {
        lookup(dir, name)
    }
}

fn absent(error: &io::Error) -> Option<Entry> {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR)).then_some(Entry::Absent)
}

// A plain stat resolves a wrong-case or other-normalization name; getattrlist reports the spelling stored.
#[cfg(target_os = "macos")]
fn lookup(dir: &str, name: &str) -> Option<Entry> {
    #[repr(C)]
    struct Reply {
        length: u32,
        name: libc::attrreference_t,
        bytes: [u8; 1024],
    }

    let path = CString::new(join(dir, name)).ok()?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_NAME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };

    let mut reply = Reply {
        length: 0,
        name: libc::attrreference_t {
            attr_dataoffset: 0,
            attr_length: 0,
        },
        bytes: [0; 1024],
    };

    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut request).cast(),
            (&raw mut reply).cast(),
            size_of::<Reply>(),
            libc::FSOPT_NOFOLLOW,
        )
    };

    if status != 0 {
        return absent(&io::Error::last_os_error());
    }

    let start = (std::mem::offset_of!(Reply, name)
        + usize::try_from(reply.name.attr_dataoffset).ok()?)
    .checked_sub(std::mem::offset_of!(Reply, bytes))?;

    let length = usize::try_from(reply.name.attr_length)
        .ok()?
        .checked_sub(1)?;
    let stored = std::str::from_utf8(reply.bytes.get(start..start + length)?).ok()?;
    Some(Entry::Stored(stored.to_owned()))
}

// A casefolded folder resolves the name with one letter's case flipped to the same inode.
#[cfg(not(target_os = "macos"))]
fn lookup(dir: &str, name: &str) -> Option<Entry> {
    use std::os::unix::fs::MetadataExt;

    let found = match fs::symlink_metadata(join(dir, name)) {
        Ok(found) => found,
        Err(error) => return absent(&error),
    };

    let letter = name.bytes().position(|byte| byte.is_ascii_alphabetic())?;

    let mut flipped = name.as_bytes().to_vec();
    flipped[letter] ^= b'a' ^ b'A';
    let flipped = String::from_utf8(flipped).ok()?;
    let exact = match fs::symlink_metadata(join(dir, &flipped)) {
        Ok(other) => (other.dev(), other.ino()) != (found.dev(), found.ino()),
        Err(error) => error.raw_os_error() == Some(libc::ENOENT),
    };

    exact.then(|| Entry::Stored(name.to_owned()))
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

    let folded = name.to_lowercase();
    let spelled = match by_name(dir, name) {
        Some(Entry::Absent) => false,
        Some(Entry::Stored(stored)) if stored.to_lowercase() == folded => true,
        _ => listing(dir, false).is_none_or(|listing| listing.folded().contains(&folded)),
    };

    let exists = spelled && fs::metadata(join(dir, name)).is_ok();

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
    match by_name(dir, name) {
        Some(Entry::Absent) => false,
        Some(Entry::Stored(stored)) => stored == name,
        None => listing(dir, false).is_some_and(|listing| listing.kinds.contains_key(name)),
    }
}

// A symlink in dir would let lstat's expansion pass PATH_MAX where this length check cannot see it.
pub fn listed_file(dir: &str, name: &str) -> Option<bool> {
    if !listing_due(dir) {
        return None;
    }

    let listing = listing(dir, true)?;
    let kind = (*listing.kinds.get(name)?)?;
    let fits = dir.len() + 1 + name.len() < libc::PATH_MAX as usize;
    let unlinked = real_directory(dir).is_ok_and(|real| real == dir);
    (fits && unlinked && listing.searchable(dir)).then(|| kind.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch::Scratch;

    #[test]
    fn derived_real_directories_match_canonicalize() {
        let scratch = Scratch::new("derived-real");
        let root = scratch.path().to_owned();
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
    }

    #[test]
    fn a_symlinked_directory_leaves_the_file_test_to_lstat() {
        let scratch = Scratch::new("linked-file-test");
        let root = scratch.path().to_owned();
        fs::create_dir(join(&root, "real")).unwrap();
        fs::write(join(&root, "real/a.ts"), "").unwrap();
        std::os::unix::fs::symlink(join(&root, "real"), join(&root, "link")).unwrap();

        assert_eq!(listed_file(&join(&root, "real"), "a.ts"), Some(true));
        assert_eq!(listed_file(&join(&root, "link"), "a.ts"), None);
    }

    fn fill(dir: &str, count: usize) {
        for index in 0..count {
            fs::File::create(join(dir, &format!("{index}.txt"))).unwrap();
        }
    }

    fn reads_of(dir: &str) -> usize {
        READS.with_borrow(|reads| reads.iter().filter(|read| *read == dir).count())
    }

    fn folds_case(dir: &str) -> bool {
        let probe = join(dir, "Fold");
        fs::write(&probe, "").unwrap();
        let folds = fs::metadata(join(dir, "fOLD")).is_ok();
        fs::remove_file(probe).unwrap();
        folds
    }

    fn assert_unlisted(dir: &str) {
        let lookups_defer = cfg!(not(target_os = "macos")) && folds_case(dir);
        if survey(dir).is_none() || lookups_defer {
            eprintln!("{dir} answers found names from a listing here, so its reads go unchecked");
            return;
        }

        assert_eq!(reads_of(dir), 0);
    }

    #[test]
    fn few_names_do_not_list_the_directory() {
        use crate::languages::javascript::config::{biome, eslint, oxlint, width};

        let scratch = Scratch::new("few-names");
        let root = scratch.path().to_owned();
        fill(&root, 50_000);
        fs::write(join(&root, "one.ts"), "").unwrap();

        let probed = width::FILES
            .iter()
            .chain(&eslint::FLAT_FILES)
            .chain(&eslint::LEGACY_FILES)
            .chain(&biome::FILES)
            .chain(&oxlint::FILES)
            .chain(&[".editorconfig", ".git", ".hg"]);

        for name in probed {
            assert!(!holds(&root, name), "{name}");
        }

        assert!(listed(&root, "one.ts"));
        assert_ne!(listed_file(&root, "one.ts"), Some(false));
        assert_unlisted(&root);
    }

    #[test]
    fn many_names_list_the_directory_once() {
        let scratch = Scratch::new("many-names");
        let root = scratch.path().to_owned();
        for index in 0..64 {
            fs::write(join(&root, &format!("f{index}.ts")), "").unwrap();
        }

        let names: Vec<String> = (0..64).map(|index| format!("f{index}.ts")).collect();
        for name in &names {
            assert_ne!(listed_file(&root, name), Some(false), "{name}");
        }

        for name in &names {
            assert!(listed(&root, name), "{name}");
        }

        assert_eq!(reads_of(&root), 1);
    }

    #[test]
    fn names_answered_without_a_listing_match_the_entries() {
        let scratch = Scratch::new("by-name");
        let root = scratch.path().to_owned();
        fill(&root, 2_000);
        let nfd = "cafe\u{301}.ts";
        let nfc = "caf\u{e9}.ts";
        fs::write(join(&root, "One.ts"), "").unwrap();
        fs::write(join(&root, nfd), "").unwrap();
        fs::write(join(&root, "Package.json"), "{}").unwrap();
        fs::create_dir(join(&root, "Sub")).unwrap();
        std::os::unix::fs::symlink("One.ts", join(&root, "link.ts")).unwrap();
        std::os::unix::fs::symlink("missing", join(&root, "biome.json")).unwrap();

        let entries: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();

        let check_listed = |name: &str| {
            let expected = entries.iter().any(|entry| entry == name);
            assert_eq!(listed(&root, name), expected, "{name}");
        };

        let check_holds = |name: &'static str| {
            let expected = entries
                .iter()
                .any(|entry| entry.to_lowercase() == name.to_lowercase())
                && fs::metadata(join(&root, name)).is_ok();
            assert_eq!(holds(&root, name), expected, "{name}");
        };

        for name in [
            "One.ts", "one.ts", "ONE.TS", "link.ts", "LINK.ts", "Sub", "sub", "gone.ts",
        ] {
            check_listed(name);
        }

        for name in ["package.json", "biome.json", "biome.jsonc", ".editorconfig"] {
            check_holds(name);
        }

        check_listed(nfd);
        check_listed(nfc);
        assert_unlisted(&root);
    }

    #[test]
    fn folders_without_read_or_search_keep_their_answers() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        for (mode, expected) in [(0o444, (true, false)), (0o111, (false, true))] {
            let scratch = Scratch::new(&format!("mode-{mode:o}"));
            let root = scratch.path().to_owned();
            fill(&root, 2_000);
            fs::write(join(&root, "package.json"), "{}").unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(mode)).unwrap();
            let answers = (listed(&root, "package.json"), holds(&root, "package.json"));
            assert_eq!(answers, expected, "{mode:o}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_has_no_listed_type() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let scratch = Scratch::new("lossy-name");
        let root = scratch.path().to_owned();
        let lossy = PathBuf::from(&root).join(OsStr::from_bytes(b"bad\xff.ts"));
        fs::write(lossy, "").unwrap();

        assert_eq!(listed_file(&root, "bad\u{fffd}.ts"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_lossy_spelling_stays_listed_in_a_large_folder() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let scratch = Scratch::new("lossy-large");
        let root = scratch.path().to_owned();
        fill(&root, 2_000);
        let lossy = PathBuf::from(&root).join(OsStr::from_bytes(b"bad\xff.ts"));
        fs::write(lossy, "").unwrap();

        assert!(listed(&root, "bad\u{fffd}.ts"));
        assert!(!listed(&root, "good\u{fffd}.ts"));
    }
}
