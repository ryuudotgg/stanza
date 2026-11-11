use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use oxc_allocator::Allocator;
use stanza::engine::process_file;
use stanza::engine::types::Mode;
use stanza::files::{Repositories, collect_files};
use stanza::languages::FALLBACK_LAYOUT;
use stanza::languages::javascript::nesting::{self, crossing, deepest};
use stanza::languages::javascript::parse::{parse, source_type};
use stanza::paths::{relative, resolve};

pub struct Shape {
    pub name: &'static str,
    pub path: &'static str,
    head: &'static str,
    open: &'static str,
    middle: &'static str,
    close: &'static str,
    tail: &'static str,
}

impl Shape {
    pub fn text(&self, depth: usize) -> String {
        let mut text = String::with_capacity(
            self.head.len() + depth * (self.open.len() + self.close.len()) + 64,
        );
        text.push_str(self.head);
        for _ in 0..depth {
            text.push_str(self.open);
        }

        text.push_str(self.middle);
        for _ in 0..depth {
            text.push_str(self.close);
        }

        text.push_str(self.tail);
        text
    }
}

const fn shape(
    name: &'static str,
    path: &'static str,
    [head, open, middle, close, tail]: [&'static str; 5],
) -> Shape {
    Shape {
        name,
        path,
        head,
        open,
        middle,
        close,
        tail,
    }
}

pub const SHAPES: &[Shape] = &[
    shape("arrays", "a.ts", ["const a = ", "[", "0", "]", ";\n"]),
    shape("parens", "a.ts", ["const a = ", "(", "0", ")", ";\n"]),
    shape("if-blocks", "a.ts", ["", "if (a) {\n", "b();\n", "}\n", ""]),
    shape("objects", "a.ts", ["const a = ", "{ a: ", "0", " }", ";\n"]),
    shape("templates", "a.ts", ["const a = ", "`${", "0", "}`", ";\n"]),
    shape("jsx", "a.tsx", ["const a = ", "<a>", "x", "</a>", ";\n"]),
    shape("bangs", "a.ts", ["const a = ", "!", "b", "", ";\n"]),
    shape("if-bare", "a.ts", ["", "if (a) ", "b();\n", "", ""]),
    shape("else-if", "a.ts", ["", "if (a) {} else ", "{}", "", "\n"]),
    shape(
        "ternary-tail",
        "a.ts",
        ["const a = ", "a ? b : ", "c", "", ";\n"],
    ),
    shape(
        "ternary-head",
        "a.ts",
        ["const a = ", "a ? ", "b", " : c", ";\n"],
    ),
    shape("arrows", "a.ts", ["const a = ", "a => ", "a", "", ";\n"]),
    shape("assigns", "a.ts", ["", "a = ", "b", "", ";\n"]),
    shape("powers", "a.ts", ["const a = ", "a ** ", "b", "", ";\n"]),
    shape("sums", "a.ts", ["const a = ", "a + ", "b", "", ";\n"]),
    shape("typeofs", "a.ts", ["const a = ", "typeof ", "b", "", ";\n"]),
    shape("news", "a.ts", ["const a = ", "new ", "b", "", ";\n"]),
    shape("minuses", "a.ts", ["const a = ", "- ", "b", "", ";\n"]),
    shape(
        "awaits",
        "a.ts",
        ["async function f() { ", "await ", "b", "", "; }\n"],
    ),
    shape("labels", "a.ts", ["", "a: ", "b;", "", "\n"]),
    shape("whiles", "a.ts", ["", "while (a) ", "b();", "", "\n"]),
    shape("fors", "a.ts", ["", "for (;;) ", "b();", "", "\n"]),
    shape("dos", "a.ts", ["", "do ", "b();", " while (a);", "\n"]),
    shape(
        "functions",
        "a.ts",
        ["", "function f() {\n", "b();\n", "}\n", ""],
    ),
    shape("calls", "a.ts", ["", "f(", "0", ")", ";\n"]),
    shape("generics", "a.ts", ["type A = ", "B<", "C", ">", ";\n"]),
    shape("casts", "a.ts", ["const a = ", "<T>", "b", "", ";\n"]),
    shape("keyofs", "a.ts", ["type A = ", "keyof ", "B", "", ";\n"]),
    shape("unions", "a.ts", ["type A = ", "B | ", "C", "", ";\n"]),
    shape(
        "conditional-types",
        "a.ts",
        ["type A = ", "B extends C ? D : ", "E", "", ";\n"],
    ),
    shape(
        "function-types",
        "a.ts",
        ["type A = ", "() => ", "B", "", ";\n"],
    ),
    shape(
        "class-extends",
        "a.ts",
        ["", "class A extends ", "B", " {}", "\n"],
    ),
    shape(
        "fragments",
        "a.tsx",
        ["const a = ", "<>", "x", "</>", ";\n"],
    ),
    shape(
        "jsx-attrs",
        "a.tsx",
        ["const a = ", "<a b={", "0", "} />", ";\n"],
    ),
    shape("spreads", "a.ts", ["const a = ", "[...", "b", "]", ";\n"]),
    shape("as-chains", "a.ts", ["const a = ", "", "b", " as T", ";\n"]),
    shape("members", "a.ts", ["const a = ", "", "b", ".c", ";\n"]),
    shape("call-chains", "a.ts", ["const a = ", "", "b", "()", ";\n"]),
    shape("non-nulls", "a.ts", ["const a = ", "", "b", "!", ";\n"]),
    shape("commas", "a.ts", ["const a = (", "b, ", "c", "", ");\n"]),
    shape(
        "yields",
        "a.ts",
        ["function* f() { ", "yield ", "b", "", "; }\n"],
    ),
    shape("voids", "a.ts", ["const a = ", "void ", "b", "", ";\n"]),
    shape("decorators", "a.ts", ["", "@a ", "class A {}", "", "\n"]),
    shape("tuple-types", "a.ts", ["type A = ", "[", "B", "]", ";\n"]),
    shape(
        "object-types",
        "a.ts",
        ["type A = ", "{ a: ", "B", " }", ";\n"],
    ),
    shape("array-types", "a.ts", ["type A = ", "", "B", "[]", ";\n"]),
    shape(
        "template-types",
        "a.ts",
        ["type A = ", "`${", "B", "}`", ";\n"],
    ),
    shape("namespaces", "a.ts", ["", "namespace a {\n", "", "}\n", ""]),
    shape("blocks", "a.ts", ["", "{\n", "", "}\n", ""]),
    shape("withs", "a.js", ["", "with (a) ", "b();", "", "\n"]),
    shape("ins", "a.ts", ["const a = ", "b in ", "c", "", ";\n"]),
    shape(
        "optional-calls",
        "a.ts",
        ["const a = ", "", "b", "?.()", ";\n"],
    ),
    shape("tagged", "a.ts", ["const a = ", "", "b", "``", ";\n"]),
    shape("nullish", "a.ts", ["const a = ", "a ?? ", "b", "", ";\n"]),
    shape("ands", "a.ts", ["const a = ", "a && ", "b", "", ";\n"]),
    shape(
        "satisfies",
        "a.ts",
        ["const a = ", "", "b", " satisfies T", ";\n"],
    ),
    shape(
        "type-args-calls",
        "a.ts",
        ["const a = ", "f<", "T", ">()", ";\n"],
    ),
    shape(
        "intersections",
        "a.ts",
        ["type A = ", "B & ", "C", "", ";\n"],
    ),
    shape(
        "readonlys",
        "a.ts",
        ["type A = ", "readonly ", "B[]", "", ";\n"],
    ),
    shape(
        "type-queries",
        "a.ts",
        ["type A = ", "typeof ", "b", "", ";\n"],
    ),
    shape("try", "a.ts", ["", "try {\n", "", "} finally {}\n", ""]),
    shape(
        "switches",
        "a.ts",
        ["", "switch (a) { case 1:\n", "", "}\n", ""],
    ),
    shape(
        "classes",
        "a.ts",
        ["", "class A { m() {\n", "", "} }\n", ""],
    ),
    shape("arrow-blocks", "a.ts", ["", "() => {\n", "", "};\n", ""]),
    shape("regexes", "a.ts", ["const a = ", "", "/a/", "", ";\n"]),
    shape(
        "object-spreads",
        "a.ts",
        ["const a = ", "{ ...", "b", " }", ";\n"],
    ),
    shape(
        "object-methods",
        "a.ts",
        ["const a = ", "{ m() { return ", "0", " } }", ";\n"],
    ),
    shape(
        "computed-keys",
        "a.ts",
        ["const a = ", "{ [k]: ", "0", " }", ";\n"],
    ),
    shape(
        "jsx-spreads",
        "a.tsx",
        ["const a = ", "<a {...", "b", "} />", ";\n"],
    ),
    shape(
        "jsx-children",
        "a.tsx",
        ["const a = ", "<a>{", "x", "}</a>", ";\n"],
    ),
    shape(
        "mapped-types",
        "a.ts",
        ["type A = ", "{ [K in T]: ", "B", " }", ";\n"],
    ),
    shape("index-types", "a.ts", ["type A = ", "B[", "C", "]", ";\n"]),
    shape(
        "new-calls",
        "a.ts",
        ["const a = ", "new A(", "0", ")", ";\n"],
    ),
    shape(
        "import-calls",
        "a.ts",
        ["const a = ", "import(", "b", ")", ";\n"],
    ),
    shape(
        "async-arrows",
        "a.ts",
        ["const a = ", "async () => ", "0", "", ";\n"],
    ),
    shape(
        "arrow-parens",
        "a.ts",
        ["const a = ", "() => (", "0", ")", ";\n"],
    ),
    shape(
        "static-blocks",
        "a.ts",
        ["", "class A { static {\n", "", "} }\n", ""],
    ),
    shape(
        "modules",
        "a.ts",
        ["", "declare module a {\n", "", "}\n", ""],
    ),
    shape(
        "enum-values",
        "a.ts",
        ["", "enum E { A = ", "0", " }", "\n"],
    ),
    shape("labels-blocks", "a.ts", ["", "a: {\n", "", "}\n", ""]),
    shape("catches", "a.ts", ["", "try {} catch {\n", "", "}\n", ""]),
    shape(
        "generic-arrows",
        "a.ts",
        ["const a = ", "<T,>() => ", "0", "", ";\n"],
    ),
    shape(
        "type-literals",
        "a.ts",
        ["type A = ", "{ a(): ", "B", " }", ";\n"],
    ),
    shape(
        "yield-stars",
        "a.ts",
        ["function* f() { ", "yield* ", "b", "", "; }\n"],
    ),
    shape(
        "object-arrays",
        "a.ts",
        ["const a = ", "[{ a: ", "0", " }]", ";\n"],
    ),
    shape(
        "sequence-parens",
        "a.ts",
        ["const a = ", "(0, ", "0", ")", ";\n"],
    ),
    shape(
        "default-params",
        "a.ts",
        ["", "function f(a = ", "0", ") {}", "\n"],
    ),
    shape(
        "destructures",
        "a.ts",
        ["const ", "{ a: ", "b", " }", " = c;\n"],
    ),
    shape(
        "array-patterns",
        "a.ts",
        ["const ", "[", "b", "]", " = c;\n"],
    ),
];

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Parse,
    Fix,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Parse => "parse",
            Stage::Fix => "fix",
        }
    }

    fn named(name: &str) -> Option<Stage> {
        match name {
            "parse" => Some(Stage::Parse),
            "fix" => Some(Stage::Fix),
            _ => None,
        }
    }
}

