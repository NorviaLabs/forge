; `foo()` and `pkg.Foo()` / `receiver.Method()` — Go's grammar uses the same
; selector_expression node for both a package-qualified call and a method
; call, so (as in Python) the two are honestly indistinguishable by name
; alone here.
(call_expression function: (identifier) @call.callee) @call.node

(call_expression
  function: (selector_expression
    field: (field_identifier) @call.callee)) @call.node
