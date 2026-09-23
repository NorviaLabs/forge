//! Language detection and tree-sitter parser registry.

use std::collections::HashMap;
use std::sync::OnceLock;
use tree_sitter::Parser;

use tree_sitter::Language as TsLanguage;

static LANGUAGE_MAP: OnceLock<HashMap<&'static str, SyntaxLanguage>> = OnceLock::new();

/// Supported programming languages for syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxLanguage {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Json,
    Html,
    Css,
    Bash,
    Toml,
    Yaml,
    Ini,
    C,
    Cpp,
    Java,
    CSharp,
    Ruby,
    Php,
    Swift,
    Kotlin,
    Sql,
    Make,
    Dockerfile,
    Scss,
    Less,
    Tsx,
    Unknown,
}

impl SyntaxLanguage {
    /// Get tree-sitter Language for this syntax language.
    pub fn tree_sitter(&self) -> TsLanguage {
        match self {
            SyntaxLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
            SyntaxLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            SyntaxLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            SyntaxLanguage::Python => tree_sitter_python::LANGUAGE.into(),
            SyntaxLanguage::Go => tree_sitter_go::LANGUAGE.into(),
            SyntaxLanguage::Json => tree_sitter_json::LANGUAGE.into(),
            SyntaxLanguage::Html => tree_sitter_html::LANGUAGE.into(),
            SyntaxLanguage::Css => tree_sitter_css::LANGUAGE.into(),
            SyntaxLanguage::Bash => tree_sitter_bash::LANGUAGE.into(),
            SyntaxLanguage::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            SyntaxLanguage::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            SyntaxLanguage::Ini => tree_sitter_ini::LANGUAGE.into(),
            SyntaxLanguage::C => tree_sitter_c::LANGUAGE.into(),
            SyntaxLanguage::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            SyntaxLanguage::Java => tree_sitter_java::LANGUAGE.into(),
            SyntaxLanguage::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            SyntaxLanguage::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            SyntaxLanguage::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            SyntaxLanguage::Swift => tree_sitter_swift::LANGUAGE.into(),
            SyntaxLanguage::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            SyntaxLanguage::Sql => tree_sitter_sequel::LANGUAGE.into(),
            SyntaxLanguage::Make => tree_sitter_make::LANGUAGE.into(),
            SyntaxLanguage::Dockerfile => tree_sitter_dockerfile_updated::language(),
            SyntaxLanguage::Scss => tree_sitter_scss::language(),
            SyntaxLanguage::Less => tree_sitter_less::language(),
            SyntaxLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            SyntaxLanguage::Unknown => tree_sitter_rust::LANGUAGE.into(),
        }
    }
}

impl SyntaxLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            SyntaxLanguage::Rust => "rust",
            SyntaxLanguage::TypeScript => "typescript",
            SyntaxLanguage::JavaScript => "javascript",
            SyntaxLanguage::Python => "python",
            SyntaxLanguage::Go => "go",
            SyntaxLanguage::Json => "json",
            SyntaxLanguage::Html => "html",
            SyntaxLanguage::Css => "css",
            SyntaxLanguage::Bash => "bash",
            SyntaxLanguage::Toml => "toml",
            SyntaxLanguage::Yaml => "yaml",
            SyntaxLanguage::Ini => "ini",
            SyntaxLanguage::C => "c",
            SyntaxLanguage::Cpp => "cpp",
            SyntaxLanguage::Java => "java",
            SyntaxLanguage::CSharp => "csharp",
            SyntaxLanguage::Ruby => "ruby",
            SyntaxLanguage::Php => "php",
            SyntaxLanguage::Swift => "swift",
            SyntaxLanguage::Kotlin => "kotlin",
            SyntaxLanguage::Sql => "sql",
            SyntaxLanguage::Make => "make",
            SyntaxLanguage::Dockerfile => "dockerfile",
            SyntaxLanguage::Scss => "scss",
            SyntaxLanguage::Less => "less",
            SyntaxLanguage::Tsx => "tsx",
            SyntaxLanguage::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for SyntaxLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SyntaxLanguage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let name = s.to_lowercase();
        if matches!(name.as_str(), "unknown" | "*") {
            return Ok(Self::Unknown);
        }
        language_map()
            .get(name.as_str())
            .copied()
            .ok_or_else(|| format!("unknown language: {s}"))
    }
}

