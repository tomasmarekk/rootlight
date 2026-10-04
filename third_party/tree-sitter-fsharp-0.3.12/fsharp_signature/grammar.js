/**
 * @file FSharp signature grammar for tree-sitter
 * @author Nikolaj Sidorenco
 * @license MIT
 * @see {@link https://fsharp.org/specs/language-spec/4.1/FSharpSpec-4.1-latest.pdf f# grammar}
 */

/* eslint-disable arrow-parens */
/* eslint-disable camelcase */
/* eslint-disable-next-line spaced-comment */
/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

module.exports = grammar(require("../fsharp/grammar"), {
  name: "fsharp_signature",

  conflicts: ($) => [
    [$.long_identifier, $._identifier_or_op],
    [$._type, $._argument_type],
    [$._type, $._curried_return_type],
    [$._type, $.nullable_type],
    [$._type, $._type_suffix_base],
    [$._type, $._union_argument_type],
    [$.long_identifier, $._bare_union_type_case],
    [$.union_type_cases],
  ],

  supertypes: ($) => [$._module_signature_elements],

  rules: {
    file: ($, _original) =>
      choice(
        $.named_module,
        repeat1($.namespace),
        repeat($._module_signature_elements),
      ),

    named_module: ($, _original) =>
      seq(
        optional($.attributes),
        "module",
        optional($.access_modifier),
        optional("rec"),
        field("name", $.long_identifier),
        repeat($._module_signature_elements),
      ),

    namespace: ($, _original) =>
      seq(
        "namespace",
        choice(
          "global",
          field("name", seq(optional("rec"), $.long_identifier)),
        ),
        repeat($._module_signature_elements),
      ),

    module_defn: ($, _original) =>
      prec.left(
        seq(
          optional($.attributes),
          "module",
          optional($.access_modifier),
          optional("rec"),
          $.identifier,
          "=",
          scoped(repeat1($._module_signature_elements), $._indent, $._dedent),
        ),
      ),

    _module_signature_elements: ($, _original) =>
      choice(
        $.value_definition,
        $.type_definition,
        $.exception_definition,
        $.import_decl,
        $.module_defn,
        $.module_abbrev,
        // Compiler and line directives are inherited extras in both profiles.
        // A main light arm would steal their shared '#' before the line name.
        $.preproc_if,
      ),

    // Inherited preprocessor rules use _module_elem for both structured
    // branches. Their contents must follow the signature profile as well.
    _module_elem: ($, _original) => $._module_signature_elements,

    value_definition: ($, _original) =>
      prec.left(
        4,
        seq(
          optional($.attributes),
          "val",
          $.value_declaration_left,
          ":",
          $.curried_spec,
          optional($.type_argument_constraints),
          optional(seq("=", field("body", $._expression_block))),
        ),
      ),

    // Signature declarations describe written types, never method bodies.
    constructor_signature: ($) =>
      seq(
        optional($.access_modifier),
        "new",
        ":",
        field("signature", $.curried_spec),
      ),

    member_signature: ($, _original) =>
      prec.left(
        seq(
          field("name", $.identifier),
          optional($.type_arguments),
          ":",
          field("signature", $.curried_spec),
          optional(
            choice(
              seq("with", "get"),
              seq("with", "set"),
              seq("with", "get", ",", "set"),
              seq("with", "set", ",", "get"),
            ),
          ),
        ),
      ),

    member_defn: ($, _original) =>
      prec.left(
        seq(
          optional($.attributes),
          choice(
            $.constructor_signature,
            seq(
              optional("static"), "member", optional("inline"),
              optional($.access_modifier), $.member_signature,
            ),
            seq(
              optional($.access_modifier), optional("static"), "abstract",
              optional("member"), optional("inline"), $.member_signature,
            ),
            seq(
              choice("override", "default"),
              optional($.access_modifier), $.member_signature,
            ),
            seq(
              optional("static"), "val", optional("mutable"),
              optional($.access_modifier), field("name", $.identifier),
              ":", field("type", $._type),
            ),
          ),
        ),
      ),

    _type_extension_inner: ($, _original) =>
      repeat1($._type_defn_elements),

    interface_implementation: ($, _original) =>
      prec.left(seq("interface", $._type)),

    class_inherits_decl: ($, _original) =>
      prec.left(
        seq(
          "inherit",
          scoped(
            choice($._type, alias($._inline_multiline_generic_type, $.generic_type)),
            $._indent, $._dedent,
          ),
        ),
      ),

    anon_type_defn: ($, _original) =>
      prec.left(
        seq(
          $.type_name,
          "=",
          choice(
            alias($.inline_line_comment, $.line_comment),
            scoped($._class_type_body, $._indent, $._dedent),
            seq(
              choice("begin", "class"),
              scoped(optional(seq(optional($._newline), optional($._class_type_body))), $._indent, $._dedent),
              "end",
            ),
            seq(
              alias($._struct_begin, "struct"),
              scoped(repeat($._type_defn_elements), $._indent, $._dedent),
              "end",
            ),
          ),
        ),
      ),

    _expression_block: ($, _original) =>
      choice(seq($._indent, $._expression, $._dedent), $._expression),

    _expression: ($, _original) =>
      choice("null", $.const, $.long_identifier_or_op, $.paren_expression),

    paren_expression: ($, _original) =>
      prec(1, seq("(", choice($.tuple_expression, $._expression), ")")),

    tuple_expression: ($, _original) =>
      prec.left(seq($._expression, ",", $._expression, repeat(seq(",", $._expression)))),
  },
});

/**
 *
 * @param {Rule} rule
 *
 * @param {Rule} indent
 *
 * @param {Rule} dedent
 *
 * @return {Rule}
 */
function scoped(rule, indent, dedent) {
  return field("block", seq(indent, rule, dedent));
}
