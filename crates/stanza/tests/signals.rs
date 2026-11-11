use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n}\n";

const SPACED: &str = "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n}\n";

const INTERRUPTS: [(libc::c_int, &str); 3] = [
    (libc::SIGINT, "INT"),
    (libc::SIGTERM, "TERM"),
    (libc::SIGHUP, "HUP"),
];

const DEADLINE: Duration = Duration::from_secs(30);

const REPORTING_GIT: &str = r#"#!/usr/bin/perl
use POSIX qw(:signal_h);
my $mask = POSIX::SigSet->new;
sigprocmask(SIG_BLOCK, POSIX::SigSet->new, $mask);
open(my $out, '>>', $ENV{STANZA_SIGNAL_REPORT}) or die;
print $out "spawn @ARGV\n";
for my $name (qw(INT TERM HUP PIPE)) {
    my $signal = eval "SIG$name";
    my $action = POSIX::SigAction->new;
    sigaction($signal, undef, $action);
    printf $out "%s %s %s\n", $name, $mask->ismember($signal) ? 'blocked' : 'open', $action->handler;
}
exit 1;
"#;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stanza-signals-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir).unwrap();
    dir
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();

    names.sort();
    names
}

struct Paused {
    child: Child,
    lines: Receiver<String>,
}

impl Paused {
    fn start(dir: &Path, args: &[&str], point: &str, workers: &str) -> Paused {
        let mut child = Command::new(env!("CARGO_BIN_EXE_stanza"))
            .args(args)
            .current_dir(dir)
            .env("STANZA_PAUSE", point)
            .env("STANZA_WORKERS", workers)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let stderr = BufReader::new(child.stderr.take().unwrap());
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });

        Paused { child, lines }
    }

    fn until(&mut self, wanted: &str) -> Vec<String> {
        let deadline = Instant::now() + DEADLINE;
        let mut before = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line == wanted => return before,
                Ok(line) => before.push(line),
                Err(error) => {
                    let _ = self.child.kill();
                    panic!("no {wanted:?} after {before:?}: {error}");
                }
            }
        }
    }

    fn interrupt(&mut self, signal: libc::c_int) -> Vec<String> {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
        self.until("stanza: interrupted")
    }

    fn release(mut self) -> (ExitStatus, Vec<String>) {
        drop(self.child.stdin.take());

        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }

            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("stanza did not exit once released");
            }

            thread::sleep(Duration::from_millis(10));
        };

        (status, self.lines.iter().collect())
    }
}

#[cfg(debug_assertions)]
fn interrupt_while_paused(point: &str, linked: bool) {
    for (signal, name) in INTERRUPTS {
        let dir = scratch(&format!("{point}-{name}"));
        fs::write(dir.join("a.ts"), SPREAD).unwrap();
        if linked {
            fs::hard_link(dir.join("a.ts"), dir.join("b.ts")).unwrap();
        }

        let mut paused = Paused::start(&dir, &["--fix", "a.ts"], point, "0");
        assert!(
            paused
                .until(&format!("stanza: paused at {point}"))
                .is_empty()
        );
        assert!(paused.interrupt(signal).is_empty());

        let (status, _) = paused.release();
        assert_eq!(status.signal(), Some(signal), "{point} {name}: {status:?}");
        assert_eq!(
            fs::read_to_string(dir.join("a.ts")).unwrap(),
            SPACED,
            "{point} {name}"
        );

        let expected: &[&str] = if linked { &["a.ts", "b.ts"] } else { &["a.ts"] };
        assert_eq!(listing(&dir), expected, "{point} {name}");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(debug_assertions)]
#[test]
fn an_interrupt_after_the_temp_file_is_created_waits_for_the_replace() {
    interrupt_while_paused("temp", false);
}

#[cfg(debug_assertions)]
#[test]
fn an_interrupt_inside_a_write_in_place_waits_for_the_whole_file() {
    interrupt_while_paused("in-place", true);
}

#[cfg(debug_assertions)]
#[test]
fn an_interrupt_finishes_every_write_in_flight_and_starts_no_other() {
    let paused_line = "stanza: paused at temp";
    for (workers, files, in_flight) in [("0", 2, 1), ("2", 200, 3)] {
        let dir = scratch(&format!("in-flight-{workers}"));
        for index in 0..files {
            fs::write(dir.join(format!("f{index:03}.ts")), SPREAD).unwrap();
        }

        let mut paused = Paused::start(&dir, &["--fix", "."], "temp", workers);
        for _ in 0..in_flight {
            assert!(paused.until(paused_line).is_empty());
        }

        assert!(paused.interrupt(libc::SIGINT).is_empty());
        let (status, rest) = paused.release();
        assert!(!rest.iter().any(|line| line == paused_line), "{rest:?}");
        assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?} {rest:?}");

        let names = listing(&dir);
        assert_eq!(names.len(), files, "{names:?}");

        let fixed = names
            .iter()
            .map(|name| fs::read_to_string(dir.join(name)).unwrap())
            .filter(|text| {
                assert!(text == SPREAD || text == SPACED, "{text:?}");
                text == SPACED
            })
            .count();

        assert_eq!(fixed, in_flight, "{workers} workers");
        fs::remove_dir_all(dir).unwrap();
    }
}

fn signals_git_starts_with(name: &str, ignore_hangup: bool) -> Vec<String> {
    let dir = scratch(name);
    let bin = dir.join("bin");
    let work = dir.join("work");
    fs::create_dir(&bin).unwrap();
    fs::create_dir_all(work.join(".git")).unwrap();
    fs::write(work.join("a.ts"), SPREAD).unwrap();
    fs::write(bin.join("git"), REPORTING_GIT).unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();

    let report = dir.join("report");
    let mut command = Command::new(env!("CARGO_BIN_EXE_stanza"));
    command
        .args(["--check", "--changed"])
        .current_dir(&work)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("GIT_DIR", work.join(".git"))
        .env("STANZA_SIGNAL_REPORT", &report);

    if ignore_hangup {
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }

    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");

    let report = fs::read_to_string(&report).unwrap();
    fs::remove_dir_all(dir).unwrap();

    let (spawns, states): (Vec<&str>, Vec<&str>) =
        report.lines().partition(|line| line.starts_with("spawn "));

    assert!(
        spawns.contains(&"spawn rev-parse --show-toplevel"),
        "{spawns:?}"
    );
    assert!(
        spawns.iter().any(|line| line.starts_with("spawn -C ")),
        "{spawns:?}"
    );
    assert_eq!(states.len(), spawns.len() * 4, "{report}");
    states.into_iter().map(str::to_owned).collect()
}

#[test]
fn git_starts_with_the_default_signal_mask_and_dispositions() {
    for line in signals_git_starts_with("children", false) {
        assert!(line.ends_with(" open DEFAULT"), "{line}");
    }
}

#[test]
fn a_hangup_ignored_at_start_stays_ignored_for_git() {
    for line in signals_git_starts_with("children-nohup", true) {
        let expected = if line.starts_with("HUP ") {
            " open IGNORE"
        } else {
            " open DEFAULT"
        };
        assert!(line.ends_with(expected), "{line}");
    }
}
