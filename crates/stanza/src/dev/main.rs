use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread;

use stanza::languages::javascript::config::evaluate::{Value, origin};
use stanza::languages::javascript::config::module::Session;
use stanza::paths::{relative, resolve};

const STACK_BYTES: usize = 1 << 30;

struct Dump<'a> {
    session: &'a Session,
    cwd: &'a str,
    out: String,
    path: Vec<String>,
    seen: HashMap<*const (), (Value, String)>,
    origins: Vec<String>,
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
                        self.origins.push(format!(
                            "{}: {}",
                            quoted(&self.path.join(".")),
                            quoted(&format!("{file}:{}", location.start))
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

fn run(args: &[String], cwd: &str) -> u8 {
    match args.split_first() {
        Some((command, files)) if command == "eval" => {
            let mut stdout = std::io::stdout();
            match writeln!(stdout, "{}", eval(files, cwd)) {
                Ok(()) => 0,
                Err(_) => 2,
            }
        }
        _ => {
            eprintln!("Usage: stanza-dev eval <files...>");
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
