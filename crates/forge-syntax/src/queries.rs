use crate::lang::SyntaxLanguage;
use std::collections::HashMap;
use std::sync::OnceLock;
use tree_sitter::Query;

pub(crate) fn query(lang: SyntaxLanguage) -> &'static Query {
    static QUERIES: OnceLock<HashMap<SyntaxLanguage, OnceLock<Query>>> = OnceLock::new();
    QUERIES.get_or_init(|| {
        use SyntaxLanguage::*;
        [
            Rust, TypeScript, Tsx, JavaScript, Python, Go, Json, Html, Css, Bash, Toml, Yaml, Ini,
            C, Cpp, Java, CSharp, Ruby, Php, Swift, Kotlin, Sql, Make, Dockerfile, Scss, Less,
        ]
        .into_iter()
        .map(|lang| (lang, OnceLock::new()))
        .collect()
    })[&lang]
        .get_or_init(|| {
            use SyntaxLanguage::*;
            let source = match lang {
                Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_owned(),
                TypeScript => format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
                Tsx => format!(
                    "{}\n{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
                JavaScript => format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
                Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
                Go => tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
                Json => tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
                Html => tree_sitter_html::HIGHLIGHTS_QUERY.to_owned(),
                Css => tree_sitter_css::HIGHLIGHTS_QUERY.to_owned(),
                Bash => tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
                Toml => format!(
                    "{}\n(bare_key) @property\n(quoted_key) @property",
                    tree_sitter_toml_ng::HIGHLIGHTS_QUERY
                ),
                Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY.to_owned(),
                Ini => tree_sitter_ini::HIGHLIGHTS_QUERY.to_owned(),
                C => tree_sitter_c::HIGHLIGHT_QUERY.to_owned(),
                Cpp => format!(
                    "{}\n{}",
                    tree_sitter_c::HIGHLIGHT_QUERY,
                    tree_sitter_cpp::HIGHLIGHT_QUERY
                ),
                Java => tree_sitter_java::HIGHLIGHTS_QUERY.to_owned(),
                CSharp => tree_sitter_c_sharp::HIGHLIGHTS_QUERY.to_owned(),
                Ruby => tree_sitter_ruby::HIGHLIGHTS_QUERY.to_owned(),
                Php => tree_sitter_php::HIGHLIGHTS_QUERY.to_owned(),
                Swift => tree_sitter_swift::HIGHLIGHTS_QUERY.to_owned(),
                Kotlin => include_str!("kotlin.scm").to_owned(),
                Sql => tree_sitter_sequel::HIGHLIGHTS_QUERY.to_owned(),
                Make => tree_sitter_make::HIGHLIGHTS_QUERY.to_owned(),
                Scss => format!(
                    "{}\n{}",
                    tree_sitter_css::HIGHLIGHTS_QUERY,
                    tree_sitter_scss::HIGHLIGHTS_QUERY
                ),
                Less => format!(
                    "{}\n{}",
                    tree_sitter_css::HIGHLIGHTS_QUERY,
                    tree_sitter_less::HIGHLIGHTS_QUERY
                ),
                Dockerfile => include_str!("dockerfile.scm").to_owned(),
                Unknown => unreachable!(),
            };
            Query::new(&lang.tree_sitter(), &source)
                .unwrap_or_else(|error| panic!("invalid {lang} highlight query: {error}"))
        })
}
