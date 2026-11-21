use std::fs;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::rc::Rc;

use super::git::{GitDirs, GitEnv, GitError, Repositories, decoded, git_dirs};
use super::lstat_if_present;
use crate::paths::join;
use crate::text::js_trim;

const HEADER: usize = 12;
const TRAILER: usize = 20;
const ENTRY_NAME: usize = 62;

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    Some(u16::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn handled_header(header: &[u8]) -> bool {
    header.get(..4) == Some(&b"DIRC"[..]) && matches!(read_u32(header, 4), Some(2 | 3))
}

// git's index format marks an extension optional when its signature starts with A to Z; any other one changes what the entries mean.
fn optional_extension(signature: &[u8]) -> bool {
    signature.first().is_some_and(u8::is_ascii_uppercase)
}

fn walk_entries(bytes: &[u8], mut visit: impl FnMut(&[u8], bool)) -> Option<()> {
    let body = bytes.get(..bytes.len().checked_sub(TRAILER)?)?;
    if !handled_header(body.get(..HEADER)?) {
        return None;
    }

    let version = read_u32(body, 4)?;
    let count = read_u32(body, 8)?;
    let mut offset = HEADER;
    for _ in 0..count {
        let entry = body.get(offset..)?;
        if read_u32(entry, 24)? & 0o170000 == 0o040000 {
            return None;
        }

        let extended = read_u16(entry, ENTRY_NAME - 2)? & 0x4000 != 0;
        if extended && version != 3 {
            return None;
        }

        let skip_worktree = extended && read_u16(entry, ENTRY_NAME)? & 0x4000 != 0;
        let start = ENTRY_NAME + if extended { 2 } else { 0 };
        let length = entry.get(start..)?.iter().position(|&byte| byte == 0)?;
        visit(&entry[start..start + length], skip_worktree);
        offset = offset.checked_add((start + length + 8) / 8 * 8)?;
    }

    while offset < body.len() {
        let header = body.get(offset..offset.checked_add(8)?)?;
        if !optional_extension(header) {
            return None;
        }

        let size = usize::try_from(read_u32(header, 4)?).ok()?;
        offset = offset.checked_add(8)?.checked_add(size)?;
    }

    (offset == body.len()).then_some(())
}

fn read_index(mut index: impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = vec![0; HEADER];
    match index.read_exact(&mut bytes) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }

    if !handled_header(&bytes) {
        return Ok(None);
    }

    index.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

enum IndexFile {
    Missing,
    Unreadable,
    Read(Vec<u8>),
}

// An entry holds a 20 byte object name only under SHA-1, and git reads the object format from this one file.
fn sha1_objects(common_dir: &str) -> bool {
    match fs::read(join(common_dir, "config")) {
        Ok(text) => !text
            .windows(b"objectformat".len())
            .any(|window| window.eq_ignore_ascii_case(b"objectformat")),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

fn index_file(dirs: &GitDirs) -> io::Result<IndexFile> {
    if !sha1_objects(dirs.common_dir()) {
        return Ok(IndexFile::Unreadable);
    }

    let path = join(&dirs.git_dir, "index");
    let Some(entry) = lstat_if_present(&path)? else {
        return Ok(IndexFile::Missing);
    };

    if !entry.is_file() {
        return Ok(IndexFile::Unreadable);
    }

    Ok(read_index(fs::File::open(&path)?)?.map_or(IndexFile::Unreadable, IndexFile::Read))
}

pub(super) struct TrackedAttributes {
    pub(super) path: String,
    pub(super) skip_worktree: bool,
}

pub(super) struct Scan {
    pub(super) attributes: Option<Vec<TrackedAttributes>>,
    tracks: bool,
}

// git sorts entries by name bytes, so one merge pass finds each name; an unsorted index only loses matches.
fn scan_in(bytes: &[u8], names: &[&str]) -> Scan {
    let mut wanted: Vec<&[u8]> = names.iter().map(|name| name.as_bytes()).collect();
    wanted.sort_unstable();
    wanted.dedup();

    let mut attributes = Vec::new();
    let mut next = 0;
    let mut found = 0;
    let walked = walk_entries(bytes, |name, skip_worktree| {
        if name == b".gitattributes" || name.ends_with(b"/.gitattributes") {
            attributes.push((name.to_vec(), skip_worktree));
        }

        while wanted.get(next).is_some_and(|candidate| *candidate < name) {
            next += 1;
        }

        if wanted.get(next) == Some(&name) {
            found += 1;
            next += 1;
        }
    });

    if walked.is_none() {
        return Scan {
            attributes: None,
            tracks: false,
        };
    }

    let attributes = attributes
        .into_iter()
        .map(|(name, skip_worktree)| {
            Some(TrackedAttributes {
                path: String::from_utf8(name).ok()?,
                skip_worktree,
            })
        })
        .collect();

    Scan {
        attributes,
        tracks: found == wanted.len(),
    }
}

fn scan(dirs: &GitDirs, names: &[String]) -> Scan {
    match index_file(dirs) {
        Ok(IndexFile::Read(bytes)) => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            scan_in(&bytes, &names)
        }
        Ok(IndexFile::Missing) => Scan {
            attributes: Some(Vec::new()),
            tracks: false,
        },
        Ok(IndexFile::Unreadable) | Err(_) => Scan {
            attributes: None,
            tracks: false,
        },
    }
}

pub(super) fn tracked_attributes(dirs: &GitDirs) -> io::Result<Option<Vec<TrackedAttributes>>> {
    Ok(match index_file(dirs)? {
        IndexFile::Missing => Some(Vec::new()),
        IndexFile::Unreadable => None,
        IndexFile::Read(bytes) => scan_in(&bytes, &[]).attributes,
    })
}

const INDEX_ENVIRONMENT: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

fn index_relocated() -> bool {
    INDEX_ENVIRONMENT
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct IndexCopy {
    pub(super) path: PathBuf,
    shown: String,
}

impl IndexCopy {
    pub(super) fn named_as_git(&self, reason: String) -> String {
        reason.replace(&*self.path.to_string_lossy(), &self.shown)
    }
}

pub(super) struct PrivateIndex {
    dir: PathBuf,
    shown: String,
}

// git names the index under the GIT_DIR its setup found: .git relative to the root it runs in, or a gitfile's target as an absolute path.
fn shown_index(root: &str, git_dir: &str) -> String {
    let marker = join(root, ".git");
    if git_dir == marker {
        ".git/index".to_owned()
    } else {
        join(git_dir, "index")
    }
}

impl PrivateIndex {
    fn path(&self) -> PathBuf {
        self.dir.join("index")
    }

    fn copy(&self) -> IndexCopy {
        IndexCopy {
            path: self.path(),
            shown: self.shown.clone(),
        }
    }

    fn copied(root: &str) -> Option<PrivateIndex> {
        if index_relocated() {
            return None;
        }

        let git_dir = git_dirs(root)?.git_dir;
        let mut source = fs::File::open(join(&git_dir, "index")).ok()?;
        let meta = source.metadata().ok()?;
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).ok()?);
        source.read_to_end(&mut bytes).ok()?;

        let random = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("stanza-index-{random:016x}"));
        fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;

        let index = PrivateIndex {
            dir,
            shown: shown_index(root, &git_dir),
        };
        let mut copy = fs::File::create_new(index.path()).ok()?;
        // Not fs::copy: an APFS clone is a fresh vnode, so git reads the whole index back from disk.
        copy.write_all(&bytes).ok()?;

        // git treats an entry as racily clean against the index file's mtime, so the copy keeps the original's.
        copy.set_modified(meta.modified().ok()?).ok()?;

        // git cannot take a lock that already exists, root included, so it never writes the refreshed copy or a sharedindex file into .git.
        fs::File::create_new(index.dir.join("index.lock")).ok()?;
        Some(index)
    }

    // write-tree dies when it cannot take the index lock, so it gets a second copy whose lock is free.
    fn unlocked(&self) -> Option<IndexCopy> {
        let path = self.dir.join("tree-index");
        fs::copy(self.path(), &path).ok()?;
        Some(IndexCopy {
            path,
            shown: self.shown.clone(),
        })
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Repositories {
    fn private_index(&mut self, root: &str) -> Option<&PrivateIndex> {
        self.private_indexes
            .entry(root.to_owned())
            .or_insert_with(|| PrivateIndex::copied(root))
            .as_ref()
    }

    pub(super) fn worktree_diff(&mut self, root: &str) -> GitEnv {
        GitEnv::WorktreeDiff(self.private_index(root).map(PrivateIndex::copy))
    }

    pub(super) fn write_tree(&mut self, root: &str) -> Result<String, String> {
        let copy = self.private_index(root).and_then(PrivateIndex::unlocked);

        // Writing the copy as a split index can make a new shared index in .git and expire the one the real index names.
        let args: &[&str] = if copy.is_some() {
            &["-c", "core.splitIndex=false", "write-tree"]
        } else {
            &["write-tree"]
        };

        let tree = self
            .git(root, args, None, &GitEnv::WorktreeDiff(copy))
            .map_err(GitError::message)?
            .output;
        Ok(js_trim(&decoded(&tree)).to_owned())
    }

    pub(super) fn tracks_all(&mut self, root: &str, names: &[String]) -> bool {
        if index_relocated() {
            return false;
        }

        let key = (root.to_owned(), names.to_vec());
        if let Some(&tracks) = self.tracked_scopes.get(&key) {
            return tracks;
        }

        let scan = match git_dirs(root) {
            Some(dirs) => scan(&dirs, names),
            None => Scan {
                attributes: None,
                tracks: false,
            },
        };

        self.index_attributes
            .insert(root.to_owned(), Rc::new(scan.attributes));

        self.tracked_scopes.insert(key, scan.tracks);
        scan.tracks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn a_private_index_under_a_name_that_is_not_utf8_reads_as_git_names_the_index() {
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff/stanza-index-1/index"));
        let copy = IndexCopy {
            path,
            shown: ".git/index".to_owned(),
        };

        let stderr = b"fatal: /tmp/\xff/stanza-index-1/index: index file smaller than expected";
        let reason = String::from_utf8_lossy(stderr).into_owned();
        assert_eq!(
            copy.named_as_git(reason),
            "fatal: .git/index: index file smaller than expected"
        );
    }

    fn index(version: u32, names: &[(&str, u16)], extension: &[u8]) -> Vec<u8> {
        let mut bytes = b"DIRC".to_vec();
        bytes.extend(version.to_be_bytes());
        bytes.extend((names.len() as u32).to_be_bytes());
        for &(name, extended_flags) in names {
            let start = bytes.len();
            let extended = if extended_flags == 0 { 0 } else { 0x4000 };
            bytes.extend([0u8; 24]);
            bytes.extend(0o100644u32.to_be_bytes());
            bytes.extend([0u8; 32]);
            bytes.extend((extended | name.len() as u16).to_be_bytes());
            if extended_flags != 0 {
                bytes.extend(extended_flags.to_be_bytes());
            }

            bytes.extend(name.as_bytes());
            bytes.push(0);
            while !(bytes.len() - start).is_multiple_of(8) {
                bytes.push(0);
            }
        }

        bytes.extend(extension);
        bytes.extend([0u8; 20]);
        bytes
    }

    fn tracked(bytes: &[u8]) -> Option<Vec<(String, bool)>> {
        let bytes = read_index(bytes).expect("in memory read")?;
        scan_in(&bytes, &[]).attributes.map(|entries| {
            entries
                .into_iter()
                .map(|entry| (entry.path, entry.skip_worktree))
                .collect()
        })
    }

    #[test]
    fn an_index_lists_its_attribute_files_and_is_conservative_on_extensions() {
        let no_attributes = Some(Vec::new());
        assert_eq!(
            tracked(&index(2, &[("a.ts", 0), ("b/c.ts", 0)], b"")),
            no_attributes
        );

        assert_eq!(
            tracked(&index(2, &[("b/.gitattributes", 0), ("b/c.ts", 0)], b"")),
            Some(vec![("b/.gitattributes".to_owned(), false)])
        );

        assert_eq!(
            tracked(&index(
                3,
                &[(".gitattributes", 0x4000), ("x/.gitattributes", 0x2000)],
                b""
            )),
            Some(vec![
                (".gitattributes".to_owned(), true),
                ("x/.gitattributes".to_owned(), false),
            ])
        );

        assert_eq!(tracked(&index(2, &[("a.ts", 0x4000)], b"")), None);
        assert_eq!(tracked(&index(4, &[("a.ts", 0)], b"")), None);
        assert_eq!(tracked(b"DIRC"), None);
    }

    fn extension(signature: &[u8], body: &[u8]) -> Vec<u8> {
        let mut bytes = signature.to_vec();
        bytes.extend(u32::try_from(body.len()).unwrap().to_be_bytes());
        bytes.extend(body);
        bytes
    }

    type Answers = (Option<Vec<(String, bool)>>, bool, bool);

    fn answers(bytes: &[u8]) -> Answers {
        (
            tracked(bytes),
            scan_in(bytes, &["a.ts", "b/.gitattributes"]).tracks,
            scan_in(bytes, &["a.ts", "c.ts"]).tracks,
        )
    }

    const OPTIONAL: [&[u8]; 7] = [
        b"TREE", b"REUC", b"UNTR", b"FSMN", b"EOIE", b"IEOT", b"ZQXJ",
    ];

    #[test]
    fn optional_extensions_leave_the_entries_as_they_are() {
        let plain = [(".gitattributes", 0), ("a.ts", 0), ("b/.gitattributes", 0)];
        let extended = [
            (".gitattributes", 0x2000),
            ("a.ts", 0),
            ("b/.gitattributes", 0x4000),
        ];

        for (version, entries) in [(2, &plain), (3, &extended)] {
            let bare = answers(&index(version, entries, b""));
            assert!(bare.0.as_ref().is_some_and(|files| files.len() == 2));
            assert!(bare.1 && !bare.2);

            let mut all = Vec::new();
            for signature in OPTIONAL {
                let one = extension(signature, b"\x01\x02\x03");
                assert_eq!(answers(&index(version, entries, &one)), bare);
                all.extend(one);
            }

            all.extend(extension(b"IEOT", b""));
            assert_eq!(answers(&index(version, entries, &all)), bare);
        }
    }

    #[test]
    fn a_malformed_or_required_extension_gives_the_fallback_answer() {
        let fallback: Answers = (None, false, false);
        let entries = [("a.ts", 0), ("b/.gitattributes", 0)];
        let with = |extension: &[u8]| answers(&index(2, &entries, extension));

        for signature in [
            &b"link"[..],
            b"sdir",
            b"\xd4\xd2\xc5\xc5",
            b"\xc1BCD",
            b"1BCD",
        ] {
            assert_eq!(with(&extension(signature, b"")), fallback);
        }

        let tree = extension(b"TREE", b"\x01\x02\x03\x04");
        assert_eq!(with(&tree[..6]), fallback);
        assert_eq!(with(&tree[..10]), fallback);
        assert_eq!(with(&[&tree[..], b"UNTR"].concat()), fallback);
        assert_eq!(with(b"TREE\0\0\0\x18"), fallback);
        assert_eq!(with(b"TREE\xff\xff\xff\xff"), fallback);

        let whole = index(2, &entries, &tree);
        for cut in HEADER..whole.len() - TRAILER {
            let mut short = whole[..cut].to_vec();
            short.extend([0xff; TRAILER]);
            let answer = answers(&short);
            if cut < whole.len() - TRAILER - tree.len() {
                assert_eq!(answer, fallback, "cut at {cut}");
            }
        }

        let mut counted = index(2, &entries, b"");
        counted[11] = 3;
        assert_eq!(answers(&counted), fallback);

        let mut unnamed = index(2, &[("a.ts", 0)], b"");
        unnamed.truncate(HEADER + ENTRY_NAME);
        unnamed.extend(b"a.ts\x01\x01");
        unnamed.extend([0; TRAILER]);
        assert_eq!(answers(&unnamed), fallback);

        let mut flagless = index(3, &[("a.ts", 0x4000)], b"");
        flagless.truncate(HEADER + ENTRY_NAME);
        flagless.extend([0; TRAILER]);
        assert_eq!(answers(&flagless), fallback);

        let bare = index(2, &entries, b"");
        assert_eq!(answers(&bare[..bare.len() - 1]), fallback);
    }

    struct Repository(String);

    impl Repository {
        fn new(config: Option<&str>, index_bytes: &[u8]) -> Repository {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("stanza-index-{}-{next}", std::process::id()));
            let git = root.join(".git");
            fs::create_dir_all(git.join("objects")).unwrap();
            fs::create_dir_all(git.join("refs")).unwrap();
            fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            fs::write(git.join("index"), index_bytes).unwrap();
            if let Some(config) = config {
                fs::write(git.join("config"), config).unwrap();
            }

            Repository(root.to_string_lossy().into_owned())
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_repository_on_another_object_format_gets_the_fallback_answer() {
        let bytes = index(
            2,
            &[(".gitattributes", 0), ("a.ts", 0)],
            &extension(b"UNTR", b""),
        );
        let names = ["a.ts".to_owned()];
        let read = |config: Option<&str>| {
            let repository = Repository::new(config, &bytes);
            let dirs = git_dirs(&repository.0).unwrap();
            let scanned = scan(&dirs, &names);
            let attributes = tracked_attributes(&dirs).unwrap().map(|files| files.len());
            (
                scanned.attributes.map(|files| files.len()),
                scanned.tracks,
                attributes,
            )
        };

        let sha1 = (Some(1), true, Some(1));
        assert_eq!(read(None), sha1);
        assert_eq!(read(Some("[core]\n\trepositoryformatversion = 0\n")), sha1);

        let fallback = (None, false, None);
        let sha256 =
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectFormat = sha256\n";
        assert_eq!(read(Some(sha256)), fallback);
        assert_eq!(
            read(Some("[extensions]\n\tOBJECTFORMAT = sha1\n")),
            fallback
        );
        assert_eq!(
            read(Some("[extensions]\n\tcompatObjectFormat = sha256\n")),
            fallback
        );
    }

    #[test]
    fn an_unhandled_index_version_stops_after_the_header() {
        let mut bytes = index(4, &[("a.ts", 0)], b"");
        bytes.resize(1 << 20, 0);
        let mut reader = io::Cursor::new(bytes);
        assert!(read_index(&mut reader).unwrap().is_none());
        assert_eq!(reader.position(), HEADER as u64);

        let mut reader = io::Cursor::new(index(2, &[("a.ts", 0)], b""));
        assert!(read_index(&mut reader).unwrap().is_some());
        assert_eq!(reader.position(), reader.get_ref().len() as u64);
    }

    #[test]
    fn an_index_tracks_a_scope_only_when_it_holds_every_name() {
        let tracks_all_in = |bytes: &[u8], names: &[&str]| scan_in(bytes, names).tracks;
        let entries = [("a.ts", 0), ("a.ts", 0), ("b/c.ts", 0), ("b/d.ts", 0)];
        let bytes = index(2, &entries, b"");
        assert!(tracks_all_in(&bytes, &["b/d.ts", "a.ts", "a.ts"]));
        assert!(tracks_all_in(&bytes, &[]));
        assert!(!tracks_all_in(&bytes, &["a.ts", "b/c"]));
        assert!(!tracks_all_in(&bytes, &["b", "b/c.ts"]));
        assert!(!tracks_all_in(&bytes, &["A.ts"]));
        assert!(!tracks_all_in(&bytes, &["e.ts"]));

        let unsorted = index(2, &[("b.ts", 0), ("a.ts", 0)], b"");
        assert!(!tracks_all_in(&unsorted, &["a.ts", "b.ts"]));
        assert!(!tracks_all_in(
            &index(2, &[("a.ts", 0)], b"link\0\0\0\0"),
            &["a.ts"]
        ));
        assert!(!tracks_all_in(&index(4, &[("a.ts", 0)], b""), &["a.ts"]));
    }
}
