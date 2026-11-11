//! The desktop app's security configuration, checked as data. These are the
//! K1 gates from architecture §4b: an explicit command allowlist, a single
//! capability scoped to the main window, no `core:default`, no plugins, the
//! pinned CSP, `withGlobalTauri` off, `freezePrototype` on, and a separate
//! identity for self-test builds.

use std::collections::BTreeSet;
use std::path::Path;

include!("../src/commands_list.rs");

include!("../src/pinned.rs");

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn json(path: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(crate_dir().join(path)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn kebab(command: &str) -> String {
    command.replace('_', "-")
}

fn expected_permissions() -> BTreeSet<String> {
    let mut permissions: BTreeSet<String> = DESKTOP_COMMANDS
        .iter()
        .map(|command| format!("allow-{}", kebab(command)))
        .collect();
    permissions.insert("core:event:allow-listen".into());
    permissions.insert("core:event:allow-unlisten".into());
    permissions
}

fn check_security(config: &serde_json::Value, name: &str) {
    let app = &config["app"];
    assert_eq!(app["withGlobalTauri"], false, "{name}: withGlobalTauri");
    let security = &app["security"];
    assert_eq!(
        security["csp"], PINNED_CSP,
        "{name}: CSP must equal the pinned string"
    );
    assert_eq!(security["freezePrototype"], true, "{name}: freezePrototype");
    assert_eq!(
        security["capabilities"],
        serde_json::json!(["main"]),
        "{name}: capabilities must be listed explicitly, and only `main`"
    );
    assert!(
        security
            .get("dangerousDisableAssetCspModification")
            .is_none(),
        "{name}"
    );
    assert_eq!(
        security["pattern"],
        serde_json::json!({"use": "isolation", "options": {"dir": "isolation"}}),
        "{name}: the isolation pattern must be on"
    );
    let windows = app["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1, "{name}: one window");
    assert_eq!(windows[0]["label"], "main", "{name}");
    assert!(
        app.get("plugins").is_none() && config.get("plugins").is_none(),
        "{name}: no plugins"
    );
}

#[test]
fn csp_is_strict_where_it_matters() {
    // style-src allows inline styles (CodeMirror, xterm.js and Svelte need
    // them); scripts, frames, objects and remote origins stay closed.
    let directives: Vec<&str> = PINNED_CSP.split("; ").collect();
    assert!(directives.contains(&"script-src 'self'"));
    assert!(
        !directives
            .iter()
            .any(|d| d.starts_with("frame-src") || d.starts_with("child-src")),
        "an explicit frame-src/child-src would block the isolation frame"
    );
    assert!(directives.contains(&"default-src 'none'"));
    assert!(!PINNED_CSP.contains("unsafe-eval"));
    assert!(!PINNED_CSP.contains("http://127.0.0.1") && !PINNED_CSP.contains("localhost:"));
    assert!(!PINNED_CSP.contains("https:") && !PINNED_CSP.contains("*"));
}

#[test]
fn release_configuration_is_locked_down() {
    let config = json("tauri.conf.json");
    assert_eq!(config["identifier"], "dev.forge.desktop");
    check_security(&config, "release");
}

#[test]
fn self_test_builds_have_their_own_identity_and_the_same_security() {
    // Self-test builds are the release config plus this merge patch (see
    // build.rs). It may change identity and the UI bundle, nothing else; the
    // self-test re-checks the embedded result at runtime.
    let patch = json("tauri.self-test.patch.json");
    let keys: BTreeSet<&str> = patch
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["identifier", "productName", "build"]));
    assert_eq!(
        patch["build"],
        serde_json::json!({"frontendDist": "ui/dist-self-test"})
    );
    assert_eq!(patch["identifier"], "dev.forge.desktop.test");
    assert_ne!(patch["identifier"], json("tauri.conf.json")["identifier"]);
}

#[test]
fn the_only_capability_grants_exactly_forge_commands_and_listen() {
    let entries: Vec<String> = std::fs::read_dir(crate_dir().join("capabilities"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["main.json".to_string()],
        "exactly one capability file"
    );

    let capability = json("capabilities/main.json");
    assert_eq!(capability["identifier"], "main");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    let granted: BTreeSet<String> = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|permission| permission.as_str().unwrap().to_string())
        .collect();
    assert_eq!(granted, expected_permissions());
    assert!(!granted.contains("core:default"));
}

/// The ACL tauri-build generated from the configuration (a build artifact,
/// not the source): it must contain the one capability and nothing else.
#[test]
fn generated_acl_matches_the_capability() {
    let generated = json("gen/schemas/capabilities.json");
    let capabilities = generated.as_object().unwrap();
    assert_eq!(capabilities.keys().collect::<Vec<_>>(), vec!["main"]);
    let granted: BTreeSet<String> = capabilities["main"]["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|permission| permission.as_str().unwrap().to_string())
        .collect();
    assert_eq!(granted, expected_permissions());
}

#[test]
fn every_registered_handler_is_on_the_allowlist_and_vice_versa() {
    let startup = std::fs::read_to_string(crate_dir().join("src/startup.rs")).unwrap();
    let start = startup.find("tauri::generate_handler![").unwrap();
    let end = start + startup[start..].find(']').unwrap();
    let registered: BTreeSet<String> = startup[start + "tauri::generate_handler![".len()..end]
        .split(',')
        .map(|item| item.trim().trim_start_matches("bridge::").to_string())
        .filter(|item| !item.is_empty())
        .collect();
    let allowed: BTreeSet<String> = DESKTOP_COMMANDS.iter().map(|c| c.to_string()).collect();
    assert_eq!(registered, allowed);
    assert_eq!(
        startup.matches("tauri::generate_handler!").count(),
        1,
        "one handler list for every build"
    );
}

#[test]
fn no_tauri_plugins_are_linked() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("tauri-plugin-"),
        "plugins expose IPC surface"
    );
}

