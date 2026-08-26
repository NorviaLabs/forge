; Captures every `fn`, whether free-standing or inside an `impl`/`trait`
; body. Rust's extractor classifies function vs. method by walking each
; match's ancestors afterward (see rust.rs) rather than duplicating this
; pattern once per context.
(function_item name: (identifier) @function.name) @function.node
