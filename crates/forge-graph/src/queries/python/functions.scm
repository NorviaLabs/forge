; Captures both `def` at module scope and inside a class body. The
; extractor classifies function vs. method by walking ancestors afterward
; (same approach as Rust's `is_method`), since Python's grammar doesn't
; distinguish them by node kind the way Go/TS/JS do.
(function_definition name: (identifier) @function.name) @function.node