fn found(name: &str) -> Result<&'static Shape, String> {
    SHAPES
        .iter()
        .find(|shape| shape.name == name)
        .ok_or_else(|| format!("no shape named {name}"))
}

fn number(text: &str) -> Result<usize, String> {
    text.parse()
        .map_err(|_| format!("expected a whole number, not {text}"))
}

pub fn probe(args: &[String]) -> Result<String, String> {
    let [name, depth, stack, stage] = args else {
        return Err("nesting-probe takes <shape> <depth> <stack> <stage>".to_owned());
    };

    let shape = found(name)?;
    let depth = number(depth)?;
    let stack = number(stack)?;
    let stage = Stage::named(stage).ok_or_else(|| format!("no stage named {stage}"))?;
    let text = shape.text(depth);
    let worker = thread::Builder::new()
        .stack_size(stack)
        .spawn(move || match stage {
            Stage::Parse => parse(&Allocator::default(), shape.path, &text)
                .diagnostics
                .is_empty(),
            Stage::Fix => [false, true].into_iter().all(|keep_braces| {
                !process_file(
                    shape.path,
                    &text,
                    Mode::Fix,
                    keep_braces,
                    FALLBACK_LAYOUT,
                    None,
                )
                .parse_error
            }),
        });

    let clean = worker
        .map_err(|error| format!("cannot start a {stack} byte thread: {error}"))?
        .join()
        .map_err(|_| "the probe panicked".to_owned())?;

    Ok(format!("{}\n", if clean { "clean" } else { "rejected" }))
}

