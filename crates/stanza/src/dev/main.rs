mod batch;
mod catalog;

use std::io::Write;
use std::process::ExitCode;

use indexmap::IndexSet;
use mimalloc::MiMalloc;
use serde_json::{Value as Json, json};
use stanza::files::{
    Caller, GIT_MISSING, Location, Repositories, SkipReason, StdinTarget, collect_changed,
    collect_files, has_git, is_generated_header, stdin_target,
};
use stanza::paths::resolve;
use stanza::pool;

#[global_allocator]
static ALLOCATOR: MiMalloc = MiMalloc;

// The CLI skips these files while formatting, but batch formats whatever it is sent.
fn generated_header(path: &str) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|text| is_generated_header(&text))
}

fn select(args: &[String], cwd: &str) -> Result<String, String> {
    let keep_generated = args.first().is_some_and(|arg| arg == "--keep-generated");
    let paths = &args[usize::from(keep_generated)..];
    let paths = paths.strip_prefix(&["--".to_owned()]).unwrap_or(paths);
    let selected = collect_files(paths, cwd, keep_generated, &mut Repositories::new(None));
    let files: Vec<&String> = selected
        .files
        .iter()
        .filter(|file| keep_generated || !generated_header(&resolve(cwd, file)))
        .collect();

    let printed = json!({
        "files": files,
        "errors": selected.errors,
        "warnings": if has_git() { vec![] } else { vec![GIT_MISSING] },
        "submodules": selected.submodules,
        "optedOut": selected.opted_out,
    });

    Ok(format!("{printed}\n"))
}

fn changed(args: &[String], cwd: &str) -> Result<String, String> {
    let hunks = args.first().is_some_and(|flag| flag == "--hunks");
    let args = &args[usize::from(hunks)..];
    let written = match args.split_first() {
        None => None,
        Some((delimiter, paths)) if delimiter == "--" => {
            Some(paths.iter().cloned().collect::<IndexSet<String>>())
        }
        Some((unexpected, _)) => return Err(format!("unexpected argument {unexpected}")),
    };

    let mut repositories = Repositories::new(Some(cwd));
    let location = repositories.locate(cwd);
    let selected = collect_changed(
        location,
        &mut repositories,
        written.as_ref(),
        Caller::Cli(cwd),
        hunks,
    )?;
    let changed_lines: serde_json::Map<String, Json> = selected
        .changed_lines
        .iter()
        .map(|(path, changed)| {
            let lines = json!({ "lines": changed.lines, "deletedAfter": changed.deleted_after });
            (path.clone(), lines)
        })
        .collect();

    let printed = json!({
        "files": selected.files,
        "errors": selected.errors,
        "warnings": [],
        "submodules": selected.submodules,
        "changedLines": changed_lines,
    });

    Ok(format!("{printed}\n"))
}

fn stdin_targets(paths: &[String], cwd: &str) -> String {
    let mut out = String::new();
    for path in paths {
        let printed = match stdin_target(path, cwd, &mut Repositories::new(None)) {
            StdinTarget::Format {
                path,
                root: Some(root),
            } => {
                json!({ "status": "format", "path": path, "root": root })
            }
            StdinTarget::Format { path, root: None } => json!({ "status": "format", "path": path }),
            StdinTarget::Skip(reason) => json!({
                "status": "skip",
                "reason": match reason {
                    SkipReason::OptedOut => "opted out",
                    SkipReason::Excluded => "excluded",
                },
            }),
            StdinTarget::Unsupported(error) => json!({ "status": "unsupported", "error": error }),
            StdinTarget::Unreadable(error) | StdinTarget::Failed(error) => {
                json!({ "status": "failed", "error": error })
            }
        };

        out.push_str(&format!("{printed}\n"));
    }

    out
}

fn locate(dirs: &[String]) -> String {
    let mut repositories = Repositories::new(None);
    let mut out = String::new();
    for dir in dirs {
        let printed = match repositories.locate(dir) {
            Location::Repository(root) => json!({ "kind": "repository", "root": root }),
            Location::Outside => json!({ "kind": "outside" }),
            Location::Failed(error) => json!({ "kind": "failed", "error": error }),
        };

        out.push_str(&format!("{printed}\n"));
    }

    out
}

fn workers(args: &[String]) -> Result<String, String> {
    let (cores, counts) = match args {
        [flag, cores, counts @ ..] if flag == "--cores" => (
            cores
                .parse::<usize>()
                .map_err(|_| format!("--cores takes a whole number, not {cores}"))?,
            counts,
        ),
        counts => (pool::cores(), counts),
    };

    let workers =
        std::env::var_os("STANZA_WORKERS").map(|value| value.to_string_lossy().into_owned());
    let mut out = String::new();
    for count in counts {
        let files = count
            .parse::<usize>()
            .map_err(|_| format!("a file count is a whole number, not {count}"))?;
        let chosen = pool::worker_count(files, workers.as_deref(), cores);
        out.push_str(&format!(
            "{}\n",
            json!({ "files": files, "workers": chosen })
        ));
    }

    Ok(out)
}

const USAGE: &str = "Usage: stanza-dev batch\n       stanza-dev select [--keep-generated] <paths...>\n       stanza-dev changed [--hunks] [-- <written paths...>]\n       stanza-dev stdin-target <paths...>\n       stanza-dev locate <dirs...>\n       stanza-dev catalog\n       stanza-dev workers [--cores N] <file counts...>";

fn run(args: &[String], cwd: &str) -> u8 {
    let printed = match args.split_first() {
        Some((command, rest)) if command == "select" => select(rest, cwd),
        Some((command, rest)) if command == "changed" => changed(rest, cwd),
        Some((command, paths)) if command == "stdin-target" => Ok(stdin_targets(paths, cwd)),
        Some((command, dirs)) if command == "locate" => Ok(locate(dirs)),
        Some((command, [])) if command == "catalog" => Ok(catalog::catalog()),
        Some((command, rest)) if command == "workers" => workers(rest),
        Some((command, [])) if command == "batch" => batch::batch(cwd).map(|()| String::new()),
        _ => Err(String::new()),
    };

    match printed {
        Ok(text) => match std::io::stdout().write_all(text.as_bytes()) {
            Ok(()) => 0,
            Err(_) => 2,
        },
        Err(message) => {
            if !message.is_empty() {
                eprintln!("stanza-dev: {message}");
            }

            eprintln!("{USAGE}");
            2
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let cwd = std::env::current_dir()
        .expect("the working directory is readable")
        .to_string_lossy()
        .into_owned();

    ExitCode::from(pool::on_largest_stack(|_| run(&args, &cwd)))
}
