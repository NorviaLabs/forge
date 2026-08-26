; Go's grammar already distinguishes free functions from methods by node
; kind — no ancestor-walk classification needed here, unlike Rust/Python.
(function_declaration name: (identifier) @function.name) @function.free

(method_declaration name: (field_identifier) @function.name) @function.method
