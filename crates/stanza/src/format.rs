use std::any::Any;
use std::cell::Cell;
use std::collections::hash_map::RandomState;
use std::ffi::CString;
use std::fs::{self, Metadata, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use crate::engine::hunks::Changed;
use crate::engine::rules::Rule;
use crate::engine::types::{Braces, Finding, Mode, compare_findings};
use crate::files::{error_code, with_code};
use crate::step::{Decoded, Unread, decode, format_text};

const CHANGED: &str = "changed while stanza ran, left as it is";

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
    pub lines: Option<Changed>,
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
    pub rewritten: Vec<String>,
    pub unread: Unread,
    pub crashed: Vec<String>,
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

enum Written {
    Fixed,
    Changed,
}

fn unchanged(real: &Path, read: &str) -> io::Result<bool> {
    Ok(fs::read(real)? == read.as_bytes())
}

fn adopt_temp(
    temp: &Path,
    real: &Path,
    target: &Metadata,
    read: &str,
) -> io::Result<Option<Written>> {
    let created = fs::metadata(temp)?;
    if created.uid() != target.uid() || created.gid() != target.gid() {
        return Ok(None);
    }

    fs::set_permissions(temp, fs::Permissions::from_mode(target.mode() & 0o7777))?;
    if !unchanged(real, read)? {
        return Ok(Some(Written::Changed));
    }

    fs::rename(temp, real)?;
    Ok(Some(Written::Fixed))
}

fn replace_file(
    real: &Path,
    text: &str,
    target: &Metadata,
    read: &str,
) -> io::Result<Option<Written>> {
    let temp = temp_path(real);
    if let Err(error) = create_temp(&temp, text) {
        let _ = fs::remove_file(&temp);
        return match error_code(&error) {
            Some("EACCES" | "EPERM") => Ok(None),
            _ => Err(error),
        };
    }

    let adopted = adopt_temp(&temp, real, target, read);
    if !matches!(adopted, Ok(Some(Written::Fixed))) {
        let _ = fs::remove_file(&temp);
    }

    adopted
}

fn write_in_place(real: &Path, text: &str, read: &str) -> io::Result<Written> {
    let original = fs::read(real)?;
    if original != read.as_bytes() {
        return Ok(Written::Changed);
    }

    fs::write(real, text).inspect_err(|_| {
        let _ = fs::write(real, original);
    })?;

    Ok(Written::Fixed)
}

fn writable(real: &Path) -> io::Result<()> {
    let path = CString::new(real.as_os_str().as_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn write_file(path: &str, text: &str, read: &str) -> io::Result<Written> {
    let real = fs::canonicalize(path)?;
    let target = fs::metadata(&real)?;
    writable(&real)?;

    if target.nlink() == 1
        && let Some(written) = replace_file(&real, text, &target, read)?
    {
        return Ok(written);
    }

    write_in_place(&real, text, read)
}

fn write_fixed(path: &str, text: &str, read: &str, output: &str) -> Option<Finding> {
    let message = match write_file(path, text, read) {
        Ok(Written::Fixed) => return None,
        Ok(Written::Changed) => CHANGED.to_owned(),
        Err(error) => with_code("cannot write", &error),
    };

    Some(Finding {
        path: output.to_owned(),
        line: 1,
        col: 1,
        rule: Rule::Write,
        message,
        fixable: false,
    })
}

thread_local! {
    static CATCHING: Cell<bool> = const { Cell::new(false) };
}

pub fn silence_caught_panics() {
    let report = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if !CATCHING.get() {
            report(info);
        }
    }));
}

pub fn panic_text(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn catch_panic<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    let outer = CATCHING.replace(true);
    let caught = panic::catch_unwind(AssertUnwindSafe(work));
    CATCHING.set(outer);
    caught.map_err(|payload| panic_text(&*payload))
}

pub struct FileOutcome {
    findings: Vec<Finding>,
    failed: bool,
    generated: bool,
    kept: Option<String>,
    rewritten: Option<String>,
    unread: Unread,
    crashed: Option<String>,
}

#[cfg(test)]
pub const PANICS: &str = "panics.ts";

fn crashed(output: String, panic: &str, unread: Unread) -> FileOutcome {
    FileOutcome {
        crashed: Some(format!(
            "internal error on {output}, left it untouched: {panic}"
        )),
        findings: vec![Finding {
            path: output,
            line: 1,
            col: 1,
            rule: Rule::Error,
            message: format!("internal error ({panic}), left untouched"),
            fixable: false,
        }],
        failed: true,
        generated: false,
        kept: None,
        rewritten: None,
        unread,
    }
}