fn build_language_map() -> HashMap<&'static str, SyntaxLanguage> {
    HashMap::from([
        ("c", SyntaxLanguage::C),
        ("cpp", SyntaxLanguage::Cpp),
        ("java", SyntaxLanguage::Java),
        ("csharp", SyntaxLanguage::CSharp),
        ("ruby", SyntaxLanguage::Ruby),
        ("php", SyntaxLanguage::Php),
        ("swift", SyntaxLanguage::Swift),
        ("kotlin", SyntaxLanguage::Kotlin),
        ("sql", SyntaxLanguage::Sql),
        ("make", SyntaxLanguage::Make),
        ("dockerfile", SyntaxLanguage::Dockerfile),
        ("h", SyntaxLanguage::C),
        ("cc", SyntaxLanguage::Cpp),
        ("cxx", SyntaxLanguage::Cpp),
        ("hpp", SyntaxLanguage::Cpp),
        ("hh", SyntaxLanguage::Cpp),
        ("hxx", SyntaxLanguage::Cpp),
        ("c++", SyntaxLanguage::Cpp),
        ("cs", SyntaxLanguage::CSharp),
        ("c#", SyntaxLanguage::CSharp),
        ("rb", SyntaxLanguage::Ruby),
        ("kt", SyntaxLanguage::Kotlin),
        ("kts", SyntaxLanguage::Kotlin),
        ("mk", SyntaxLanguage::Make),
        ("makefile", SyntaxLanguage::Make),
        ("rs", SyntaxLanguage::Rust),
        ("rust", SyntaxLanguage::Rust),
        ("ts", SyntaxLanguage::TypeScript),
        ("tsx", SyntaxLanguage::Tsx),
        ("typescript", SyntaxLanguage::TypeScript),
        ("js", SyntaxLanguage::JavaScript),
        ("jsx", SyntaxLanguage::JavaScript),
        ("mjs", SyntaxLanguage::JavaScript),
        ("cjs", SyntaxLanguage::JavaScript),
        ("javascript", SyntaxLanguage::JavaScript),
        ("py", SyntaxLanguage::Python),
        ("pyi", SyntaxLanguage::Python),
        ("python", SyntaxLanguage::Python),
        ("go", SyntaxLanguage::Go),
        ("golang", SyntaxLanguage::Go),
        ("json", SyntaxLanguage::Json),
        ("toml", SyntaxLanguage::Toml),
        ("yaml", SyntaxLanguage::Yaml),
        ("yml", SyntaxLanguage::Yaml),
        ("ini", SyntaxLanguage::Ini),
        ("html", SyntaxLanguage::Html),
        ("htm", SyntaxLanguage::Html),
        ("css", SyntaxLanguage::Css),
        ("scss", SyntaxLanguage::Scss),
        ("sass", SyntaxLanguage::Scss),
        ("less", SyntaxLanguage::Less),
        ("sh", SyntaxLanguage::Bash),
        ("bash", SyntaxLanguage::Bash),
        ("zsh", SyntaxLanguage::Bash),
        ("shell", SyntaxLanguage::Bash),
    ])
}

fn language_map() -> &'static HashMap<&'static str, SyntaxLanguage> {
    LANGUAGE_MAP.get_or_init(build_language_map)
}

/// JSON content detection above this many bytes skips the full parse (see
/// [`detect_language`]); large enough to hold any reasonable code block, small
/// enough that a per-render parse is free.
const MAX_JSON_DETECT_BYTES: usize = 256 * 1024;

/// Get a parser for a syntax language.
pub fn get_parser(lang: SyntaxLanguage) -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.tree_sitter())
        .expect("language should be valid");
    parser
}

/// Detect language from file extension or content heuristics.
pub fn detect_language(input: &str) -> Result<SyntaxLanguage, &'static str> {
    let trimmed = input.trim();

    // Check shebang
    if trimmed.starts_with("#!") {
        if trimmed.contains("python") {
            return Ok(SyntaxLanguage::Python);
        }
        if trimmed.contains("bash") || trimmed.contains("/sh") {
            return Ok(SyntaxLanguage::Bash);
        }
    }

    // Check for JSON
    if ((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        // The parse allocates the whole value tree and runs on every render
        // of a `{`/`[`-delimited block, even when its highlight is cached,
        // so gate it on size: below the ceiling it is exact, above it the
        // delimiters alone decide. ponytail: size cap, exact below.
        && trimmed.len() <= MAX_JSON_DETECT_BYTES
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Ok(SyntaxLanguage::Json);
    }

    // Check for HTML
    if trimmed.starts_with("<!DOCTYPE") || trimmed.starts_with("<html") || trimmed.contains("<html")
    {
        return Ok(SyntaxLanguage::Html);
    }

    // Check for Rust patterns
    if trimmed.contains("fn ") && trimmed.contains("->") && trimmed.contains("let ") {
        return Ok(SyntaxLanguage::Rust);
    }

    // Check for Go patterns
    if trimmed.contains("package ") && trimmed.contains("func ") {
        return Ok(SyntaxLanguage::Go);
    }

    // Check for Python patterns
    if trimmed.contains("def ") && trimmed.contains(":") && !trimmed.contains("{") {
        return Ok(SyntaxLanguage::Python);
    }

    // Check for TypeScript patterns
    if trimmed.contains(": string")
        || trimmed.contains(": number")
        || trimmed.contains("interface ")
    {
        return Ok(SyntaxLanguage::TypeScript);
    }

    Ok(SyntaxLanguage::Unknown)
}