/// The isolation hook passes exactly Forge's commands, event listen/unlisten,
/// and Tauri's built-in channel transport, and is inline
/// (Tauri serves only `index.html` from the isolation directory).
#[test]
fn isolation_hook_allows_exactly_the_granted_commands() {
    let html = std::fs::read_to_string(crate_dir().join("isolation/index.html")).unwrap();
    assert!(
        !html.contains("<script src"),
        "the hook must be an inline script"
    );
    let start = html.find("new Set([").unwrap() + "new Set([".len();
    let end = start + html[start..].find("])").unwrap();
    let allowed: BTreeSet<String> = html[start..end]
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect();
    let mut expected: BTreeSet<String> = DESKTOP_COMMANDS.iter().map(|c| c.to_string()).collect();
    expected.insert("plugin:event|listen".into());
    expected.insert("plugin:event|unlisten".into());
    expected.insert("plugin:__TAURI_CHANNEL__|fetch".into());
    assert_eq!(allowed, expected);
    assert!(html.contains("ALLOWED.has(payload.cmd) ? payload : null"));
    let entries: Vec<String> = std::fs::read_dir(crate_dir().join("isolation"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["index.html".to_string()]);
}

/// Model output reaches the DOM through exactly one `{@html}`, the one in
/// Markdown.svelte, behind markdown-it (`html: false`) and DOMPurify.
#[test]
fn exactly_one_html_insertion_site() {
    let mut sites = Vec::new();
    for entry in std::fs::read_dir(crate_dir().join("ui/src")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for _ in text.matches("{@html") {
            sites.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
        assert!(
            !text.contains("innerHTML") || path.ends_with("selftest.js"),
            "{path:?} writes innerHTML"
        );
    }
    assert_eq!(sites, vec!["Markdown.svelte".to_string()]);
    let markdown = std::fs::read_to_string(crate_dir().join("ui/src/markdown.js")).unwrap();
    assert!(markdown.contains("html: false"));
    assert!(markdown.contains("DOMPurify.sanitize("));
}

/// The release UI bundle carries none of the self-test probes (they are in
/// a chunk only the `self-test` mode build imports).
#[test]
fn release_ui_bundle_has_no_self_test_code() {
    let mut release = String::new();
    for entry in std::fs::read_dir(crate_dir().join("ui/dist/assets")).unwrap() {
        release.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default());
    }
    assert!(!release.contains("not_a_forge_command"));
    assert!(!release.contains("selftest"));
}

/// Commands that act on a project, the providers or the app, not on one
/// session. Everything else must name its session.
const SESSIONLESS_COMMANDS: &[&str] = &[
    "trust_status",
    "trust_workspace",
    "choose_folder",
    "list_projects",
    "open_project",
    "subscribe",
    "project_switch",
    "project_add",
    "project_trust",
    "project_close",
    "project_remove",
    "project_relocate",
    // These two start a session in the project on screen.
    "create_session",
    "attach_worktree",
    "list_models",
    "list_providers",
    "connect_provider",
    "disconnect_provider",
    "import_provider_login",
    "save_custom_provider",
    "update_custom_provider",
    "remove_custom_provider",
    "add_model",
    "set_unsaved",
    "quit_app",
];

/// A session command that reads "the selected session" from the backend can
/// land on a session the operator is not looking at. Every bridge command
/// therefore takes `session_id` from the page, unless it is listed above.
#[test]
fn every_session_command_names_its_session() {
    let source = std::fs::read_to_string(crate_dir().join("src/bridge.rs")).unwrap();
    let mut sessionless = BTreeSet::new();
    let mut commands = 0;
    for (start, _) in source.match_indices("#[tauri::command]") {
        let rest = &source[start..];
        let name_start = rest.find("fn ").expect("a command is a function") + 3;
        let name_end = name_start + rest[name_start..].find('(').unwrap();
        let body_start = rest.find("{\n").unwrap();
        commands += 1;
        if !rest[name_end..body_start].contains("session_id:") {
            sessionless.insert(rest[name_start..name_end].to_string());
        }
    }
    assert!(commands > 60, "the scan found only {commands} commands");
    let expected: BTreeSet<String> = SESSIONLESS_COMMANDS
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(
        sessionless, expected,
        "a command without `session_id` must be project-, provider- or app-level and listed in \
         SESSIONLESS_COMMANDS; a session command takes its session from the page"
    );
}
