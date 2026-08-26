; JS's grammar names a class's name field `identifier`, not `type_identifier`
; (that's TypeScript-specific) — the one real node-kind divergence from
; `queries/typescript/types.scm`.
(class_declaration name: (identifier) @type.name) @type.node
