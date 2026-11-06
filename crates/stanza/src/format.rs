use std::collections::hash_map::RandomState;
use std::ffi::CString;
use std::fs::{self, Metadata, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::engine::rules::Rule;
use crate::engine::types::{Braces, Finding, Mode, compare_findings};
use crate::files::{error_code, with_code};
use crate::step::{Decoded, decode, format_text};

pub fn read_text(path: &str) -> Decoded {
    match fs::read(path) {
        Ok(bytes) => decode(bytes),
        Err(error) => Err(with_code("cannot read", &error)),
    }
}

pub struct Input {
    pub path: String,
    pub output: String,
    pub text: Decoded,
}

pub struct Run {
    pub mode: Mode,
    pub braces: Option<Braces>,
    pub write: bool,
}

pub struct Formatted {
    pub findings: Vec<Finding>,
    pub failed: bool,
    pub only_generated: bool,
    pub kept: Option<String>,
    pub unread: Vec<String>,
}

fn temp_path(real: &Path) -> PathBuf {
    let random = RandomState::new().build_hasher().finish();
    let name = format!(".stanza-{}-{random:016x}.tmp", std::process::id());
    real.parent().unwrap_or(Path::new("/")).join(name)
}

fn create_temp(temp: &Path, text: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)?
        .write_all(text.as_bytes())
}

fn adopt_temp(temp: &Path, real: &Path, target: &Metadata) -> io::Result<bool> {
    let created = fs::metadata(temp)?;
    if created.uid() != target.uid() || created.gid() != target.gid() {
        return Ok(false);
    }

    fs::set_permissions(temp, fs::Permissions::from_mode(target.mode() & 0o7777))?;
    fs::rename(temp, real)?;
    Ok(true)
}

fn replace_file(real: &Path, text: &str, target: &Metadata) -> io::Result<bool> {
    let temp = temp_path(real);
    if let Err(error) = create_temp(&temp, text) {
        let _ = fs::remove_file(&temp);
        return match error_code(&error) {
            Some("EACCES" | "EPERM") => Ok(false),
            _ => Err(error),
        };
    }

    let adopted = adopt_temp(&temp, real, target);
    if !matches!(adopted, Ok(true)) {
        let _ = fs::remove_file(&temp);
    }

    adopted
}

fn write_in_place(real: &Path, text: &str) -> io::Result<()> {
    let original = fs::read(real)?;
    fs::write(real, text).inspect_err(|_| {
        let _ = fs::write(real, original);
    })
}

fn writable(real: &Path) -> io::Result<()> {
    let path = CString::new(real.as_os_str().as_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn write_file(path: &str, text: &str) -> io::Result<()> {
    let real = fs::canonicalize(path)?;
    let target = fs::metadata(&real)?;
    writable(&real)?;

    if target.nlink() > 1 || !replace_file(&real, text, &target)? {
        write_in_place(&real, text)?;
    }

    Ok(())
}

fn write_fixed(path: &str, text: &str, output: &str) -> Option<Finding> {
    let error = write_file(path, text).err()?;
    Some(Finding {
        path: output.to_owned(),
        line: 1,
        col: 1,
        rule: Rule::Write,
        message: with_code("cannot write", &error),
        fixable: false,
    })
}

pub fn format_inputs(inputs: impl ExactSizeIterator<Item = Input>, run: &Run) -> Formatted {
    let count = inputs.len();
    let mut findings = Vec::new();
    let mut kept = None;
    let mut failed = false;
    let mut generated = 0;
    let mut unread = Vec::new();
    for input in inputs {
        let result = format_text(&input.path, input.text, run.mode, run.braces, &mut unread);
        failed |= result.parse_error;
        generated += usize::from(result.generated);
        findings.extend(result.findings.into_iter().map(|finding| Finding {
            path: input.output.clone(),
            ..finding
        }));

        let Some(fixed) = result.fixed else {
            continue;
        };

        if !run.write {
            kept = Some(fixed);
            continue;
        }

        if let Some(unwritten) = write_fixed(&input.path, &fixed, &input.output) {
            findings.push(unwritten);
            failed = true;
        }
    }

    findings.sort_by(compare_findings);
    Formatted {
        findings,
        failed,
        only_generated: generated > 0 && generated == count,
        kept,
        unread,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n}\n";

    const SPACED: &str = "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n}\n";

    fn scratch(name: &str) -> PathBuf {
        let dir = temp_path(&std::env::temp_dir().join(name)).with_extension("dir");
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn fix(file: &Path) -> Formatted {
        let path = file.to_string_lossy().into_owned();
        let input = Input {
            output: "a.ts".to_owned(),
            text: read_text(&path),
            path,
        };

        let run = Run {
            mode: Mode::Fix,
            braces: Some(Braces::Off),
            write: true,
        };

        format_inputs([input].into_iter(), &run)
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        names.sort();
        names
    }

    #[test]
    fn a_replaced_file_keeps_its_mode_and_leaves_no_temp() {
        let dir = scratch("mode");
        let file = dir.join("a.ts");
        fs::write(&file, SPREAD).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();

        assert!(!fix(&file).failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), SPACED);
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o7777, 0o640);
        assert_eq!(listing(&dir), ["a.ts"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hard_link_stays_shared() {
        let dir = scratch("link");
        let file = dir.join("a.ts");
        let twin = dir.join("b.txt");
        fs::write(&file, SPREAD).unwrap();
        fs::hard_link(&file, &twin).unwrap();

        assert!(!fix(&file).failed);
        assert_eq!(fs::read_to_string(&twin).unwrap(), SPACED);
        assert_eq!(
            fs::metadata(&file).unwrap().ino(),
            fs::metadata(&twin).unwrap().ino()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_directory_is_written_in_place() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = scratch("locked");
        let file = dir.join("a.ts");
        fs::write(&file, SPREAD).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let formatted = fix(&file);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!formatted.failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), SPACED);
        assert_eq!(listing(&dir), ["a.ts"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_file_is_a_write_finding() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = scratch("readonly");
        let file = dir.join("a.ts");
        fs::write(&file, SPREAD).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        let formatted = fix(&file);
        let written: Vec<_> = formatted
            .findings
            .iter()
            .filter(|finding| finding.rule == Rule::Write)
            .map(|finding| finding.message.as_str())
            .collect();

        assert!(formatted.failed);
        assert_eq!(written, ["cannot write: EACCES"]);
        assert_eq!(fs::read_to_string(&file).unwrap(), SPREAD);
        fs::remove_dir_all(dir).unwrap();
    }
}
