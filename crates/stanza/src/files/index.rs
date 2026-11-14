use std::fs;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::rc::Rc;

use super::git::{GitEnv, Repositories, git_dirs};
use super::lstat_if_present;
use crate::paths::join;

const HEADER: usize = 12;

fn ascii_signature(bytes: &[u8], signature: &[u8]) -> bool {
    bytes
        .iter()
        .map(|byte| byte & 0x7f)
        .eq(signature.iter().copied())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn handled_header(header: &[u8]) -> bool {
    ascii_signature(&header[..4], b"DIRC") && matches!(read_u32(header, 4), 2 | 3)
}

fn walk_entries(bytes: &[u8], mut visit: impl FnMut(&[u8], bool)) -> Option<()> {
    let length = bytes.len() as i64;
    if length < 32 || !handled_header(bytes) {
        return None;
    }

    let version = read_u32(bytes, 4);
    let end_of_entries = length - 20;
    let mut offset = HEADER as i64;
    let count = read_u32(bytes, 8);
    for _ in 0..count {
        if offset + 62 > end_of_entries {
            return None;
        }

        let at = offset as usize;
        if read_u32(bytes, at + 24) & 0o170000 == 0o040000 {
            return None;
        }

        let flags = u16::from_be_bytes([bytes[at + 60], bytes[at + 61]]);
        let extended = flags & 0x4000 != 0;
        if extended && version != 3 {
            return None;
        }

        let skip_worktree =
            extended && u16::from_be_bytes([bytes[at + 62], bytes[at + 63]]) & 0x4000 != 0;

        let start = at + 62 + if extended { 2 } else { 0 };
        let end = start + bytes.get(start..)?.iter().position(|&byte| byte == 0)?;
        if end as i64 >= end_of_entries {
            return None;
        }

        visit(&bytes[start..end], skip_worktree);
        offset += (end as i64 + 1 - offset + 7) / 8 * 8;
    }

    while offset < end_of_entries {
        if offset + 8 > end_of_entries {
            return None;
        }

        let at = offset as usize;
        if !ascii_signature(&bytes[at..at + 4], b"TREE") {
            return None;
        }

        offset += 8 + i64::from(read_u32(bytes, at + 4));
    }

    (offset == end_of_entries).then_some(())
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

fn index_file(git_dir: &str) -> io::Result<IndexFile> {
    let path = join(git_dir, "index");
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

fn scan(git_dir: &str, names: &[String]) -> Scan {
    match index_file(git_dir) {
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

pub(super) fn tracked_attributes(git_dir: &str) -> io::Result<Option<Vec<TrackedAttributes>>> {
    Ok(match index_file(git_dir)? {
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

#[derive(Clone, PartialEq, Eq)]
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
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Repositories {
    pub(super) fn worktree_diff(&mut self, root: &str) -> GitEnv {
        let index = self
            .private_indexes
            .entry(root.to_owned())
            .or_insert_with(|| PrivateIndex::copied(root));

        GitEnv::WorktreeDiff(index.as_ref().map(PrivateIndex::copy))
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
            Some(dirs) => scan(&dirs.git_dir, names),
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
        assert_eq!(
            tracked(&index(2, &[("a.ts", 0)], b"\xd4\xd2\xc5\xc5\0\0\0\0")),
            no_attributes
        );

        assert_eq!(
            tracked(&index(2, &[("a.ts", 0)], b"TREE\0\0\0\0")),
            no_attributes
        );

        assert_eq!(tracked(&index(2, &[("a.ts", 0)], b"link\0\0\0\0")), None);
        assert_eq!(tracked(b"DIRC"), None);
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
