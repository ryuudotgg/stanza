use super::args::ExplainArguments;
use super::report::printed_path;
use super::{Io, Program, required_cwd};
use crate::engine::explain::{Request, explain};
use crate::files::{
    EmptyReason, Repositories, collect_files, empty_reasons, is_generated_header, missing,
    realpath, reason, supplied,
};
use crate::languages::entry_for;
use crate::paths::shown;
use crate::step::{decode, without_mark};

pub(crate) fn run_explain(args: &ExplainArguments, io: &mut Io) -> i32 {
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let supplied = supplied(&cwd, &args.path);
    if entry_for(&supplied).is_none() {
        io.error(
            Program::Stanza,
            format_args!("not a TypeScript or JavaScript file: {}", shown(&args.path)),
        );
        return 2;
    }

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
                format_args!("not a regular file: {}", shown(&args.path)),
            );
            return 2;
        }
        Err(error) => {
            let failure = if missing(&error) {
                format!("no such file: {}", shown(&args.path))
            } else {
                format!("cannot read {}: {}", shown(&args.path), reason(&error))
            };

            io.error(Program::Stanza, failure);
            return 2;
        }
    };

    let named = std::slice::from_ref(&args.path);
    let mut repositories = Repositories::new(Some(&cwd));
    let selection = collect_files(named, &cwd, false, &mut repositories);
    if !selection.errors.is_empty() {
        for error in &selection.errors {
            io.error(Program::Stanza, error);
        }

        return 2;
    }

    for warning in &selection.warnings {
        io.notice(Program::Stanza, warning);
    }

    let skipped = selection.files.is_empty() && selection.invalid_names.is_empty();
    let reasons = if skipped {
        empty_reasons(named, &cwd, &selection, &mut repositories).unwrap_or_default()
    } else {
        Vec::new()
    };

    let opted_out = reasons.first() == Some(&Some(EmptyReason::OptedOut));

    let text = match decode(bytes) {
        Ok(text) => text,
        Err(message) => {
            io.error(
                Program::Stanza,
                format_args!("{}: {message}", shown(&args.path)),
            );
            return 2;
        }
    };

    let (root, path) = match (realpath(&cwd), realpath(&supplied)) {
        (Ok(root), Ok(path)) => (root, path),
        (Err(error), _) => {
            io.error(
                Program::Stanza,
                format_args!("cannot read the working directory: {}", reason(&error)),
            );
            return 2;
        }
        (_, Err(error)) => {
            io.error(
                Program::Stanza,
                format_args!("cannot read {}: {}", shown(&args.path), reason(&error)),
            );
            return 2;
        }
    };

    let body = without_mark(&text);
    let display = |path: &str| shown(&printed_path(path, Some(&root))).into_owned();
    let request = Request {
        path: &path,
        text: body,
        braces: args.braces,
        display: &display,
    };

    let result = explain(&request, &[args.line])
        .map(|lines| lines.into_iter().next().expect("one line was asked"));

    let explained = match result {
        Ok(explained) => explained,
        Err(error) => {
            io.error(Program::Stanza, error);
            return 2;
        }
    };

    let explained = match explained {
        Ok(explained) => explained,
        Err(past) => {
            io.error(Program::Stanza, past.message(&args.typed_line));
            return 2;
        }
    };

    for line in &explained.lines {
        io.out(format!("{line}\n").as_bytes());
    }

    if opted_out {
        io.out(b"\nnote: --fix and --check skip this file, its stanza attribute opts it out\n");
    } else if skipped || is_generated_header(body) {
        io.out(b"\nnote: --fix and --check skip this file as generated or excluded\n");
    }

    if explained.found { 0 } else { 1 }
}
