use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CREATED: AtomicUsize = AtomicUsize::new(0);

const AUTHOR: [&str; 8] = [
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@t",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.hooksPath=/dev/null",
];

const REPOSITORY_VARIABLES: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

pub(crate) struct Scratch {
    root: String,
}

impl Scratch {
    pub(crate) fn new(label: &str) -> Scratch {
        Scratch::first_free(std::iter::repeat_with(|| candidate(label)))
    }

    pub(super) fn first_free(candidates: impl IntoIterator<Item = PathBuf>) -> Scratch {
        for path in candidates {
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    let root = path.into_os_string().into_string().unwrap();
                    return Scratch { root };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }

        panic!("no scratch candidate was free")
    }

    pub(crate) fn path(&self) -> &str {
        &self.root
    }

    pub(crate) fn join(&self, name: &str) -> String {
        format!("{}/{name}", self.root)
    }

    pub(crate) fn write(&self, name: &str, text: &str) {
        let path = PathBuf::from(self.join(name));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        open_up(Path::new(&self.root));
        let Err(error) = fs::remove_dir_all(&self.root) else {
            return;
        };

        if std::thread::panicking() {
            eprintln!("cannot remove {}: {error}", self.root);
        } else {
            panic!("cannot remove {}: {error}", self.root);
        }
    }
}

pub(super) fn candidate(label: &str) -> PathBuf {
    let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
    let count = CREATED.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("stanza-{label}-{}-{count}", std::process::id()))
}

fn open_up(dir: &Path) {
    if !fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return;
    }

    if fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).is_err() {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        open_up(&entry.path());
    }
}

pub(crate) fn git(dir: &str, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for variable in REPOSITORY_VARIABLES {
        command.env_remove(variable);
    }

    let output = command.args(args).current_dir(dir).output().unwrap();

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub(crate) fn authored_git(dir: &str, args: &[&str]) -> String {
    git(dir, &[&AUTHOR[..], args].concat())
}

pub(crate) fn commit(dir: &str, message: &str) {
    authored_git(dir, &["commit", "-qm", message]);
}
