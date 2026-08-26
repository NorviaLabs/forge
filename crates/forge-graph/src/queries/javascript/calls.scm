(call_expression function: (identifier) @call.callee) @call.node

(call_expression
  function: (member_expression
    property: (property_identifier) @call.callee)) @call.node
