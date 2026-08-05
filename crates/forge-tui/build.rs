use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=themes");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let themes_dir = Path::new(&manifest_dir).join("themes");

    let mut files: Vec<String> = Vec::new();

    if let Ok(read_dir) = fs::read_dir(&themes_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                files.push(name.to_string());
            }
        }
    }

    files.sort();

    let mut code = String::from("const BUILTIN_THEMES: &[(&str, &str)] = &[\n");
    for name in files {
        // Emit include_str! so the theme stays embedded at compile time
        code.push_str(&format!(
            "    (\"{name}\", include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/themes/{name}\"))),\n"
        ));
    }
    code.push_str("];\n");

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("builtin_themes.rs"), code).unwrap();
}
