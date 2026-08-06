; Basic SQL highlighting via tree-sitter-sql.

(comment) @comment

((keyword) @keyword)
((keyword)
  (#match? @keyword "^(CREATE|TABLE|SELECT|FROM|WHERE|JOIN|LEFT|RIGHT|INNER|OUTER|FULL|CROSS|ON|AS|INSERT|INTO|VALUES|UPDATE|SET|DELETE|GROUP|BY|ORDER|HAVING|LIMIT|OFFSET|UNION|ALL|DISTINCT|AND|OR|NOT|NULL|PRIMARY|KEY|FOREIGN|REFERENCES|UNIQUE|DEFAULT|CASE|WHEN|THEN|ELSE|END|EXISTS|IN|BETWEEN|LIKE|IS|RETURNING|WITH|ASC|DESC)$"i))

(relation (object_reference (identifier) @variable.namespace))
(column_definition (identifier) @property)

((identifier) @variable)
((identifier)
  (#match? @identifier "^(serial|bigserial|int|integer|bigint|smallint|numeric|decimal|varchar|text|boolean|bool|timestamp|date|time|uuid|json|jsonb)$"i)
  @type.builtin)

(string) @string
(number) @number
(boolean) @boolean
(parameter) @variable.parameter
