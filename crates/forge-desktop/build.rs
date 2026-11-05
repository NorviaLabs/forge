include!("src/commands_list.rs");

fn main() {
    // `generate_context!` reads these at compile time; tauri-build only
    // tracks tauri.conf.json, so rebuild when the others change too.
    println!("cargo:rerun-if-changed=tauri.self-test.patch.json");
    println!("cargo:rerun-if-changed=ui/dist");
    println!("cargo:rerun-if-changed=ui/dist-self-test");
    println!("cargo:rerun-if-changed=isolation");
    if std::env::var_os("CARGO_FEATURE_SELF_TEST").is_some() {
        // Self-test builds differ from release only by this merge patch
        // (identity and UI bundle). Tauri reads the patch from TAURI_CONFIG:
        // tauri-build here (ACL, capabilities) and `generate_context!` in
        // the crate (embedded config and assets). Passing another config
        // *file* to `generate_context!` does not work: it only picks the
        // directory and still reads tauri.conf.json.
        let patch = std::fs::read_to_string("tauri.self-test.patch.json")
            .expect("tauri.self-test.patch.json");
        let patch: serde_json::Value = serde_json::from_str(&patch).expect("patch is JSON");
        let patch = patch.to_string();
        std::env::set_var("TAURI_CONFIG", &patch);
        println!("cargo:rustc-env=TAURI_CONFIG={patch}");
    }
    // Self-test builds expose exactly the same commands as release builds.
    let commands = DESKTOP_COMMANDS;
    // Declaring the app manifest makes every command opt-in: only the
    // commands granted in `capabilities/` are callable, and only from the
    // windows those capabilities name.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )
    .expect("tauri build configuration");
}
