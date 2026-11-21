use std::cell::OnceCell;
use std::fs;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::time::SystemTime;

use super::git::{GitDirs, GitEnv, GitError, Repositories, decoded, git_dirs};
use super::lstat_if_present;
use crate::paths::join;

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

#[cfg(test)]
thread_local! {
    static BYTES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
struct Counted(fs::File);

#[cfg(test)]
impl Read for &Counted {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = (&self.0).read(buffer)?;
        BYTES_READ.set(BYTES_READ.get() + read);
        Ok(read)
    }
}

struct IndexSnapshot {
    bytes: Vec<u8>,
    modified: SystemTime,
    attributes: OnceCell<Option<Vec<TrackedAttributes>>>,
}

enum IndexFile {
    Missing,
    Unscanned,
    Read(IndexSnapshot),
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
        return Ok(IndexFile::Unscanned);
    }

    let path = join(&dirs.git_dir, "index");
    let Some(entry) = lstat_if_present(&path)? else {
        return Ok(IndexFile::Missing);
    };

    if !entry.is_file() {
        return Ok(IndexFile::Unscanned);
    }

    let index = fs::File::open(&path)?;
    let modified = index.metadata()?.modified()?;

    #[cfg(test)]
    let index = Counted(index);

    Ok(match read_index(&index)? {
        Some(bytes) => IndexFile::Read(IndexSnapshot {
            bytes,
            modified,
            attributes: OnceCell::new(),
        }),
        None => IndexFile::Unscanned,
    })
}

// git reads whatever the scan left unread (a symlink, an unsupported header, another object format, an index written since), so the copy takes a handle of its own.
fn whole_index(git_dir: &str) -> Option<(Vec<u8>, SystemTime)> {
    let index = fs::File::open(join(git_dir, "index")).ok()?;
    let modified = index.metadata().ok()?.modified().ok()?;

    #[cfg(test)]
    let index = Counted(index);

    let mut bytes = Vec::new();
    (&index).read_to_end(&mut bytes).ok()?;
    Some((bytes, modified))
}

pub(super) struct TrackedAttributes {
    pub(super) path: String,
    pub(super) skip_worktree: bool,
}

struct Scan {
    attributes: Option<Vec<TrackedAttributes>>,
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

pub(super) struct RootIndex {
    dirs: GitDirs,
    file: IndexFile,
}

impl RootIndex {
    fn found(root: &str) -> Option<RootIndex> {
        let dirs = git_dirs(root)?;
        let file = index_file(&dirs).unwrap_or(IndexFile::Unscanned);
        Some(RootIndex { dirs, file })
    }

    pub(super) fn common_dir(&self) -> &str {
        self.dirs.common_dir()
    }

    fn tracks_all(&self, names: &[String]) -> bool {
        let IndexFile::Read(snapshot) = &self.file else {
            return false;
        };

        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let Scan { attributes, tracks } = scan_in(&snapshot.bytes, &names);
        snapshot.attributes.get_or_init(|| attributes);
        tracks
    }

    pub(super) fn attributes(&self) -> Option<&[TrackedAttributes]> {
        match &self.file {
            IndexFile::Missing => Some(&[]),
            IndexFile::Unscanned => None,
            IndexFile::Read(snapshot) => snapshot
                .attributes
                .get_or_init(|| scan_in(&snapshot.bytes, &[]).attributes)
                .as_deref(),
        }
    }

    fn private_copy(&self, root: &str) -> Option<PrivateIndex> {
        let git_dir = &self.dirs.git_dir;
        match &self.file {
            IndexFile::Read(snapshot) => {
                PrivateIndex::copied(root, git_dir, &snapshot.bytes, snapshot.modified)
            }
            IndexFile::Missing | IndexFile::Unscanned => {
                let (bytes, modified) = whole_index(git_dir)?;
                PrivateIndex::copied(root, git_dir, &bytes, modified)
            }
        }
    }
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

    fn copied(
        root: &str,
        git_dir: &str,
        bytes: &[u8],
        modified: SystemTime,
    ) -> Option<PrivateIndex> {
        let random = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("stanza-index-{random:016x}"));
        fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;

        let index = PrivateIndex {
            dir,
            shown: shown_index(root, git_dir),
        };
        let mut copy = fs::File::create_new(index.path()).ok()?;
        // Not fs::copy: an APFS clone is a fresh vnode, so git reads the whole index back from disk.
        copy.write_all(bytes).ok()?;

