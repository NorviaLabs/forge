include!("src/commands_list.rs");

fn main() {
    // `generate_context!` reads these at compile time; tauri-build only
    // tracks tauri.conf.json, so rebuild when the others change too.
    println!("cargo:rerun-if-changed=tauri.self-test.conf.json");
    println!("cargo:rerun-if-changed=ui/dist");
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
