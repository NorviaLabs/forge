; Both `extends` and `implements` are modeled as `implements` edges — both
; represent an is-a/contract relationship; v1 doesn't distinguish them.
(class_declaration
  name: (type_identifier) @impl.type
  (class_heritage
    (extends_clause value: (identifier) @impl.trait)))

(class_declaration
  name: (type_identifier) @impl.type
  (class_heritage
    (implements_clause (type_identifier) @impl.trait)))