        // git treats an entry as racily clean against the index file's mtime, so the copy keeps the mtime of the handle its bytes came from.
        copy.set_modified(modified).ok()?;

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
    pub(super) fn root_index(&mut self, root: &str) -> Option<&RootIndex> {
        if index_relocated() {
            return None;
        }

        self.root_indexes
            .entry(root.to_owned())
            .or_insert_with(|| RootIndex::found(root))
            .as_ref()
    }

    fn private_index(&mut self, root: &str) -> Option<&PrivateIndex> {
        if !self.private_indexes.contains_key(root) {
            let copy = self
                .root_index(root)
                .and_then(|index| index.private_copy(root));

            self.private_indexes.insert(root.to_owned(), copy);
        }

        self.private_indexes[root].as_ref()
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
        Ok(decoded(&tree).trim().to_owned())
    }

    pub(super) fn tracks_all(&mut self, root: &str, names: &[String]) -> bool {
        self.root_index(root)
            .is_some_and(|index| index.tracks_all(names))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::files::attributes::{SELECTION_ATTRIBUTES, Source};
    use crate::files::git::{GitOutput, parsed_variables};

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

        fn index(&self) -> String {
            join(&self.0, ".git/index")
        }

        fn stamp(&self, since_epoch: Duration) -> SystemTime {
            let modified = SystemTime::UNIX_EPOCH + since_epoch;
            let index = fs::File::options().write(true).open(self.index()).unwrap();
            index.set_modified(modified).unwrap();
            index.metadata().unwrap().modified().unwrap()
        }

        fn link_index(&self) {
            let target = join(&self.0, "elsewhere");
            fs::rename(self.index(), &target).unwrap();
            symlink(&target, self.index()).unwrap();
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const CHILD: &str = "STANZA_TEST_INDEX_CHILD";

    // Every index reader consults the GIT_ variables of its own process, so a case runs in a child that has none.
    fn without_git_environment(test: &str) -> bool {
        in_git_environment(test, None)
    }

    fn in_git_environment(test: &str, present: Option<&str>) -> bool {
        if std::env::var_os(CHILD).is_some() {
            return true;
        }

        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", &format!("files::index::tests::{test}")])
            .env(CHILD, "1");

        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"GIT_") {
                child.env_remove(name);
            }
        }

        if let Some(name) = present {
            child.env(name, "");
        }

        let output = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
        false
    }

    type Scanned = (Option<Vec<(String, bool)>>, bool);

    fn listed(repositories: &mut Repositories, root: &str) -> Option<Vec<(String, bool)>> {
        let attributes = repositories.root_index(root)?.attributes()?;
        let listed = attributes
            .iter()
            .map(|file| (file.path.clone(), file.skip_worktree));

        Some(listed.collect())
    }

    fn scanned(repositories: &mut Repositories, root: &str) -> Scanned {
        let tracks = repositories.tracks_all(root, &["a.ts".to_owned()]);
        (listed(repositories, root), tracks)
    }

    fn answered(repository: &Repository) -> Scanned {
        let root = &repository.0;
        let mut attributes_first = Repositories::new(None);
        let attributes = listed(&mut attributes_first, root);
        let tracks = attributes_first.tracks_all(root, &["a.ts".to_owned()]);
        let answer = scanned(&mut Repositories::new(None), root);
        assert_eq!((attributes, tracks), answer);
        answer
    }

    const ENTRIES: [(&str, u16); 2] = [(".gitattributes", 0), ("a.ts", 0)];
    const STAMP: Duration = Duration::new(1_700_000_000, 123_456_789);
    const SHA256: &str =
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectFormat = sha256\n";

