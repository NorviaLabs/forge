use std::env;
use std::fs;
use std::path::Path;

fn main() {
    assert_ratatui_layout_cache_enabled();
    println!("cargo:rerun-if-changed=themes");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");

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

fn assert_ratatui_layout_cache_enabled() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = Path::new(&manifest_dir);
    let workspace_manifest = fs::read_to_string(manifest_dir.join("../../Cargo.toml")).unwrap();
    let crate_manifest = fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();

    assert!(
        workspace_manifest.contains("ratatui = { version = \"=0.30.2\"")
            && workspace_manifest.contains("\"crossterm\"")
            && workspace_manifest.contains("\"layout-cache\"")
            && workspace_manifest.contains("\"underline-color\"")
            && workspace_manifest.contains("\"unstable-rendered-line-info\""),
        "workspace ratatui dependency must keep layout-cache enabled for TUI render performance"
    );
    assert!(
        crate_manifest.contains("ratatui = { workspace = true }"),
        "forge-tui must use the workspace ratatui dependency so layout-cache stays enabled"
    );
}
