; TS/JS's grammar already distinguishes a free function from a class method
; by node kind — no ancestor-walk classification needed here, unlike
; Rust/Python.
(function_declaration name: (identifier) @function.name) @function.free

(method_definition name: (property_identifier) @function.name) @function.method
