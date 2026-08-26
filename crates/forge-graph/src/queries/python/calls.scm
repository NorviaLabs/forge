; `foo()` and `receiver.foo()` — Python has no separate path-call syntax
; distinct from attribute access, so `module.func()` and `obj.method()`
; look identical here; both are honestly ambiguous by name alone.
(call function: (identifier) @call.callee) @call.node

(call
  function: (attribute
    attribute: (identifier) @call.callee)) @call.node
