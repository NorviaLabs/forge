//! Language detection and tree-sitter parser registry.

use std::collections::HashMap;
use std::sync::OnceLock;
use tree_sitter::Parser;

use tree_sitter::Language as TsLanguage;

static LANGUAGE_MAP: OnceLock<HashMap<&'static str, SyntaxLanguage>> = OnceLock::new();

/// Supported programming languages for syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            SyntaxLanguage::Unknown => tree_sitter_rust::LANGUAGE.into(),
        }
    }
}

impl std::fmt::Display for SyntaxLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SyntaxLanguage::Rust => "rust",
            SyntaxLanguage::TypeScript => "typescript",
            SyntaxLanguage::JavaScript => "javascript",
            SyntaxLanguage::Python => "python",
            SyntaxLanguage::Go => "go",
            SyntaxLanguage::Json => "json",
            SyntaxLanguage::Html => "html",
            SyntaxLanguage::Css => "css",
            SyntaxLanguage::Bash => "bash",
            SyntaxLanguage::Unknown => "unknown",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for SyntaxLanguage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rust" | "rs" => Ok(SyntaxLanguage::Rust),
            "typescript" | "ts" | "tsx" => Ok(SyntaxLanguage::TypeScript),
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Ok(SyntaxLanguage::JavaScript),
            "python" | "py" | "pyi" => Ok(SyntaxLanguage::Python),
            "go" | "golang" => Ok(SyntaxLanguage::Go),
            "json" => Ok(SyntaxLanguage::Json),
            "html" | "htm" => Ok(SyntaxLanguage::Html),
            "css" | "scss" | "sass" | "less" => Ok(SyntaxLanguage::Css),
            "bash" | "sh" | "zsh" | "shell" => Ok(SyntaxLanguage::Bash),
            "unknown" | "*" => Ok(SyntaxLanguage::Unknown),
            other => Err(format!("unknown language: {other}")),
        }
    }
}

fn build_language_map() -> HashMap<&'static str, SyntaxLanguage> {
    HashMap::from([
        ("rs", SyntaxLanguage::Rust),
        ("rust", SyntaxLanguage::Rust),
        ("ts", SyntaxLanguage::TypeScript),
        ("tsx", SyntaxLanguage::TypeScript),
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
        ("html", SyntaxLanguage::Html),
        ("htm", SyntaxLanguage::Html),
        ("css", SyntaxLanguage::Css),
        ("scss", SyntaxLanguage::Css),
        ("sass", SyntaxLanguage::Css),
        ("less", SyntaxLanguage::Css),
        ("sh", SyntaxLanguage::Bash),
        ("bash", SyntaxLanguage::Bash),
        ("zsh", SyntaxLanguage::Bash),
        ("shell", SyntaxLanguage::Bash),
    ])
}

fn language_map() -> &'static HashMap<&'static str, SyntaxLanguage> {
    LANGUAGE_MAP.get_or_init(build_language_map)
}

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
    fn syntax_language_parse_display_and_parser_cover_supported_languages() {
        for (alias, language, display) in [
            ("rs", SyntaxLanguage::Rust, "rust"),
            ("ts", SyntaxLanguage::TypeScript, "typescript"),
            ("tsx", SyntaxLanguage::TypeScript, "typescript"),
            ("js", SyntaxLanguage::JavaScript, "javascript"),
            ("jsx", SyntaxLanguage::JavaScript, "javascript"),
            ("mjs", SyntaxLanguage::JavaScript, "javascript"),
            ("cjs", SyntaxLanguage::JavaScript, "javascript"),
            ("py", SyntaxLanguage::Python, "python"),
            ("pyi", SyntaxLanguage::Python, "python"),
            ("go", SyntaxLanguage::Go, "go"),
            ("golang", SyntaxLanguage::Go, "go"),
            ("html", SyntaxLanguage::Html, "html"),
            ("htm", SyntaxLanguage::Html, "html"),
            ("css", SyntaxLanguage::Css, "css"),
            ("scss", SyntaxLanguage::Css, "css"),
            ("sass", SyntaxLanguage::Css, "css"),
            ("less", SyntaxLanguage::Css, "css"),
            ("sh", SyntaxLanguage::Bash, "bash"),
            ("zsh", SyntaxLanguage::Bash, "bash"),
            ("shell", SyntaxLanguage::Bash, "bash"),
            ("*", SyntaxLanguage::Unknown, "unknown"),
        ] {
            let parsed: SyntaxLanguage = alias.parse().unwrap();
            assert_eq!(parsed, language);
            assert_eq!(language.to_string(), display);
            let _parser = get_parser(language);
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
        assert_eq!(
            detect_from_path("C:\\Temp\\APP.TSX"),
            SyntaxLanguage::TypeScript
        );
        assert_eq!(detect_from_path("/tmp/site.HTML"), SyntaxLanguage::Html);
        assert_eq!(detect_from_path("Dockerfile"), SyntaxLanguage::Unknown);
        assert_eq!(detect_from_path("archive.tar.gz"), SyntaxLanguage::Unknown);
    }
}
