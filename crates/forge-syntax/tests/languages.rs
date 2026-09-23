use forge_syntax::{
    detect_from_path, highlight, parse_and_capture, HighlightClass, HighlightTheme,
};

#[test]
fn supported_formats_have_semantic_colors() {
    let cases = [
        (
            "main.c",
            "int main() { return 42; }",
            "return",
            HighlightClass::Keyword,
        ),
        (
            "main.cpp",
            "class Widget {};",
            "class",
            HighlightClass::Keyword,
        ),
        (
            "App.java",
            "class App { String name = \"hello\"; }",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "App.cs",
            "class App { string name = \"hello\"; }",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "main.rb",
            "def hello\n  \"hello\"\nend",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "main.php",
            "<?php echo \"hello\";",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "main.swift",
            "let name = \"hello\"",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "main.kt",
            "fun main() { println(\"hello\") }",
            "fun",
            HighlightClass::Keyword,
        ),
        (
            "query.sql",
            "SELECT name FROM users WHERE id = 42;",
            "SELECT",
            HighlightClass::Keyword,
        ),
        (
            "Dockerfile",
            "FROM alpine\nRUN echo hello\n",
            "FROM",
            HighlightClass::Keyword,
        ),
        (
            "Makefile",
            "# build\nall:\n\techo hello\n",
            "# build",
            HighlightClass::Comment,
        ),
        (
            "config.yaml",
            "name: \"hello\"\nenabled: true\n",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "config.ini",
            "[server]\nname=hello\n",
            "name",
            HighlightClass::Property,
        ),
        (
            "index.html",
            "<div class=\"hello\">Hello</div>",
            "div",
            HighlightClass::Tag,
        ),
        (
            "style.css",
            "body { color: red; }",
            "color",
            HighlightClass::Property,
        ),
        (
            "style.scss",
            "$primary: red; body { color: $primary; }",
            "color",
            HighlightClass::Property,
        ),
        (
            "style.less",
            "@primary: red; body { color: @primary; }",
            "color",
            HighlightClass::Property,
        ),
        (
            "main.cjs",
            "const name = \"hello\";",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "main.pyi",
            "name: str = \"hello\"",
            "\"hello\"",
            HighlightClass::String,
        ),
        (
            "view.tsx",
            "const view = <div>Hello</div>;",
            "div",
            HighlightClass::Tag,
        ),
    ];
    for (path, code, token, class) in cases {
        let lang = detect_from_path(path).as_str();
        let tree = parse_and_capture(lang, code, "").unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{path}: {}",
            tree.root_node().to_sexp()
        );
        let spans = highlight(lang, code, &HighlightTheme::default());
        let start = code.find(token).unwrap();
        assert!(
            spans.iter().any(|span| span.range.start <= start
                && span.range.end >= start + token.len()
                && span.style.class == class),
            "{path}: missing {class:?} for {token}: {spans:?}"
        );
    }
}
