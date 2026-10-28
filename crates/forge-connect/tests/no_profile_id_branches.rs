//! Guard (design §2, G10): no production code branches on a built-in
//! profile id. Built-in behavior lives in `providers/*.toml`; code reads the
//! profile's fields. This test parses every crate's `src/` and fails on a
//! string literal equal to a built-in id, or starting with `<id>/`.
//!
//! Skipped: `builtin.rs` (the id table) and `normalize.rs` (the legacy
//! prefix table), items under `#[cfg(test)]`, test files, and macro bodies
//! (`format!`, `assert!`, … are token streams, not parsed).

use std::path::{Path, PathBuf};

use syn::visit::Visit;

struct Finder<'a> {
    ids: &'a [String],
    file: &'a Path,
    hits: Vec<String>,
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| meta.path().is_ident("test"))
    })
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = match item {
            syn::Item::Mod(item) => &item.attrs,
            syn::Item::Fn(item) => &item.attrs,
            syn::Item::Impl(item) => &item.attrs,
            syn::Item::Const(item) => &item.attrs,
            syn::Item::Static(item) => &item.attrs,
            syn::Item::Struct(item) => &item.attrs,
            syn::Item::Use(item) => &item.attrs,
            _ => return syn::visit::visit_item(self, item),
        };
        if !is_cfg_test(attrs) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
        let value = lit.value();
        if self
            .ids
            .iter()
            .any(|id| value == *id || value.starts_with(&format!("{id}/")))
        {
            self.hits
                .push(format!("{}: \"{value}\"", self.file.display()));
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "tests" && name != "providers" {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs")
            && !name.ends_with("tests.rs")
            && !matches!(name, "builtin.rs" | "normalize.rs")
        {
            out.push(path);
        }
    }
}

#[test]
fn no_production_code_names_a_builtin_profile_id() {
    let ids: Vec<String> = forge_connect::builtin::builtin_specs()
        .into_iter()
        .map(|spec| spec.id)
        .collect();
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        rust_files(&entry.path().join("src"), &mut files);
    }
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        crates.display()
    );
    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let Ok(parsed) = syn::parse_file(&text) else {
            continue;
        };
        let mut finder = Finder {
            ids: &ids,
            file,
            hits: Vec::new(),
        };
        finder.visit_file(&parsed);
        hits.extend(finder.hits);
    }
    assert!(
        hits.is_empty(),
        "production code names a built-in profile id; read the profile's \
         fields instead:\n{}",
        hits.join("\n")
    );
}
