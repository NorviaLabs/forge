; Three callee shapes: `foo()`, `receiver.foo()`, `Path::foo()`. Whether
; `foo` resolves to one, several, or zero real definitions is decided later
; against the whole-repo symbol table (see store.rs) — this query only
; states the syntactic fact that a call happened and what name it named.
(call_expression
  function: (identifier) @call.callee) @call.node

(call_expression
  function: (field_expression
    field: (field_identifier) @call.callee)) @call.node

(call_expression
  function: (scoped_identifier
    name: (identifier) @call.callee)) @call.node