#[derive(Clone, Copy, PartialEq)]
enum Probed {
    Clean,
    Rejected,
    Crashed,
}

fn probed(shape: &Shape, depth: usize, stack: usize, stage: Stage) -> Result<Probed, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let output = Command::new(exe)
        .args([
            "nesting-probe",
            shape.name,
            &depth.to_string(),
            &stack.to_string(),
            stage.name(),
        ])
        .stderr(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;

    Ok(match (output.status.success(), output.stdout.as_slice()) {
        (true, b"clean\n") => Probed::Clean,
        (true, _) => Probed::Rejected,
        (false, _) => Probed::Crashed,
    })
}

fn last_true(
    ceiling: usize,
    mut holds: impl FnMut(usize) -> Result<bool, String>,
) -> Result<usize, String> {
    if holds(ceiling)? {
        return Ok(ceiling);
    }

    let (mut low, mut high) = (0, ceiling);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if holds(middle)? {
            low = middle;
        } else {
            high = middle;
        }
    }

    Ok(low)
}

fn levels(shape: &Shape, depth: usize) -> usize {
    deepest(&shape.text(depth), source_type(shape.path))
}

fn measured(shapes: &[&Shape], stacks: &[usize], ceiling: usize) -> Result<String, String> {
    let mut out = String::new();
    for shape in shapes {
        for &stack in stacks {
            let depth = last_true(ceiling, |depth| {
                Ok(probed(shape, depth, stack, Stage::Parse)? != Probed::Crashed)
            })?;
            let counted = levels(shape, depth);
            let per_level = if depth == ceiling {
                "flat".to_owned()
            } else {
                (stack / counted.max(1)).to_string()
            };

            out.push_str(&format!(
                "{}\t{stack}\t{depth}\t{counted}\t{per_level}\n",
                shape.name
            ));
        }
    }

    Ok(out)
}

