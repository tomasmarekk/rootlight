/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Operator precedence from the official Q# language specification.
// https://learn.microsoft.com/en-us/azure/quantum/user-guide/language/expressions/precedenceandassociativity
const PREC = {
  ASSIGN: 0,
  UPDATE: 1,     // w/ <-
  RANGE: 2,      // ..
  TERNARY: 3,    // ? |
  OR: 4,         // or
  AND: 5,        // and
  BIT_OR: 6,     // |||
  BIT_XOR: 7,    // ^^^
  BIT_AND: 8,    // &&&
  EQUALITY: 9,   // == !=
  COMPARE_LTE: 10, // <=
  COMPARISON: 11,  // < > >=
  SHIFT: 12,     // <<< >>>
  ADD: 13,       // + -
  MUL: 14,       // * / %
  EXP: 15,       // ^
  PREFIX: 16,    // ~~~ not -
  CALL: 17,      // ( )
  FUNCTOR: 18,   // Adjoint Controlled
  POSTFIX: 19,   // !
  ACCESS: 20,    // . [] ::
  LAMBDA: 21,    // -> =>
};

module.exports = grammar({
  name: "qsharp",

  extras: ($) => [/\s/, $.comment, $.doc_comment],

  externals: ($) => [
    $.float_literal,
    $._error_sentinel, // unused token to detect error recovery mode
  ],

  supertypes: ($) => [
    $.literal,
    $.type,
  ],

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$._expression_no_block, $.path],
    [$.expression_statement, $._expression],
    [$.type_def, $.tuple_type],
    [$._top_level_item, $._statement],
  ],

  rules: {
    // =====================================================================
    // Top-level structure
    // =====================================================================

    source_file: ($) =>
      repeat(choice($.namespace, $._top_level_item, $._statement)),

    namespace: ($) =>
      seq("namespace", field("name", $.path_or_identifier), "{", repeat($._namespace_item), "}"),

    _namespace_item: ($) => $._top_level_item,

    _top_level_item: ($) =>
      choice(
        $.import_decl,
        $.export_decl,
        $.open_decl,
        $.callable_decl,
        $.newtype_decl,
        $.struct_decl
      ),

    // =====================================================================
    // Import / Export / Open
    // =====================================================================

    import_decl: ($) => seq("import", sep1($.import_item, ","), ";"),

    import_item: ($) =>
      seq($.import_path, optional(seq("as", $.identifier))),

    import_path: ($) =>
      choice(
        seq($.identifier, repeat(seq(".", $.identifier)), ".", "*"),
        seq($.identifier, repeat(seq(".", $.identifier)))
      ),

    export_decl: ($) => seq(repeat($.attribute), "export", sep1($.export_item, ","), ";"),

    export_item: ($) => seq($.path_or_identifier, optional(seq("as", $.identifier))),

    open_decl: ($) =>
      seq("open", $.path_or_identifier, optional(seq("as", $.identifier)), ";"),

    // =====================================================================
    // Callable declarations (operation / function)
    // =====================================================================

    callable_decl: ($) =>
      seq(
        repeat($.attribute),
        optional("internal"),
        choice("operation", "function"),
        field("name", $.identifier),
        optional($.type_params),
        field("params", $.param_list),
        ":",
        field("return_type", $.type),
        optional($.functor_clause),
        field("body", $.callable_body)
      ),

    attribute: ($) =>
      seq("@", $.path_or_identifier, optional(seq("(", optional($._expression), ")"))),

    type_params: ($) => seq("<", sep1($.type_param, ","), ">"),

    type_param: ($) =>
      seq($.apos_ident, optional(seq(":", sep1($.identifier, "+")))),

    param_list: ($) => seq("(", optional(sep1($._param, ",")), ")"),

    _param: ($) => choice($.typed_param, $.tuple_param),

    typed_param: ($) =>
      seq(field("name", $.identifier), ":", field("type", $.type)),

    tuple_param: ($) => seq("(", sep1($._param, ","), ")"),

    functor_clause: ($) => seq("is", $.functor_expr),

    functor_expr: ($) =>
      choice(
        "Adj",
        "Ctl",
        prec.left(1, seq($.functor_expr, "+", $.functor_expr)),
        prec.left(2, seq($.functor_expr, "*", $.functor_expr)),
        seq("(", $.functor_expr, ")")
      ),

    callable_body: ($) => choice($.block, $.specialization_list),

    specialization_list: ($) => seq("{", repeat1($.specialization), "}"),

    specialization: ($) =>
      seq(
        $.spec_name,
        choice(
          seq($.spec_generator, ";"),
          seq(optional($.spec_param), $.block)
        )
      ),

    spec_name: ($) =>
      choice("body", "adjoint", seq("controlled", optional("adjoint"))),

    spec_generator: ($) =>
      choice("auto", "distribute", "intrinsic", "invert", "self"),

    // `body ...`, `adjoint ...`, `body (cs, ...)`, `controlled (cs, ...)`
    spec_param: ($) =>
      choice("...", seq("(", sep1(choice($.identifier, "..."), ","), ")")),

    // =====================================================================
    // Type declarations
    // =====================================================================

    newtype_decl: ($) =>
      seq(optional("internal"), "newtype", field("name", $.identifier), "=", $.type_def, ";"),

    type_def: ($) =>
      choice(
        seq($.identifier, ":", $.type),
        $.type,
        seq("(", sep1($.type_def, ","), ")")
      ),

    struct_decl: ($) =>
      seq(optional("internal"), "struct", field("name", $.identifier), "{", optional(seq(sep1($.field_def, ","), optional(","))), "}"),

    field_def: ($) =>
      seq(field("name", $.identifier), ":", field("type", $.type)),

    // =====================================================================
    // Statements
    // =====================================================================

    _statement: ($) =>
      choice(
        $.let_statement,
        $.mutable_statement,
        $.set_statement,
        $.use_statement,
        $.borrow_statement,
        $.return_statement,
        $.fail_statement,
        $.import_decl,
        $.expression_statement,
        $.struct_decl,
        $.empty_statement
      ),

    let_statement: ($) =>
      seq("let", $.pattern, optional(seq(":", $.type)), "=", $._expression, ";"),

    mutable_statement: ($) =>
      seq("mutable", $.pattern, optional(seq(":", $.type)), "=", $._expression, ";"),

    // `set x = expr;`, `set x += expr;`, `set (a, b) = (1, 2);`
    set_statement: ($) => seq("set", $._expression, ";"),

    use_statement: ($) =>
      seq("use", $.pattern, optional(seq(":", $.type)), "=", $.qubit_init, choice(";", $.block)),

    borrow_statement: ($) =>
      seq("borrow", $.pattern, optional(seq(":", $.type)), "=", $.qubit_init, choice(";", $.block)),

    return_statement: ($) => seq("return", $._expression, ";"),

    fail_statement: ($) => seq("fail", $._expression, ";"),

    expression_statement: ($) =>
      choice(
        seq($._expression_no_block, ";"),
        $._expression_with_block,
        // Q# allows omitting trailing semicolons for the last expression in a block
        prec(-1, $._expression_no_block)
      ),

    empty_statement: (_) => ";",

    // =====================================================================
    // Qubit initializers
    // =====================================================================

    qubit_init: ($) =>
      choice(
        seq("Qubit", "(", ")"),
        seq("Qubit", "[", $._expression, "]"),
        seq("(", sep1($.qubit_init, ","), ")")
      ),

    // =====================================================================
    // Patterns
    // =====================================================================

    pattern: ($) =>
      choice(
        $.identifier,
        $.discard,
        seq("(", sep1($.pattern, ","), ")")
      ),

    discard: (_) => "_",

    // =====================================================================
    // Types
    // =====================================================================

    type: ($) =>
      choice(
        $.builtin_type,
        $.apos_ident,
        $.path,
        $.identifier,
        $.array_type,
        $.tuple_type,
        $.callable_type,
        $.discard // type hole — compiler-inferred type
      ),

    builtin_type: (_) =>
      choice("Unit", "Int", "BigInt", "Double", "Bool", "String", "Qubit", "Result", "Pauli", "Range"),

    array_type: ($) => prec.left(seq($.type, "[", "]")),

    tuple_type: ($) => seq("(", sep1($.type, ","), ")"),

    callable_type: ($) =>
      prec.right(seq($.type, choice("->", "=>"), $.type, optional(seq("is", $.functor_expr)))),

    // =====================================================================
    // Expressions
    // =====================================================================

    _expression: ($) =>
      choice($._expression_no_block, $._expression_with_block),

    _expression_no_block: ($) =>
      choice(
        $.identifier,
        $.path,
        $.literal,
        $.parenthesized_expression,
        $.unit_expression,
        $.tuple_expression,
        $.array_expression,
        $.array_repeat_expression,
        $.string_interpolation,
        $.unary_expression,
        $.binary_expression,
        $.assignment_expression,
        $.compound_assignment_expression,
        $.update_expression,
        $.ternary_expression,
        $.call_expression,
        $.index_expression,
        $.field_expression,
        $.unwrap_expression,
        $.lambda_expression,
        $.range_expression,
        $.step_range_expression,
        $.open_range_start,
        $.open_range_end,
        $.full_range,
        $.new_expression,
        $.discard
      ),

    _expression_with_block: ($) =>
      choice(
        $.block,
        $.if_expression,
        $.for_expression,
        $.while_expression,
        $.repeat_expression,
        $.conjugation_expression
      ),

    parenthesized_expression: ($) => seq("(", $._expression, ")"),

    unit_expression: (_) => seq("(", ")"),

    tuple_expression: ($) =>
      seq("(", $._expression, ",", optional(sep1($._expression, ",")), ")"),

    array_expression: ($) =>
      seq("[", optional(sep1($._expression, ",")), "]"),

    array_repeat_expression: ($) =>
      seq("[", $._expression, ",", "size", "=", $._expression, "]"),

    string_interpolation: ($) =>
      seq('$"', repeat(choice($.interpolation_content, $.interpolation_expression)), '"'),

    interpolation_content: (_) => /[^"{}\\]+|\\./,

    interpolation_expression: ($) => seq("{", $._expression, "}"),

    // --- Operators ---

    unary_expression: ($) =>
      choice(
        prec(PREC.PREFIX, seq(choice("-", "+", "not", "~~~"), $._expression)),
        prec(PREC.FUNCTOR, seq(choice("Adjoint", "Controlled"), $._expression))
      ),

    binary_expression: ($) =>
      choice(
        prec.left(PREC.OR, seq($._expression, "or", $._expression)),
        prec.left(PREC.AND, seq($._expression, "and", $._expression)),
        prec.left(PREC.EQUALITY, seq($._expression, choice("==", "!="), $._expression)),
        prec.left(PREC.COMPARE_LTE, seq($._expression, "<=", $._expression)),
        prec.left(PREC.COMPARISON, seq($._expression, choice("<", ">", ">="), $._expression)),
        prec.left(PREC.BIT_OR, seq($._expression, "|||", $._expression)),
        prec.left(PREC.BIT_XOR, seq($._expression, "^^^", $._expression)),
        prec.left(PREC.BIT_AND, seq($._expression, "&&&", $._expression)),
        prec.left(PREC.SHIFT, seq($._expression, choice("<<<", ">>>"), $._expression)),
        prec.left(PREC.ADD, seq($._expression, choice("+", "-"), $._expression)),
        prec.left(PREC.MUL, seq($._expression, choice("*", "/", "%"), $._expression)),
        prec.right(PREC.EXP, seq($._expression, "^", $._expression))
      ),

    assignment_expression: ($) =>
      prec.right(PREC.ASSIGN, seq($._expression, "=", $._expression)),

    compound_assignment_expression: ($) =>
      prec.right(
        PREC.ASSIGN,
        seq(
          $._expression,
          choice("+=", "-=", "*=", "/=", "%=", "^=", "&&&=", "|||=", "^^^=", "<<<=", ">>>=", "and=", "or=", "w/="),
          $._expression
        )
      ),

    update_expression: ($) =>
      prec.left(PREC.UPDATE, seq($._expression, "w/", $._expression, "<-", $._expression)),

    ternary_expression: ($) =>
      prec.right(PREC.TERNARY, seq($._expression, "?", $._expression, "|", $._expression)),

    // Q# *documents* explicit type arguments at call sites, e.g. `CControlled<'T3>`
    // or `Mapped<String[], _>(...)` (see Microsoft's "Type parameterizations" and
    // "Type inference" pages). But the current reference compiler doesn't parse
    // them: qsc_parse/src/expr.rs treats `<` only as BinOp::Lt and call_op accepts
    // only `lhs ( args )` (the classic C#/F# compiler did support the syntax). We
    // match the current compiler, so `Foo<Int>(x)` parses as the comparison chain
    // `Foo < Int > (x)`. Supporting it would mean resolving a `<` ambiguity that the
    // reference parser itself leaves unresolved here.
    call_expression: ($) =>
      prec(PREC.CALL, seq($._expression, "(", optional(sep1($._expression, ",")), ")")),

    index_expression: ($) =>
      prec(PREC.ACCESS, seq($._expression, "[", $._expression, "]")),

    field_expression: ($) =>
      prec(PREC.ACCESS, seq($._expression, choice(".", "::"), field("field", $.identifier))),

    unwrap_expression: ($) =>
      prec(PREC.POSTFIX, seq($._expression, "!")),

    lambda_expression: ($) =>
      prec.right(PREC.LAMBDA, seq($._expression, choice("->", "=>"), $._expression)),

    range_expression: ($) =>
      prec.left(PREC.RANGE, seq($._expression, "..", $._expression)),

    step_range_expression: ($) =>
      prec.left(PREC.RANGE + 1, seq($._expression, "..", $._expression, "..", $._expression)),

    open_range_start: ($) =>
      prec.right(PREC.ACCESS + 1, seq($._expression, "...")),

    open_range_end: ($) =>
      prec.right(PREC.ACCESS + 1, seq("...", $._expression)),

    full_range: (_) => "...",

    new_expression: ($) =>
      choice(
        seq("new", $.path_or_identifier, "{", optional($.struct_fields), "}"),
        seq("new", $.type, "[", $._expression, "]")
      ),

    struct_fields: ($) =>
      choice(
        seq("...", $._expression, optional(seq(",", sep1($.field_assign, ","), optional(",")))),
        seq(sep1($.field_assign, ","), optional(","))
      ),

    field_assign: ($) =>
      seq(field("name", $.identifier), "=", field("value", $._expression)),

    // --- Block expressions ---

    block: ($) => seq("{", repeat($._statement), "}"),

    if_expression: ($) =>
      seq(
        "if",
        field("condition", $._expression),
        field("body", $.block),
        repeat($.elif_clause),
        optional($.else_clause)
      ),

    elif_clause: ($) =>
      seq("elif", field("condition", $._expression), field("body", $.block)),

    else_clause: ($) => seq("else", $.block),

    for_expression: ($) =>
      seq("for", $.pattern, "in", $._expression, $.block),

    while_expression: ($) =>
      seq("while", $._expression, $.block),

    repeat_expression: ($) =>
      choice(
        seq("repeat", $.block, "until", $._expression, "fixup", $.block),
        seq("repeat", $.block, "until", $._expression, ";")
      ),

    conjugation_expression: ($) =>
      seq("within", $.block, "apply", $.block),

    // =====================================================================
    // Paths and identifiers
    // =====================================================================

    path_or_identifier: ($) => choice($.path, $.identifier),

    path: ($) => prec.left(seq($.identifier, repeat1(seq(".", $.identifier)))),

    // =====================================================================
    // Literals
    // =====================================================================

    literal: ($) =>
      choice(
        $.integer_literal,
        $.bigint_literal,
        $.float_literal,
        $.imaginary_literal,
        $.string_literal,
        $.bool_literal,
        $.result_literal,
        $.pauli_literal
      ),

    integer_literal: (_) =>
      token(choice(/0[bB][01_]+/, /0[oO][0-7_]+/, /0[xX][0-9a-fA-F_]+/, /[0-9][0-9_]*/)),

    bigint_literal: (_) =>
      token(choice(/0[bB][01_]+L/, /0[oO][0-7_]+L/, /0[xX][0-9a-fA-F_]+L/, /[0-9][0-9_]*L/)),

    // float_literal: handled by external scanner (src/scanner.c)
    // to disambiguate `1.` (float) from `0..` (integer + range)

    imaginary_literal: (_) =>
      token(/[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9][0-9_]*)?i/),

    string_literal: (_) => token(seq('"', repeat(choice(/[^"\\]/, /\\./)), '"')),

    bool_literal: (_) => choice("true", "false"),

    result_literal: (_) => choice("Zero", "One"),

    pauli_literal: (_) => choice("PauliI", "PauliX", "PauliY", "PauliZ"),

    // =====================================================================
    // Identifiers and comments
    // =====================================================================

    // Note: `_` alone is NOT an identifier in Q# — it's a discard/hole.
    // The word rule handles this: tree-sitter matches `_` as keyword, not identifier.
    identifier: (_) => /[a-zA-Z_][a-zA-Z0-9_]*/,

    apos_ident: (_) => /'[a-zA-Z_][a-zA-Z0-9_]*/,

    // `//` is a normal comment; `///` (exactly three slashes) is a doc comment.
    // Four or more leading slashes (`////`) is a NORMAL comment, matching the Q#
    // compiler's lexer (qsc_parse/src/lex/raw.rs: Doc only when the 3rd char is
    // `/` and the 4th is not). The comment body's first alternative forbids a
    // leading slash so `///x` is left to doc_comment; the second alternative
    // (`//` again) lets `////…` be consumed here as a normal comment.
    comment: (_) => token(seq("//", /[^\/\n][^\n]*|\/\/[^\n]*|/)),

    doc_comment: (_) => token(seq("///", /[^\/\n][^\n]*|/)),
  },
});

function sep1(rule, delimiter) {
  return seq(rule, repeat(seq(delimiter, rule)));
}
