use super::args::ExplainArguments;
use super::report::printed_path;
use super::{Io, Program, required_cwd};
use crate::engine::explain::{Request, explain};
use crate::engine::types::Braces;
use crate::files::{
    Repositories, SkipReason, StdinTarget, error_code, is_generated_header, realpath, stdin_target,
};
use crate::paths::resolve;
use crate::step::{decode, without_mark};

pub(crate) fn run_explain(args: &ExplainArguments, io: &mut Io) -> i32 {
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let target = stdin_target(&args.path, &cwd, &mut Repositories::default());
    if let StdinTarget::Unsupported(error)
    | StdinTarget::Unreadable(error)
    | StdinTarget::Failed(error) = &target
    {
        io.error(Program::Stanza, error);
        return 2;
    }

    let supplied = resolve(&cwd, &args.path);
    let regular = std::fs::metadata(&supplied).and_then(|meta| {
        if meta.is_file() {
            std::fs::read(&supplied).map(Some)
        } else {
            Ok(None)
        }
    });

    let bytes = match regular {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            io.error(
                Program::Stanza,
                format_args!("not a regular file: {}", args.path),
            );
            return 2;
        }
        Err(error) => {
            let failure = match error_code(&error) {
                Some("ENOENT") => format!("no such file: {}", args.path),
                code => format!(
                    "cannot read {}: {}",
                    args.path,
                    code.map_or_else(|| error.to_string(), str::to_owned)
                ),
            };

            io.error(Program::Stanza, failure);
            return 2;
        }
    };

    let text = match decode(bytes) {
        Ok(text) => text,
        Err(message) => {
            io.error(Program::Stanza, format_args!("{}: {message}", args.path));
            return 2;
        }
    };

    let (root, path) = match (realpath(&cwd), realpath(&supplied)) {
        (Ok(root), Ok(path)) => (root, path),
        (Err(error), _) | (_, Err(error)) => {
            io.error(Program::Stanza, error);
            return 2;
        }
    };

    let body = without_mark(&text);
    let display = |path: &str| printed_path(path, Some(&root));
    let request = Request {
        path: &path,
        text: body,
        braces: args.no_braces.then_some(Braces::Off),
        display: &display,
    };

    let result = explain(&request, &[args.line])
        .and_then(|lines| lines.into_iter().next().expect("one line was asked"));

    let explained = match result {
        Ok(explained) => explained,
        Err(error) => {
            io.error(Program::Stanza, error);
            return 2;
        }
    };

    for line in &explained.lines {
        io.out(format!("{line}\n").as_bytes());
    }

    if target == StdinTarget::Skip(SkipReason::OptedOut) {
        io.out(b"\nnote: --fix and --check skip this file, its stanza attribute opts it out\n");
    } else if matches!(target, StdinTarget::Skip(_)) || is_generated_header(body) {
        io.out(b"\nnote: --fix and --check skip this file as generated or excluded\n");
    }

    if explained.found { 0 } else { 1 }
}