    #[test]
    fn index_snapshot_answers_follow_the_index_on_disk() {
        if !without_git_environment("index_snapshot_answers_follow_the_index_on_disk") {
            return;
        }

        let on_disk = |bytes: &[u8]| answered(&Repository::new(None, bytes));

        let known: Scanned = (Some(vec![(".gitattributes".to_owned(), false)]), true);
        let plain = index(2, &ENTRIES, b"");
        assert_eq!(on_disk(&plain), known);

        let extended = [(".gitattributes", 0x2000), ("a.ts", 0)];
        assert_eq!(on_disk(&index(3, &extended, b"")), known);
        for signature in OPTIONAL {
            let optional = extension(signature, b"\x01\x02\x03");
            assert_eq!(on_disk(&index(2, &ENTRIES, &optional)), known);
            assert_eq!(on_disk(&index(3, &extended, &optional)), known);
        }

        let unknown: Scanned = (None, false);
        assert_eq!(
            on_disk(&index(2, &ENTRIES, &extension(b"link", b""))),
            unknown
        );
        assert_eq!(on_disk(&index(4, &ENTRIES, b"")), unknown);
        assert_eq!(on_disk(&plain[..plain.len() - 1]), unknown);
        assert_eq!(on_disk(b"DIRC"), unknown);

        let sha256 = Repository::new(Some(SHA256), &plain);
        assert_eq!(answered(&sha256), unknown);

        let linked = Repository::new(None, &plain);
        linked.link_index();
        assert_eq!(answered(&linked), unknown);

        let missing = Repository::new(None, &plain);
        fs::remove_file(missing.index()).unwrap();
        assert_eq!(answered(&missing), (Some(Vec::new()), false));
    }

    fn private_copy(repositories: &mut Repositories, root: &str) -> Option<(Vec<u8>, SystemTime)> {
        let GitEnv::WorktreeDiff(copy) = repositories.worktree_diff(root) else {
            panic!("{root} got another environment");
        };

        let path = copy?.path;
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        Some((fs::read(&path).unwrap(), modified))
    }

    #[test]
    fn index_snapshot_private_copy_keeps_the_bytes_and_mtime_of_its_source() {
        if !without_git_environment(
            "index_snapshot_private_copy_keeps_the_bytes_and_mtime_of_its_source",
        ) {
            return;
        }

        for (version, linked) in [(2, false), (3, false), (4, false), (2, true)] {
            let bytes = index(version, &ENTRIES, &extension(b"TREE", b"\x01"));
            let repository = Repository::new(None, &bytes);
            let modified = repository.stamp(STAMP);
            if linked {
                repository.link_index();
            }

            let mut repositories = Repositories::new(None);
            scanned(&mut repositories, &repository.0);
            assert_eq!(
                private_copy(&mut repositories, &repository.0),
                Some((bytes, modified)),
                "version {version}, linked {linked}"
            );
        }

        let missing = Repository::new(None, b"");
        fs::remove_file(missing.index()).unwrap();
        assert_eq!(private_copy(&mut Repositories::new(None), &missing.0), None);
    }

    fn prechecked(root: &str) -> Repositories {
        let listed = [
            "core.attributesfile=/nowhere/global",
            "GIT_ATTR_SYSTEM=/nowhere/system",
            "GIT_ATTR_GLOBAL=/nowhere/global",
            "GIT_CONFIG_SYSTEM=/nowhere/config",
            "",
        ];

        let output = GitOutput {
            output: listed.join("\n").into_bytes(),
            warnings: String::new(),
        };

        let mut repositories = Repositories::new(None);
        let variables = Rc::new(parsed_variables(Ok(output)));
        repositories.variables.insert(root.to_owned(), variables);
        BYTES_READ.set(0);
        repositories
    }

    fn may_opt_in(repositories: &mut Repositories, root: &str, name: &str) -> bool {
        repositories.may_opt_in(&[join(root, name)], root)
    }

    type Stamped = (Vec<u8>, SystemTime);
    type Consumer = fn(&mut Repositories, &str, &Stamped);
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    fn tracking(repositories: &mut Repositories, root: &str, _: &Stamped) {
        let scope = |names: [&str; 2]| names.map(str::to_owned);
        assert!(repositories.tracks_all(root, &scope(["a.ts", "b/c.ts"])));
        assert!(!repositories.tracks_all(root, &scope(["a.ts", "d.ts"])));
    }

    fn prechecks(repositories: &mut Repositories, root: &str, _: &Stamped) {
        let unreached = [join(root, "a.ts")];
        assert!(!repositories.may_opt_in(&unreached, root));

        let skipped = repositories
            .read_attributes(&unreached, root, &SELECTION_ATTRIBUTES, Source::Worktree)
            .unwrap();

        assert_eq!(skipped.get(root, &unreached[0], "stanza"), None);
        assert!(may_opt_in(repositories, root, "b/c.ts"));
    }

    fn copying(repositories: &mut Repositories, root: &str, source: &Stamped) {
        assert_eq!(private_copy(repositories, root).as_ref(), Some(source));
    }

