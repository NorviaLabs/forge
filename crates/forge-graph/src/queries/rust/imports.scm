; Common `use` shapes. Deliberately not exhaustive (renames via `as`,
; nested `use foo::{bar::{Baz}}` groups, and glob `use foo::*` are not
; captured) — imports are a best-effort syntactic fact in v1, not the
; feature's core value; calls/definitions are.
(use_declaration
  argument: (scoped_identifier
    name: (identifier) @import.name))

(use_declaration
  argument: (identifier) @import.name)

(use_declaration
  argument: (scoped_use_list
    list: (use_list
      (identifier) @import.name)))
