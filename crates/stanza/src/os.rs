use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

// std prints an OS error as strerror's text followed by ` (os error N)`, and the words are all stanza shows.
pub fn reason(error: &io::Error) -> String {
    let text = error.to_string();
    let words = error
        .raw_os_error()
        .and_then(|code| text.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&text);

    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub fn realpath(path: &str) -> io::Result<String> {
    fs::canonicalize(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))
}

pub fn lstat_if_present(path: &str) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn access(path: impl AsRef<Path>, mode: libc::c_int) -> io::Result<()> {
    let path = c_path(path.as_ref())?;
    checked(unsafe { libc::access(path.as_ptr(), mode) })
}

pub fn effective_access(path: impl AsRef<Path>, mode: libc::c_int) -> io::Result<()> {
    let path = c_path(path.as_ref())?;
    checked(unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) })
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))
}

fn checked(status: libc::c_int) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch::Scratch;

    #[test]
    fn a_failure_reads_as_the_system_words_it() {
        let reasons = [libc::EACCES, libc::ENOENT, libc::EISDIR]
            .map(|code| reason(&io::Error::from_raw_os_error(code)));

        assert_eq!(
            reasons,
            [
                "permission denied",
                "no such file or directory",
                "is a directory",
            ]
        );
        assert_eq!(
            reason(&io::Error::other("a config value contains itself")),
            "a config value contains itself"
        );
    }

    #[test]
    fn os_helpers_keep_their_error_rules() {
        let scratch = Scratch::new("os-helpers");
        scratch.write("file", "");
        let file = scratch.join("file");

        let nul = access("a\0b", libc::F_OK).unwrap_err();
        assert_eq!(nul.raw_os_error(), Some(libc::ENOENT));
        let nul = effective_access("a\0b", libc::F_OK).unwrap_err();
        assert_eq!(nul.raw_os_error(), Some(libc::ENOENT));

        assert!(
            lstat_if_present(&scratch.join("missing"))
                .unwrap()
                .is_none()
        );
        assert!(
            lstat_if_present(&file)
                .unwrap()
                .is_some_and(|meta| meta.is_file())
        );

        let under_file = lstat_if_present(&scratch.join("file/x")).unwrap_err();
        assert_eq!(under_file.raw_os_error(), Some(libc::ENOTDIR));
    }
}
