use std::collections::HashSet;
use std::fs;

use super::git::{GitEnv, run_git_with};
use crate::paths::{join, resolve};

type Entry = (Vec<u8>, Option<Vec<u8>>);

enum Parsed {
    Entries(Vec<Entry>),
    Invalid,
    VersionDependent,
}

pub(super) fn submodule_paths(root: &str) -> HashSet<String> {
    let file = join(root, ".gitmodules");
    let Ok(meta) = fs::metadata(&file) else {
        return HashSet::new();
    };

    // A FIFO yields its bytes to one open only, so anything but a regular file stays with git's single read.
    if !meta.is_file() {
        return spawned_submodule_paths(root);
    }

    let Ok(bytes) = fs::read(&file) else {
        return HashSet::new();
    };

    // git matches the key pattern under the caller's locale, where `.` may not match a byte outside UTF-8.
    if std::str::from_utf8(&bytes).is_err() {
        return spawned_submodule_paths(root);
    }

    let entries = match config_entries(&bytes) {
        Parsed::Entries(entries) => entries,
        Parsed::Invalid => return HashSet::new(),
        Parsed::VersionDependent => return spawned_submodule_paths(root),
    };

    entries
        .into_iter()
        .filter_map(|(key, value)| {
            let key = c_string(key);
            names_path(&key).then(|| {
                let value = value.map_or(key, c_string);
                resolve(root, &String::from_utf8_lossy(&value))
            })
        })
        .collect()
}

fn spawned_submodule_paths(root: &str) -> HashSet<String> {
    let args = [
        "config",
        "-z",
        "--file",
        ".gitmodules",
        "--get-regexp",
        r"^submodule\..*\.path$",
    ];

    let Ok(output) = run_git_with(root, &args, None, GitEnv::Inherited) else {
        return HashSet::new();
    };

    listed_paths(root, &output.output)
}

fn listed_paths(root: &str, output: &[u8]) -> HashSet<String> {
    output
        .split(|&byte| byte == 0)
        .filter(|item| !item.is_empty())
        .filter_map(|item| {
            let value = match item.iter().position(|&byte| byte == b'\n') {
                Some(newline) => &item[newline + 1..],
                None => item,
            };

            std::str::from_utf8(value)
                .ok()
                .map(|value| resolve(root, value))
        })
        .collect()
}

fn names_path(key: &[u8]) -> bool {
    const PREFIX: &[u8] = b"submodule.";
    const SUFFIX: &[u8] = b".path";

    key.len() >= PREFIX.len() + SUFFIX.len() && key.starts_with(PREFIX) && key.ends_with(SUFFIX)
}

fn c_string(mut bytes: Vec<u8>) -> Vec<u8> {
    if let Some(end) = bytes.iter().position(|&byte| byte == 0) {
        bytes.truncate(end);
    }

    bytes
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

struct Source<'a> {
    bytes: &'a [u8],
    at: usize,
    eof: bool,
    version_dependent: bool,
}

impl Source<'_> {
    fn next(&mut self) -> u8 {
        let Some(&byte) = self.bytes.get(self.at) else {
            self.eof = true;
            return b'\n';
        };

        self.at += 1;
        if byte == b'\r' && self.bytes.get(self.at) == Some(&b'\n') {
            self.at += 1;
            return b'\n';
        }

        byte
    }
}

// A port of git's config.c parser for one file with includes off, as `git config --file` reads it.
fn config_entries(bytes: &[u8]) -> Parsed {
    const BOM: &[u8] = b"\xef\xbb\xbf";

    let start = if bytes.starts_with(BOM) {
        BOM.len()
    } else if bytes.first() == Some(&BOM[0]) {
        return Parsed::Invalid;
    } else {
        0
    };

    let mut source = Source {
        bytes: &bytes[start..],
        at: 0,
        eof: false,
        version_dependent: false,
    };

    match entries(&mut source) {
        None => Parsed::Invalid,
        Some(_) if source.version_dependent => Parsed::VersionDependent,
        Some(entries) => Parsed::Entries(entries),
    }
}

fn entries(source: &mut Source) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut name = Vec::new();
    let mut base_len = 0;
    let mut comment = false;
    loop {
        let byte = source.next();
        if byte == b'\n' {
            if source.eof {
                return Some(entries);
            }

            comment = false;
            continue;
        }

        if comment || is_space(byte) {
            continue;
        }

        if byte == b'#' || byte == b';' {
            comment = true;
            continue;
        }

        if byte == b'[' {
            name.clear();
            if !section(source, &mut name) || name.is_empty() {
                return None;
            }

            name.push(b'.');
            base_len = name.len();
            continue;
        }

        if !byte.is_ascii_alphabetic() {
            return None;
        }

        name.truncate(base_len);
        name.push(byte.to_ascii_lowercase());
        let value = entry_value(source, &mut name)?;
        entries.push((name.clone(), value));
    }
}