    #[test]
    fn index_snapshot_shares_one_full_read() {
        if !without_git_environment("index_snapshot_shares_one_full_read") {
            return;
        }

        let plain = [("a.ts", 0), ("b/.gitattributes", 0), ("b/c.ts", 0)];
        let extended = [("a.ts", 0), ("b/.gitattributes", 0x2000), ("b/c.ts", 0)];
        let consumers: [Consumer; 3] = [tracking, prechecks, copying];
        for (version, entries) in [(2, &plain), (3, &extended)] {
            let bytes = index(version, entries, &extension(b"TREE", b"\x01"));
            for order in ORDERS {
                let repository = Repository::new(None, &bytes);
                let source = (bytes.clone(), repository.stamp(STAMP));
                let root = &repository.0;
                let mut repositories = prechecked(root);
                for consumer in order {
                    consumers[consumer](&mut repositories, root, &source);
                }

                assert_eq!(
                    BYTES_READ.get(),
                    bytes.len(),
                    "version {version}, order {order:?}"
                );
            }
        }
    }

    #[test]
    fn index_snapshot_leaves_what_the_scan_refuses_to_the_copy() {
        if !without_git_environment("index_snapshot_leaves_what_the_scan_refuses_to_the_copy") {
            return;
        }

        let refused = [
            (4, None, false, HEADER),
            (2, None, true, 0),
            (2, Some(SHA256), false, 0),
        ];

        for (version, config, linked, scanned_bytes) in refused {
            let bytes = index(version, &ENTRIES, b"");
            let repository = Repository::new(config, &bytes);
            let source = (bytes, repository.stamp(STAMP));
            if linked {
                repository.link_index();
            }

            let root = &repository.0;
            let mut repositories = prechecked(root);
            assert_eq!(scanned(&mut repositories, root), (None, false));
            assert!(may_opt_in(&mut repositories, root, "a.ts"));
            assert!(may_opt_in(&mut repositories, root, "b/c.ts"));
            assert_eq!(BYTES_READ.get(), scanned_bytes, "version {version}");

            copying(&mut repositories, root, &source);
            assert_eq!(BYTES_READ.get(), scanned_bytes + source.0.len());
        }
    }

    #[test]
    fn index_snapshot_is_never_read_under_a_relocated_index() {
        let test = "index_snapshot_is_never_read_under_a_relocated_index";
        for name in INDEX_ENVIRONMENT {
            if !in_git_environment(test, Some(name)) {
                continue;
            }

            let repository = Repository::new(None, &index(2, &ENTRIES, b""));
            let root = &repository.0;
            let mut repositories = prechecked(root);
            assert_eq!(scanned(&mut repositories, root), (None, false));
            assert!(may_opt_in(&mut repositories, root, "a.ts"));
            assert_eq!(private_copy(&mut repositories, root), None);
            assert_eq!(BYTES_READ.get(), 0);
            return;
        }
    }

    #[test]
    fn index_snapshot_private_copy_ignores_a_source_replaced_after_the_read() {
        if !without_git_environment(
            "index_snapshot_private_copy_ignores_a_source_replaced_after_the_read",
        ) {
            return;
        }

        let bytes = index(2, &ENTRIES, b"");
        let repository = Repository::new(None, &bytes);
        let modified = repository.stamp(STAMP);
        let root = &repository.0;
        let mut repositories = Repositories::new(None);
        let known: Scanned = (Some(vec![(".gitattributes".to_owned(), false)]), true);
        assert_eq!(scanned(&mut repositories, root), known);

        let replacement = join(root, "replacement");
        fs::write(&replacement, index(2, &[("z.ts", 0)], b"")).unwrap();
        fs::rename(&replacement, repository.index()).unwrap();
        assert_ne!(repository.stamp(STAMP + Duration::new(60, 1)), modified);

        assert_eq!(
            private_copy(&mut repositories, root),
            Some((bytes, modified))
        );

        assert_eq!(scanned(&mut repositories, root), known);
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
            let index = RootIndex::found(&repository.0).unwrap();
            (
                index.attributes().map(<[TrackedAttributes]>::len),
                index.tracks_all(&names),
            )
        };

        let sha1 = (Some(1), true);
        assert_eq!(read(None), sha1);
        assert_eq!(read(Some("[core]\n\trepositoryformatversion = 0\n")), sha1);

        let fallback = (None, false);
        assert_eq!(read(Some(SHA256)), fallback);
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
