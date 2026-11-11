mod batch;
mod catalog;
mod nesting;

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread;

use indexmap::IndexSet;
use mimalloc::MiMalloc;
use serde_json::{Value as Json, json};
use stanza::files::{
    Location, Repositories, SkipReason, StdinTarget, collect_changed, collect_files, stdin_target,
};
use stanza::languages::javascript::config::brace_decisions;
use stanza::languages::javascript::config::evaluate::{ByteOffset, Origin, Value, origin};
use stanza::languages::javascript::config::module::Session;
use stanza::languages::javascript::config::width::{WidthSource, formatter_width};
use stanza::languages::javascript::entry::ENTRY;
use stanza::paths::{relative, resolve};
use stanza::pool;

#[global_allocator]
static ALLOCATOR: MiMalloc = MiMalloc;

const STACK_BYTES: usize = 1 << 30;

struct Dump<'a> {
    session: &'a Session,
    cwd: &'a str,
    out: String,
    path: Vec<String>,
    seen: HashMap<*const (), (Value, String)>,
    origins: Vec<String>,
    utf16: &'a mut HashMap<Rc<str>, Utf16Starts>,
}

struct Utf16Starts(Vec<(u32, u32)>);

impl Utf16Starts {
    fn new(source: &str) -> Self {
        let mut shrunk = 0;
        let mut ends = Vec::new();
        for (index, char) in source.char_indices() {
            if !char.is_ascii() {
                shrunk += char.len_utf8() - char.len_utf16();
                ends.push((to_u32(index + char.len_utf8()), to_u32(shrunk)));
            }
        }

        Self(ends)
    }

    fn at(&self, offset: ByteOffset) -> u32 {
        let passed = self.0.partition_point(|&(end, _)| end <= offset.0);
        offset.0 - passed.checked_sub(1).map_or(0, |last| self.0[last].1)
    }
}

