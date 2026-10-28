//! The embedded built-in definitions parse, validate, and match the
//! constructor-built registry field by field (stage 7 transition check).

use forge_connect::builtin::{builtin_specs, BUILTIN_PROVIDERS};

#[test]
fn embedded_files_are_sorted_and_parse() {
    let names: Vec<&str> = BUILTIN_PROVIDERS.iter().map(|(name, _)| *name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert_eq!(builtin_specs().len(), 7);
}

#[test]
fn embedded_definitions_match_the_constructors() {
    let registry = forge_connect::builtin_registry();
    let specs = builtin_specs();
    for expected in registry.profiles() {
        let actual = specs
            .iter()
            .find(|spec| spec.id == expected.id)
            .unwrap_or_else(|| panic!("no embedded definition for `{}`", expected.id));
        assert_eq!(actual, expected, "{}", expected.id);
    }
}
