; Go has no inheritance — struct embedding (a field with a type and no
; field name) is modeled as `implements` as the closest analog. An
; approximation, not literal Go semantics: embedding shares methods, it
; doesn't assert an interface contract the way `impl Trait for Type` does.
(type_spec
  name: (type_identifier) @impl.type
  type: (struct_type
    (field_declaration_list
      (field_declaration
        type: (type_identifier) @impl.trait
        !name))))
