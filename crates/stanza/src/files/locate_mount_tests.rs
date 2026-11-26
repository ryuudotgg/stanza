use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

use super::{INHERITED, Location, directory, initialized, shown};
use crate::paths::join;
use crate::testing::scratch::Scratch;

struct Volume {
    host: Option<Scratch>,
    root: String,
    mountpoint: String,
    device: Option<String>,
}

fn selected_volume() -> Option<Volume> {
    let selected = std::env::var_os("CI").is_some()
        || std::env::var_os("STANZA_TEST_MOUNTS").is_some_and(|value| value == "1");

    selected.then(Volume::attach)
}

fn succeed(command: &[&str]) -> String {
    let output = Command::new(command[0])
        .args(&command[1..])
        .output()
        .unwrap();

    assert!(output.status.success(), "{command:?}: {}", shown(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

impl Volume {
    fn attach() -> Volume {
        let host = Scratch::new("mount");
        let root = initialized(&host.join("repo"));
        let mountpoint = directory(&join(&root, "mnt"));
        let mut volume = Volume {
            host: Some(host),
            root,
            mountpoint,
            device: None,
        };

        volume.device = volume.mount();
        assert!(volume.mounted(), "nothing mounted at {}", volume.mountpoint);
        directory(&volume.start());
        volume
    }

    fn host(&self) -> &Scratch {
        self.host.as_ref().unwrap()
    }

    fn start(&self) -> String {
        join(&self.mountpoint, "inner")
    }

    fn mounted(&self) -> bool {
        let device = |path: &str| fs::metadata(path).map(|meta| meta.dev());
        matches!((device(&self.mountpoint), device(&self.root)), (Ok(inner), Ok(outer)) if inner != outer)
    }

    #[cfg(target_os = "macos")]
    fn mount(&self) -> Option<String> {
        let image = self.host().join("volume.dmg");
        succeed(&[
            "hdiutil", "create", "-size", "2m", "-fs", "APFS", "-quiet", &image,
        ]);
        let attached = succeed(&[
            "hdiutil",
            "attach",
            "-nobrowse",
            "-mountpoint",
            &self.mountpoint,
            &image,
        ]);

        let disk = attached
            .split_whitespace()
            .next()?
            .strip_prefix("/dev/disk")?;
        let number: String = disk.chars().take_while(char::is_ascii_digit).collect();
        Some(format!("/dev/disk{number}"))
    }

    #[cfg(not(target_os = "macos"))]
    fn mount(&self) -> Option<String> {
        let options = format!("size=2m,uid={},gid={}", unsafe { libc::getuid() }, unsafe {
            libc::getgid()
        });
        succeed(&[
            "sudo",
            "-n",
            "mount",
            "-t",
            "tmpfs",
            "-o",
            &options,
            "tmpfs",
            &self.mountpoint,
        ]);
        None
    }

    fn detachments(&self) -> [Vec<&str>; 2] {
        let target = self.device.as_deref().unwrap_or(&self.mountpoint);
        if cfg!(target_os = "macos") {
            [
                vec!["hdiutil", "detach", "-quiet", target],
                vec!["hdiutil", "detach", "-quiet", "-force", target],
            ]
        } else {
            [
                vec!["sudo", "-n", "umount", &self.mountpoint],
                vec!["sudo", "-n", "umount", "-l", &self.mountpoint],
            ]
        }
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        if self.device.is_none() && !self.mounted() {
            return;
        }

        let mut refused = String::new();
        for command in self.detachments() {
            match Command::new(command[0]).args(&command[1..]).output() {
                Ok(output) if output.status.success() && !self.mounted() => return,
                Ok(output) => refused.push_str(&format!(
                    "\n{command:?} {}: {}",
                    output.status,
                    shown(&output)
                )),
                Err(error) => refused.push_str(&format!("\n{command:?}: {error}")),
            }
        }

        std::mem::forget(self.host.take());
        let message = format!(
            "cannot detach the volume at {}, so its folder stays: {refused}",
            self.mountpoint
        );

        if std::thread::panicking() {
            eprintln!("{message}");
        } else {
            panic!("{message}");
        }
    }
}

#[test]
fn a_mounted_filesystem_stops_discovery_where_git_stops() {
    let Some(volume) = selected_volume() else {
        return;
    };

    let located = INHERITED.expect(&volume.start(), 1);
    assert!(
        matches!(&located, Location::Failed(error) if error.contains("not a git repository")),
        "{located:?}"
    );
}

#[test]
fn git_discovery_across_filesystem_crosses_a_mounted_filesystem_like_git() {
    let Some(volume) = selected_volume() else {
        return;
    };

    let environment = INHERITED.with("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1");
    assert_eq!(
        environment.expect(&volume.start(), 1),
        Location::Repository(volume.root.clone())
    );
}

#[test]
fn a_mounted_filesystem_with_no_repository_above_is_outside_without_discovery() {
    let Some(volume) = selected_volume() else {
        return;
    };

    fs::rename(join(&volume.root, ".git"), volume.host().join("git-aside")).unwrap();
    let start = volume.start();

    assert_eq!(INHERITED.expect_outside(&start, 0), Location::Outside);
    let environment = INHERITED.with("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1");
    assert_eq!(environment.expect_outside(&start, 1), Location::Outside);
}