pub fn format_file(input: Input, run: &Run) -> FileOutcome {
    let mut unread = Unread::default();
    let caught = catch_panic(|| {
        #[cfg(test)]
        assert!(!input.path.ends_with(PANICS), "injected panic");
        format_text(
            &input.path,
            &input.text,
            run.mode,
            run.braces,
            &mut unread,
            input.lines,
        )
    });

    let result = match caught {
        Ok(result) => result,
        Err(panic) => return crashed(input.output, &panic, unread),
    };

    let mut outcome = FileOutcome {
        findings: result
            .findings
            .into_iter()
            .map(|finding| Finding {
                path: input.output.clone(),
                ..finding
            })
            .collect(),
        failed: result.parse_error,
        generated: result.generated,
        kept: None,
        rewritten: None,
        unread,
        crashed: None,
    };

    let (Some(fixed), Ok(read)) = (result.fixed, &input.text) else {
        return outcome;
    };

    if !run.write {
        outcome.kept = Some(fixed);
        return outcome;
    }

    match write_fixed(&input.path, &fixed, read, &input.output) {
        Some(unwritten) => {
            outcome.findings.push(unwritten);
            outcome.failed = true;
        }
        None => outcome.rewritten = Some(input.output),
    }

    outcome
}

pub fn merge_outcomes(outcomes: Vec<FileOutcome>) -> Formatted {
    let count = outcomes.len();
    let mut formatted = Formatted {
        findings: Vec::new(),
        failed: false,
        only_generated: false,
        kept: None,
        rewritten: Vec::new(),
        unread: Unread::default(),
        crashed: Vec::new(),
    };

    let mut generated = 0;
    for outcome in outcomes {
        formatted.findings.extend(outcome.findings);
        formatted.failed |= outcome.failed;
        generated += usize::from(outcome.generated);
        formatted.kept = outcome.kept.or(formatted.kept);
        formatted.rewritten.extend(outcome.rewritten);
        formatted.unread.braces.extend(outcome.unread.braces);
        formatted.unread.width.extend(outcome.unread.width);
        formatted.crashed.extend(outcome.crashed);
    }

    formatted.findings.sort_by(compare_findings);
    formatted.only_generated = generated > 0 && generated == count;
    formatted
}

pub fn format_inputs(inputs: impl Iterator<Item = Input>, run: &Run) -> Formatted {
    merge_outcomes(inputs.map(|input| format_file(input, run)).collect())
}

pub fn hard_linked(path: &str, run: &Run) -> bool {
    run.mode == Mode::Fix && run.write && fs::metadata(path).is_ok_and(|meta| meta.nlink() > 1)
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
        fix_after(file, |_| {})
    }

    fn fix_after(file: &Path, between: impl FnOnce(&Path)) -> Formatted {
        let path = file.to_string_lossy().into_owned();
        let input = Input {
            output: "a.ts".to_owned(),
            text: read_text(&path),
            path,
            lines: None,
        };

        between(file);
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
    fn a_caught_panic_skips_the_panic_hook_and_reads_as_one_line() {
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let report = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if info.payload_as_str() == Some("caught\n  on two lines") {
                REPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            report(info);
        }));

        silence_caught_panics();
        let caught: Result<(), String> = catch_panic(|| panic!("caught\n  on two lines"));
        assert_eq!(caught, Err("caught on two lines".to_owned()));
        assert!(!REPORTED.load(std::sync::atomic::Ordering::Relaxed));
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

    fn written(formatted: &Formatted) -> Vec<&str> {
        formatted
            .findings
            .iter()
            .filter(|finding| finding.rule == Rule::Write)
            .map(|finding| finding.message.as_str())
            .collect()
    }

    #[test]
    fn a_file_edited_after_the_read_keeps_the_edit() {
        let dir = scratch("raced");
        let file = dir.join("a.ts");
        fs::write(&file, SPREAD).unwrap();

        let formatted = fix_after(&file, |file| fs::write(file, "edited\n").unwrap());
        assert!(formatted.failed);
        assert!(formatted.rewritten.is_empty());
        assert_eq!(written(&formatted), [CHANGED]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "edited\n");
        assert_eq!(listing(&dir), ["a.ts"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_edit_keeping_size_and_mtime_is_still_kept() {
        let dir = scratch("raced-same");
        let file = dir.join("a.ts");
        fs::write(&file, SPREAD).unwrap();
        let edited = SPREAD.replace("log", "use");

        let formatted = fix_after(&file, |file| {
            let modified = fs::metadata(file).unwrap().modified().unwrap();
            fs::write(file, &edited).unwrap();
            fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(modified)
                .unwrap();

            let now = fs::metadata(file).unwrap();
            assert_eq!(now.len(), SPREAD.len() as u64);
            assert_eq!(now.modified().unwrap(), modified);
        });

        assert_eq!(written(&formatted), [CHANGED]);
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hard_linked_file_edited_after_the_read_keeps_the_edit() {
        let dir = scratch("raced-link");
        let file = dir.join("a.ts");
        let twin = dir.join("b.txt");
        fs::write(&file, SPREAD).unwrap();
        fs::hard_link(&file, &twin).unwrap();

        let formatted = fix_after(&file, |file| fs::write(file, "edited\n").unwrap());
        assert!(formatted.failed);
        assert_eq!(written(&formatted), [CHANGED]);
        assert_eq!(fs::read_to_string(&twin).unwrap(), "edited\n");
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
