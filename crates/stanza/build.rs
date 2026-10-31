use std::{env, fs, path::Path};

fn main() {
    let manifest = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../package.json");
    println!("cargo::rerun-if-changed={}", manifest.display());

    let text = fs::read_to_string(&manifest).expect("package.json is readable");
    let package: serde_json::Value = serde_json::from_str(&text).expect("package.json is JSON");
    let version = package["version"]
        .as_str()
        .expect("package.json has a string version");
    println!("cargo::rustc-env=STANZA_VERSION={version}");
}
