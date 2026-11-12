//! Embeds every SVG in `assets/vscode-icons` so new icons can be added by
//! dropping a file into that folder and mapping it in `src/ui/file_icons.rs`.

use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/vscode-icons");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("assets/vscode-icons exists")
        .filter_map(|e| {
            let path = e.ok()?.path();
            (path.extension()? == "svg").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();

    let mut out = String::from("&[\n");
    for name in &names {
        let path = dir.join(format!("{name}.svg"));
        out += &format!(
            "    ({name:?}, \"bytes://vscode-icons/{name}.svg\", include_bytes!({:?}) as &[u8]),\n",
            path.display().to_string()
        );
    }
    out += "]\n";
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("vscode_icons.rs");
    fs::write(dest, out).unwrap();
}
