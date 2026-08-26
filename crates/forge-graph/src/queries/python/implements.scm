; One match per base class, all sharing the same `impl.type` — Python's
; multiple inheritance (`class Foo(Bar, Baz):`) naturally becomes multiple
; `implements` edges from the same query.
(class_definition
  name: (identifier) @impl.type
  superclasses: (argument_list (identifier) @impl.trait))