fn to_u32(count: usize) -> u32 {
    u32::try_from(count).expect("a parsed source fits oxc's u32 offsets")
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

fn indented(items: &[String], depth: usize, open: &str, close: &str) -> String {
    if items.is_empty() {
        return format!("{open}{close}");
    }

    let inner = "  ".repeat(depth + 1);
    let lines: Vec<String> = items.iter().map(|item| format!("{inner}{item}")).collect();
    format!(
        "{open}\n{}\n{}{close}",
        lines.join(",\n"),
        "  ".repeat(depth)
    )
}

fn number(value: f64) -> String {
    if value.is_finite() {
        return dragonbox_ecma::Buffer::new()
            .format_finite(value)
            .to_owned();
    }

    let name = if value.is_nan() {
        "NaN"
    } else if value > 0.0 {
        "Infinity"
    } else {
        "-Infinity"
    };

    format!(r#"{{"$number": "{name}"}}"#)
}

impl Dump<'_> {
    fn scalar(value: &Value) -> Option<String> {
        Some(match value {
            Value::Undefined => r#"{"$undefined": true}"#.to_owned(),
            Value::Unknown => r#"{"$unknown": true}"#.to_owned(),
            Value::NoSetting => r#"{"$noSetting": true}"#.to_owned(),
            Value::Null => "null".to_owned(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => number(*value),
            Value::String(value) => quoted(value),
            Value::Array(_) | Value::Object(_) => return None,
        })
    }

    fn open(&mut self, depth: usize, count: usize, open: char) -> bool {
        self.out.push(open);
        if count == 0 {
            return false;
        }

        self.out.push('\n');
        self.indent(depth + 1);
        true
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
    }

    fn separate(&mut self, depth: usize, index: usize) {
        if index > 0 {
            self.out.push_str(",\n");
            self.indent(depth + 1);
        }
    }

    fn close(&mut self, depth: usize, filled: bool, close: char) {
        if filled {
            self.out.push('\n');
            self.indent(depth);
        }

        self.out.push(close);
    }

    fn utf16_start(&mut self, origin: &Origin) -> u32 {
        let session = self.session;
        self.utf16
            .entry(origin.file.clone())
            .or_insert_with(|| {
                let program = session
                    .module_at(&origin.file)
                    .and_then(|module| module.program)
                    .expect("an origin's module is parsed and cached");
                Utf16Starts::new(program.source_text)
            })
            .at(origin.start)
    }

    fn print(&mut self, value: &Value, depth: usize) {
        if let Some(text) = Self::scalar(value) {
            self.out.push_str(&text);
            return;
        }

        let identity = match value {
            Value::Array(items) => Rc::as_ptr(items).cast::<()>(),
            Value::Object(object) => Rc::as_ptr(object).cast::<()>(),
            _ => unreachable!("scalars print above"),
        };

        if let Some((_, first)) = self.seen.get(&identity) {
            let reference = format!(r#"{{"$ref": {}}}"#, quoted(first));
            self.out.push_str(&reference);
            return;
        }

        self.seen
            .insert(identity, (value.clone(), self.path.join(".")));

        match value {
            Value::Array(items) => {
                let filled = self.open(depth, items.len(), '[');
                for (index, item) in items.iter().enumerate() {
                    self.separate(depth, index);
                    self.path.push(index.to_string());
                    self.print(item, depth + 1);
                    self.path.pop();
                }

                self.close(depth, filled, ']');
            }
            Value::Object(object) => {
                let keys = object.keys();
                let filled = self.open(depth, keys.len(), '{');
                for (index, key) in keys.iter().enumerate() {
                    self.separate(depth, index);
                    self.path.push(key.clone());
                    if let Some(location) = origin(value, key) {
                        let file = relative(self.cwd, &location.file);
                        let start = self.utf16_start(&location);
                        self.origins.push(format!(
                            "{}: {}",
                            quoted(&self.path.join(".")),
                            quoted(&format!("{file}:{start}"))
                        ));
                    }

                    self.out.push_str(&quoted(key));
                    self.out.push_str(": ");
                    let item = object.get(self.session, key);
                    self.print(&item, depth + 1);
                    self.path.pop();
                }

                self.close(depth, filled, '}');
            }
            _ => unreachable!("scalars print above"),
        }
    }
}

fn eval(files: &[String], cwd: &str) -> String {
    let session = Session::default();
    let mut utf16 = HashMap::new();
    let entries: Vec<String> = files
        .iter()
        .map(|file| {
            let value = session.exported(&resolve(cwd, file), "default", &Rc::default());
            let mut dump = Dump {
                session: &session,
                cwd,
                out: String::new(),
                path: Vec::new(),
                seen: HashMap::new(),
                origins: Vec::new(),
                utf16: &mut utf16,
            };

            dump.print(&value, 2);
            let entry = [
                format!(r#""value": {}"#, dump.out),
                format!(r#""origins": {}"#, indented(&dump.origins, 2, "{", "}")),
            ];

            format!("{}: {}", quoted(file), indented(&entry, 1, "{", "}"))
        })
        .collect();

    indented(&entries, 0, "{", "}")
}

fn braces(dirs: &[String], cwd: &str) -> Result<String, String> {
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .into_owned();

    let (extensions, dirs): (Vec<Option<&str>>, _) = match dirs {
        [flag, extension, dirs @ ..] if flag == "--extension" => (
            vec![Some(extension.as_str()).filter(|extension| !extension.is_empty())],
            dirs,
        ),
        dirs => (
            std::iter::once(None)
                .chain(ENTRY.extensions.iter().map(|&extension| Some(extension)))
                .collect(),
            dirs,
        ),
    };

    let mut out = String::new();
    for dir in dirs {
        let mut decisions = serde_json::Map::new();
        for &extension in &extensions {
            let found = brace_decisions(dir, extension).map_err(|error| error.to_string())?;
            let found: Vec<Json> = found
                .iter()
                .map(|decision| {
                    json!({
                        "family": decision.family.name(),
                        "setting": decision.setting.name(),
                        "files": decision.files.iter().map(|file| relative(&cwd, file)).collect::<Vec<_>>(),
                    })
                })
                .collect();

            decisions.insert(extension.unwrap_or_default().to_owned(), Json::Array(found));
        }

        out.push_str(&json!({ "dir": dir, "decisions": decisions }).to_string());
        out.push('\n');
    }

    Ok(out)
}

fn width(paths: &[String], cwd: &str) -> Result<String, String> {
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .into_owned();

    let mut out = String::new();
    for path in paths {
        let found = formatter_width(path);
        let source = match &found.source {
            WidthSource::Config(file) => json!({ "kind": "config", "file": relative(&cwd, file) }),
            WidthSource::Default(formatter) => {
                json!({ "kind": "default", "formatter": formatter.name() })
            }
            WidthSource::Fallback => json!({ "kind": "fallback" }),
        };

        let unread: Vec<String> = found
            .unread
            .iter()
            .map(|file| relative(&cwd, file))
            .collect();
        out.push_str(&format!(
            "{{\"path\":{},\"columns\":{},\"tab\":{},\"source\":{source},\"unread\":{}}}\n",
            quoted(path),
            dragonbox_ecma::Buffer::new().format_finite(found.columns),
            dragonbox_ecma::Buffer::new().format_finite(found.tab),
            json!(unread),
        ));
    }

    Ok(out)
}

fn select(args: &[String], cwd: &str) -> Result<String, String> {
    let keep_generated = args.first().is_some_and(|arg| arg == "--keep-generated");
    let paths = &args[usize::from(keep_generated)..];
    let paths = paths.strip_prefix(&["--".to_owned()]).unwrap_or(paths);
    let selected = collect_files(paths, cwd, keep_generated, &mut Repositories::default())?;
    let printed = json!({
        "files": selected.files,
        "errors": selected.errors,
        "warnings": selected.warnings,
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

    let mut repositories = Repositories::default();
    let location = repositories.locate(cwd);
    let selected = collect_changed(location, &mut repositories, written.as_ref(), hunks)?;
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
        "warnings": selected.warnings,
        "submodules": selected.submodules,
        "changedLines": changed_lines,
    });

    Ok(format!("{printed}\n"))
}

fn stdin_targets(paths: &[String], cwd: &str) -> String {
    let mut out = String::new();
    for path in paths {
        let printed = match stdin_target(path, cwd, &mut Repositories::default()) {
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
    let mut repositories = Repositories::default();
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
        let chosen = pool::worker_count(files, workers.as_deref(), pool::DEFAULT_WORKERS, cores);
        out.push_str(&format!(
            "{}\n",
            json!({ "files": files, "workers": chosen })
        ));
    }

    Ok(out)
}

const USAGE: &str = "Usage: stanza-dev eval <files...>\n       stanza-dev batch\n       stanza-dev braces [--extension EXT] <dirs...>\n       stanza-dev width <paths...>\n       stanza-dev select [--keep-generated] <paths...>\n       stanza-dev changed [--hunks] [-- <written paths...>]\n       stanza-dev stdin-target <paths...>\n       stanza-dev locate <dirs...>\n       stanza-dev catalog\n       stanza-dev workers [--cores N] <file counts...>\n       stanza-dev nesting [--at-cap] [--stack BYTES]... [--ceiling DEPTH] [shapes...]\n       stanza-dev nesting --deepest <paths...>";

fn run(args: &[String], cwd: &str) -> u8 {
    let printed = match args.split_first() {
        Some((command, files)) if command == "eval" => Ok(format!("{}\n", eval(files, cwd))),
        Some((command, dirs)) if command == "braces" => braces(dirs, cwd),
        Some((command, paths)) if command == "width" => width(paths, cwd),
        Some((command, rest)) if command == "select" => select(rest, cwd),
        Some((command, rest)) if command == "changed" => changed(rest, cwd),
        Some((command, paths)) if command == "stdin-target" => Ok(stdin_targets(paths, cwd)),
        Some((command, dirs)) if command == "locate" => Ok(locate(dirs)),
        Some((command, [])) if command == "catalog" => Ok(catalog::catalog()),
        Some((command, rest)) if command == "workers" => workers(rest),
        Some((command, [])) if command == "batch" => batch::batch(cwd).map(|()| String::new()),
        Some((command, rest)) if command == "nesting" => nesting::nesting(rest, cwd),
        Some((command, rest)) if command == "nesting-probe" => nesting::probe(rest),
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

    let worker = thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || run(&args, &cwd));

    match worker {
        Ok(worker) => ExitCode::from(worker.join().expect("the work thread finishes")),
        Err(error) => {
            eprintln!("stanza-dev: cannot start the work thread: {error}");
            ExitCode::from(2)
        }
    }
}
