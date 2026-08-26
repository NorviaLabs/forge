; JS has no `implements` keyword — only `extends`. Unlike TypeScript's
; grammar, JS's `class_heritage` wraps the base class expression directly
; (no intermediate `extends_clause` node).
(class_declaration
  name: (identifier) @impl.type
  (class_heritage (identifier) @impl.trait))
