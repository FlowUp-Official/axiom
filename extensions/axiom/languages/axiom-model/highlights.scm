; Highlighting for the Axiom Model (.axm) language.
;
; The grammar is currently the SQL tree-sitter grammar (see extension.toml)
; until a dedicated tree-sitter-axm grammar exists, so the node captures below
; cover what that grammar can emit (comments, strings, numbers, SQL `$param`
; placeholders) plus spelling-based fallbacks for the .axm declaration
; scaffolding, which the SQL grammar parses as generic identifiers or ERROR
; regions. A dedicated grammar will be able to make these precise.

(comment) @comment
(string) @string
(number) @number
(boolean) @boolean
(parameter) @variable.parameter

((identifier) @variable)

; .axm declaration keywords (`export` does not exist in the grammar).
((identifier) @keyword
  (#match? @keyword "^(import|type|model|extends|select|query|from|to)$"))

; Canonical PascalCase primitives.
((identifier) @type.builtin
  (#match? @identifier "^(String|Int|BigInt|Float|Boolean|UUID|Date|DateTime|Json|Bytes)$"))

; Validators and transforms.
((identifier) @function.call
  (#match? @function.call
    "^(trim|lowercase|uppercase|email|url|uuid|ulid|ipv4|ipv6|isodate|alphanumeric|nonempty|min|max|min_length|max_length|regex)$"))