fn section(source: &mut Source, name: &mut Vec<u8>) -> bool {
    loop {
        let byte = source.next();
        if source.eof {
            return false;
        }

        if byte == b']' {
            return true;
        }

        if is_space(byte) {
            return subsection(source, name, byte);
        }

        if !is_key_char(byte) && byte != b'.' {
            return false;
        }

        name.push(byte.to_ascii_lowercase());
    }
}

fn subsection(source: &mut Source, name: &mut Vec<u8>, mut byte: u8) -> bool {
    while is_space(byte) {
        if byte == b'\n' {
            return false;
        }

        byte = source.next();
    }

    if byte != b'"' {
        return false;
    }

    name.push(b'.');
    loop {
        let mut byte = source.next();
        if byte == b'\n' {
            return false;
        }

        if byte == b'"' {
            break;
        }

        if byte == b'\\' {
            byte = source.next();
            if byte == b'\n' {
                return false;
            }
        }

        name.push(byte);
    }

    source.next() == b']'
}

fn entry_value(source: &mut Source, name: &mut Vec<u8>) -> Option<Option<Vec<u8>>> {
    let mut byte = source.next();
    while !source.eof && is_key_char(byte) {
        name.push(byte.to_ascii_lowercase());
        byte = source.next();
    }

    while byte == b' ' || byte == b'\t' {
        byte = source.next();
    }

    if byte == b'\n' {
        return Some(None);
    }

    if byte != b'=' {
        return None;
    }

    value(source).map(Some)
}

