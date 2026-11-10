include!("src/commands_list.rs");

fn main() {
    // `generate_context!` reads these at compile time; tauri-build only
    // tracks tauri.conf.json, so rebuild when the others change too.
    println!("cargo:rerun-if-changed=tauri.self-test.patch.json");
    println!("cargo:rerun-if-changed=isolation");
    let self_test = std::env::var_os("CARGO_FEATURE_SELF_TEST").is_some();
    build_ui(self_test);
    if self_test {
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

/// Build the web UI that `generate_context!` embeds, so a plain `cargo build`
/// works from a fresh checkout. It reruns only when the UI sources change.
///
/// `FORGE_SKIP_UI_BUILD=1` uses an already built `ui/dist` (or
/// `ui/dist-self-test`) instead, for pipelines that build the UI separately.
fn build_ui(self_test: bool) {
    let out_dir = if self_test {
        "ui/dist-self-test"
    } else {
        "ui/dist"
    };
    println!("cargo:rerun-if-env-changed=FORGE_SKIP_UI_BUILD");
    if std::env::var_os("FORGE_SKIP_UI_BUILD").is_some_and(|value| !value.is_empty()) {
        // Watching the output is safe only when this script never writes it.
        println!("cargo:rerun-if-changed={out_dir}");
        return;
    }
    for input in [
        "ui/src",
        "ui/index.html",
        "ui/package.json",
        "ui/pnpm-lock.yaml",
        "ui/vite.config.js",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    // Absolute: a relative program path is not resolved against `current_dir`.
    let ui =
        std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"))
            .join("ui");
    if !ui.join("node_modules").is_dir() {
        run(
            std::process::Command::new("pnpm")
                .args(["install", "--frozen-lockfile"])
                .current_dir(&ui),
            "pnpm install --frozen-lockfile",
        );
    }
    let mut vite = std::process::Command::new(ui.join("node_modules/.bin/vite"));
    vite.arg("build").current_dir(&ui);
    if self_test {
        vite.args(["--mode", "self-test"]);
    }
    run(&mut vite, "vite build");
}

fn run(command: &mut std::process::Command, what: &str) {
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("{what} in crates/forge-desktop/ui failed ({status})"),
        Err(error) => panic!(
            "cannot run {what} in crates/forge-desktop/ui: {error}. \
             The desktop UI needs Node.js and pnpm; or build it yourself and set \
             FORGE_SKIP_UI_BUILD=1"
        ),
    }
}