/// Detect language from file path extension.
pub fn detect_from_path(path: &str) -> SyntaxLanguage {
    let path_lower = path.to_lowercase();

    let filename = path_lower
        .rsplit('/')
        .next()
        .unwrap_or(&path_lower)
        .rsplit('\\')
        .next()
        .unwrap_or(&path_lower);

    if matches!(filename, "makefile" | "gnumakefile") {
        return SyntaxLanguage::Make;
    }
    if filename == "dockerfile" || filename.starts_with("dockerfile.") {
        return SyntaxLanguage::Dockerfile;
    }
    if let Some(dot_pos) = filename.rfind('.') {
        let ext = &filename[dot_pos + 1..];
        if let Some(lang) = language_map().get(ext) {
            return *lang;
        }
    }

    SyntaxLanguage::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_from_path_rust() {
        assert_eq!(detect_from_path("src/main.rs"), SyntaxLanguage::Rust);
    }

    #[test]
    fn detect_from_path_python() {
        assert_eq!(detect_from_path("script.py"), SyntaxLanguage::Python);
    }

    #[test]
    fn detect_json() {
        let json = r#"{"key": "value", "num": 42}"#;
        assert_eq!(detect_language(json).unwrap(), SyntaxLanguage::Json);
    }

    #[test]
    fn oversized_brace_block_skips_the_json_parse() {
        // The size cap keeps a huge `{`-delimited block from triggering a
        // full parse on every render; it falls through to Unknown rather
        // than mis-detecting as a different language.
        let mut huge = String::from("{\"pad\": \"");
        huge.push_str(&"x".repeat(MAX_JSON_DETECT_BYTES));
        huge.push_str("\"}");
        assert_eq!(detect_language(&huge).unwrap(), SyntaxLanguage::Unknown);
    }

    #[test]
    fn syntax_language_parse_display_and_parser_cover_supported_languages() {
        for (alias, language) in language_map() {
            assert_eq!(alias.parse::<SyntaxLanguage>().unwrap(), *language);
            assert_eq!(
                language.as_str().parse::<SyntaxLanguage>().unwrap(),
                *language
            );
            let _parser = get_parser(*language);
            let _query = crate::queries::query(*language);
        }
        assert!("definitely-not-a-language"
            .parse::<SyntaxLanguage>()
            .is_err());
    }

    #[test]
    fn detect_language_uses_shebangs_and_common_patterns() {
        assert_eq!(
            detect_language("#!/usr/bin/env python\nprint('x')").unwrap(),
            SyntaxLanguage::Python
        );
        assert_eq!(
            detect_language("#!/bin/sh\necho x").unwrap(),
            SyntaxLanguage::Bash
        );
        // A shebang that names neither python nor a shell falls through the
        // shebang checks entirely (no early return) rather than defaulting
        // to Bash just because the line started with `#!`.
        assert_eq!(
            detect_language("#!/usr/bin/env node\nconsole.log('x')").unwrap(),
            SyntaxLanguage::Unknown
        );
        assert_eq!(
            detect_language("<!DOCTYPE html><html></html>").unwrap(),
            SyntaxLanguage::Html
        );
        assert_eq!(
            detect_language("fn main() -> i32 { let x = 1; x }").unwrap(),
            SyntaxLanguage::Rust
        );
        assert_eq!(
            detect_language("package main\nfunc main() {}").unwrap(),
            SyntaxLanguage::Go
        );
        assert_eq!(
            detect_language("def main():\n    return 1").unwrap(),
            SyntaxLanguage::Python
        );
        assert_eq!(
            detect_language("interface User { name: string }").unwrap(),
            SyntaxLanguage::TypeScript
        );
        assert_eq!(
            detect_language("plain text").unwrap(),
            SyntaxLanguage::Unknown
        );
    }

    #[test]
    fn detect_from_path_handles_case_windows_paths_and_unknowns() {
        assert_eq!(detect_from_path("C:\\Temp\\APP.TSX"), SyntaxLanguage::Tsx);
        assert_eq!(detect_from_path("/tmp/site.HTML"), SyntaxLanguage::Html);
        assert_eq!(detect_from_path("Dockerfile"), SyntaxLanguage::Dockerfile);
        assert_eq!(
            detect_from_path("Dockerfile.dev"),
            SyntaxLanguage::Dockerfile
        );
        assert_eq!(detect_from_path("build/Makefile"), SyntaxLanguage::Make);
        assert_eq!(detect_from_path("archive.tar.gz"), SyntaxLanguage::Unknown);
    }
}
