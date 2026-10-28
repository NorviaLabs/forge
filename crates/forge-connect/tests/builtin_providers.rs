//! The embedded built-in definitions parse and are listed in order.

use forge_connect::builtin::{builtin_specs, BUILTIN_PROVIDERS};

#[test]
fn embedded_files_are_sorted_and_parse() {
    let names: Vec<&str> = BUILTIN_PROVIDERS.iter().map(|(name, _)| *name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert_eq!(builtin_specs().len(), 7);
}
