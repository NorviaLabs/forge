; `impl Trait for Type` where both are simple (non-generic, non-path-
; qualified) identifiers. `impl<T> Trait<T> for Type<T>` and
; `impl foo::Trait for Type` are not matched — a documented v1
; approximation, not an attempt at full trait-resolution.
(impl_item
  trait: (type_identifier) @impl.trait
  type: (type_identifier) @impl.type)
