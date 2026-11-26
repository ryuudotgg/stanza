mod batch;
mod catalog;

use std::io::Write;
use std::process::ExitCode;

use mimalloc::MiMalloc;
use serde_json::json;
use stanza::files::{GIT_MISSING, Repositories, collect_files, has_git, is_generated_header};
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

const USAGE: &str = "Usage: stanza-dev batch\n       stanza-dev select [--keep-generated] <paths...>\n       stanza-dev catalog";

fn run(args: &[String], cwd: &str) -> u8 {
    let printed = match args.split_first() {
        Some((command, rest)) if command == "select" => select(rest, cwd),
        Some((command, [])) if command == "catalog" => Ok(catalog::catalog()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_batch_select_and_catalog_are_commands() {
        let named: Vec<&str> = USAGE
            .lines()
            .filter_map(|line| line.split_once("stanza-dev ")?.1.split_whitespace().next())
            .collect();

        assert_eq!(named, ["batch", "select", "catalog"]);

        for retired in ["changed", "stdin-target", "locate", "workers", "nonsense"] {
            assert_eq!(run(&[retired.to_owned()], "."), 2, "{retired}");
        }
    }
}