fn value(source: &mut Source) -> Option<Vec<u8>> {
    let mut value = Vec::new();
    let mut quote = false;
    let mut comment = false;
    let mut trim_len = 0;
    loop {
        let byte = source.next();
        if byte == b'\n' {
            if quote {
                return None;
            }

            if trim_len > 0 {
                value.truncate(trim_len);
            }

            return Some(value);
        }

        if comment {
            continue;
        }

        if is_space(byte) && !quote {
            if trim_len == 0 {
                trim_len = value.len();
            }

            // git before 2.46 stores each unquoted whitespace byte inside a value as a space; later versions keep the byte.
            if !value.is_empty() {
                source.version_dependent |= byte != b' ';
                value.push(byte);
            }

            continue;
        }

        if !quote && (byte == b';' || byte == b'#') {
            comment = true;
            continue;
        }

        trim_len = 0;
        if byte == b'\\' {
            let escaped = match source.next() {
                b'\n' => continue,
                b't' => b'\t',
                b'b' => 0x08,
                b'n' => b'\n',
                byte @ (b'\\' | b'"') => byte,
                _ => return None,
            };

            value.push(escaped);
            continue;
        }

        if byte == b'"' {
            quote = !quote;
            continue;
        }

        value.push(byte);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const FIXTURES: [&[u8]; 48] = [
        b"[submodule \"a\"]\n\tpath = vendor/a\n\turl = ../a\n",
        b"[submodule \"a\"]\n\tpath = vendor/a\n[submodule \"b\"]\n\tpath = vendor/b\n",
        b"[submodule \"a\"]\n\tpath = one\n[submodule \"a\"]\n\tpath = two\n\tpath = three\n",
        b"[Submodule \"MixedCase\"]\n\tPATH = up\n",
        b"[submodule.Old]\n\tpath = old\n",
        b"[submodule \"a\"] path = same-line\n",
        b"[submodule \"a\"]\n\tpath = \"quoted path\" \n",
        b"[submodule \"a\"]\n\tpath = \"  spaced  \"  tail  \n",
        b"[submodule \"a\"]\n\tpath = a\\\\b\\\"c\\td\\ne\\bf\n",
        b"[submodule \"a\"]\n\tpath = bad\\escape\n",
        b"[submodule \"a\"]\n\tpath = con\\\ntinued\n",
        b"[submodule \"a\"]\n\tpath = \"con\\\ntinued in quotes\"\n",
        b"[submodule \"a\"]\n\tpath = trailing\\",
        b"[submodule \"a\"]\n\tpath = value # comment\n\tpath = other ; comment\n",
        b"[submodule \"a\"]\n\tpath = \"inside # not a comment\"\n",
        b"# leading comment\n; another\n[submodule \"a\"] ; header comment\n\tpath = a\n",
        b"[submodule \"a\"]\n\tpath = \"unterminated\n",
        b"[submodule \"a\"]\n\tpath\n",
        b"[submodule \"a\"]\n\tpath =\n",
        b"[submodule \"a\"]\n\tpath = no newline at end",
        b"[submodule \"a\"]\r\n\tpath = crlf\r\n",
        b"[submodule \"a\"]\n\tpath = lone\rreturn\n",
        b"\xef\xbb\xbf[submodule \"a\"]\n\tpath = bom\n",
        b"\xef\xbb[submodule \"a\"]\n\tpath = partial bom\n",
        b"[submodule \"esc\\\"aped\\\\sub\\x\"]\n\tpath = sub\n",
        b"[submodule \"a\"\n\tpath = broken header\n",
        b"[submodule \"a\" ]\n\tpath = space before bracket\n",
        b"[submodule   \"a\"]\n\tpath = spaces before quote\n",
        b"[submodule\t\"a\"]\n\tpath = tab before quote\n",
        b"[]\n\tpath = empty header\n",
        b"[sub_module \"a\"]\n\tpath = bad section char\n",
        b"path = before any section\n[submodule \"a\"]\n\tpath = after\n",
        b"[submodule]\n\tpath = no subsection\n",
        b"[submodule \"a.path\"]\n\turl = x\n",
        b"[submodule \"a\"]\n\tpath-x = not path\n\tmypath = not path\n",
        b"[submodule \"a\"]\n\t1path = digit first\n",
        b"[submodule \"a\"]\n\tpath = vt\x0bff\x0c\n",
        b"[submodule \"a\"]\n\tpath =\x0b leading vt\n",
        b"[submodule \"a\"]\n\tpath = nul\0after\n",
        b"[submodule \"nul\0sub\"]\n\tpath = x\n",
        b"[submodule \"a\"]\n\tpath = \xe6\x97\xa5\xe6\x9c\xac\n",
        b"[submodule \"a\"]\n\tpath = \xef\xbb\xbfbom first\n",
        b"[submodule \"a\"]\n\tpath = tab\tinside\n",
        b"[submodule \"a\"]\n\tpath = \"quoted\ttab\"\n",
        b"\xef\xbb\xbf[submodule \"a\"]\n\tpath = \xef\xbb\xbfbom twice\n",
        b"",
        b"\n\n   \n",
        b"[submodule \"a\"]\n\tpath = x\n[core]\n\tbare = false\n[submodule \"b\"]\n\tpath = y\n",
    ];

    fn scratch(name: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("stanza-gitmodules-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        fs::canonicalize(&dir)
            .expect("the scratch directory resolves")
            .to_string_lossy()
            .into_owned()
    }

    fn git_output(dir: &str) -> (Option<i32>, Vec<u8>) {
        let output = Command::new("git")
            .args([
                "-C",
                dir,
                "config",
                "-z",
                "--file",
                ".gitmodules",
                "--get-regexp",
            ])
            .arg(r"^submodule\..*\.path$")
            .output()
            .expect("git runs");

        (output.status.code(), output.stdout)
    }

    fn reader_output(bytes: &[u8]) -> Option<(Option<i32>, Vec<u8>)> {
        let entries = match config_entries(bytes) {
            Parsed::Entries(entries) => entries,
            Parsed::Invalid => return Some((Some(1), Vec::new())),
            Parsed::VersionDependent => return None,
        };

        let mut stdout = Vec::new();
        for (key, value) in entries {
            let key = c_string(key);
            if !names_path(&key) {
                continue;
            }

            stdout.extend(&key);
            if let Some(value) = value {
                stdout.push(b'\n');
                stdout.extend(c_string(value));
            }

            stdout.push(0);
        }

        Some((Some(if stdout.is_empty() { 1 } else { 0 }), stdout))
    }

    #[test]
    fn reads_gitmodules_as_git_config_does() {
        let dir = scratch("reader");
        for fixture in FIXTURES {
            fs::write(join(&dir, ".gitmodules"), fixture).expect("the fixture is written");
            let shown = String::from_utf8_lossy(fixture);
            let (code, stdout) = git_output(&dir);
            let failed = code.map(|code| code.min(1));
            if let Some(read) = reader_output(fixture) {
                assert_eq!(read, (failed, stdout), "{shown:?}");
            }

            assert_eq!(
                submodule_paths(&dir),
                spawned_submodule_paths(&dir),
                "{shown:?}"
            );
        }

        fs::remove_dir_all(&dir).expect("the scratch directory is removed");
    }

    #[test]
    fn unquoted_tabs_and_returns_inside_a_value_keep_the_git_reading() {
        let read = |bytes: &[u8]| matches!(config_entries(bytes), Parsed::VersionDependent);
        assert!(read(b"[submodule \"a\"]\n\tpath = tab\tinside\n"));
        assert!(read(b"[submodule \"a\"]\n\tpath = lone\rreturn\n"));
        assert!(!read(b"[submodule \"a\"]\n\tpath = \"quoted\ttab\"\n"));
        assert!(!read(b"[submodule \"a\"]\n\tpath = two  spaces\n"));
        assert!(!read(b"[submodule \"a\"]\n\tpath = x\r\n"));
    }

    #[test]
    fn a_file_outside_utf8_keeps_the_git_reading() {
        let dir = scratch("latin1");
        let file = b"[submodule \"caf\xe9\"]\n\tpath = caf\xe9\n";
        fs::write(join(&dir, ".gitmodules"), file).expect("the fixture is written");
        assert_eq!(submodule_paths(&dir), spawned_submodule_paths(&dir));
        fs::remove_dir_all(&dir).expect("the scratch directory is removed");
    }

    #[test]
    fn a_listed_name_outside_utf8_keeps_its_utf8_path() {
        let listed = b"submodule.caf\xe9.path\nvendor/sub\0submodule.b.path\nbad\xff\0";
        assert_eq!(
            listed_paths("/r", listed),
            HashSet::from(["/r/vendor/sub".to_owned()])
        );
    }
}