fn at_cap(shapes: &[&Shape], stacks: &[usize], ceiling: usize) -> Result<String, String> {
    let mut out = String::new();
    for &stack in stacks {
        let cap = nesting::cap(stack);
        for shape in shapes {
            let source = source_type(shape.path);
            let depth = last_true(ceiling, |depth| {
                Ok(crossing(&shape.text(depth), source, cap).is_none())
            })?;
            let verdict = match probed(shape, depth, stack, Stage::Fix)? {
                Probed::Clean => "formats",
                Probed::Rejected => "rejected",
                Probed::Crashed => "crashes",
            };

            out.push_str(&format!(
                "{}\t{stack}\t{cap}\t{depth}\t{}\t{verdict}\n",
                shape.name,
                levels(shape, depth)
            ));
        }
    }

    Ok(out)
}

fn deepest_files(paths: &[String], cwd: &str) -> Result<String, String> {
    let (files, directories): (Vec<String>, Vec<String>) = paths
        .iter()
        .map(|path| resolve(cwd, path))
        .partition(|path| Path::new(path).is_file());
    let selected = collect_files(&directories, cwd, false, &mut Repositories::default())?;
    let mut depths: Vec<(usize, String)> = files
        .into_iter()
        .chain(selected.files)
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((deepest(&text, source_type(&path)), path))
        })
        .collect();

    depths.sort_by(|left, right| right.cmp(left));
    let mut out = format!("files\t{}\n", depths.len());
    for (depth, path) in depths.iter().take(10) {
        out.push_str(&format!("{depth}\t{}\n", relative(cwd, path)));
    }

    Ok(out)
}

pub fn nesting(args: &[String], cwd: &str) -> Result<String, String> {
    let mut stacks = Vec::new();
    let mut ceiling = 2_000_000;
    let mut capped = false;
    let mut shapes = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().ok_or_else(|| format!("{arg} takes a value"));
        match arg.as_str() {
            "--deepest" => return deepest_files(rest.as_slice(), cwd),
            "--stack" => stacks.push(number(value()?)?),
            "--ceiling" => ceiling = number(value()?)?,
            "--at-cap" => capped = true,
            name => shapes.push(found(name)?),
        }
    }

    if stacks.is_empty() {
        stacks = vec![64 << 20, 8 << 20];
    }

    if shapes.is_empty() {
        shapes = SHAPES.iter().collect();
    }

    if capped {
        at_cap(&shapes, &stacks, ceiling)
    } else {
        measured(&shapes, &stacks, ceiling)
    }
}
