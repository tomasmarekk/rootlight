/**
 * tree-sitter-abap — highlight-grade tree-sitter grammar for ABAP.
 *
 * Scope: syntax highlighting, outline, brackets, indentation. Deliberately
 * NOT a full ABAP parser — statements the grammar doesn't model structurally
 * are consumed as token soup (`statement`), and abaplint provides real
 * analysis. Exotic syntax must degrade to ERROR nodes gracefully, never hang.
 *
 * ABAP lexical facts this grammar encodes:
 *  - keywords are case-insensitive (see kw())
 *  - `*` in column 1 comments the whole line (external scanner — column check)
 *  - `"` comments to end of line anywhere
 *  - statements end with `.`; `:` + `,` chain statements
 *  - identifiers may be namespaced (/DRU/CL_FOO) and contain % $
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

/** Case-insensitive keyword token. Every keyword goes through here. */
function kw(word, precedence = 2) {
  const pattern = word
    .split('')
    .map((c) => (/[a-z]/i.test(c) ? `[${c.toLowerCase()}${c.toUpperCase()}]` : c))
    .join('');
  return alias(token(prec(precedence, new RegExp(pattern))), word.toLowerCase());
}

function sep1(rule, separator) {
  return seq(rule, repeat(seq(separator, rule)));
}

module.exports = grammar({
  name: 'abap',

  externals: ($) => [
    $.star_comment, // `*` full-line comment — only valid at column 0
  ],

  extras: ($) => [/[ \t\r\n]+/, $.line_comment, $.star_comment, $.pragma],

  word: ($) => $.identifier,

  conflicts: () => [],

  rules: {
    source_file: ($) => repeat($._top_level),

    _top_level: ($) =>
      choice(
        $.class_definition,
        $.class_forward_declaration,
        $.class_implementation,
        $.interface_definition,
        $.interface_forward_declaration,
        $.form_definition,
        $.function_definition,
        $.module_definition,
        $.program_statement,
        $._simple_statement,
      ),

    // ------------------------------------------------------------------
    // Statements usable anywhere (top level, class sections, bodies)
    // ------------------------------------------------------------------

    _simple_statement: ($) =>
      choice(
        $.data_declaration,
        $.inline_data_statement,
        $.methods_declaration,
        $.statement,
        $.empty_statement,
      ),

    // REPORT zfoo. / PROGRAM zfoo. / FUNCTION-POOL zfoo.
    program_statement: ($) =>
      seq(
        field('kind', choice(kw('report'), kw('program'), kw('function-pool'))),
        field('name', $.identifier),
        repeat($._token),
        '.',
      ),

    // DATA / TYPES / CONSTANTS / ... declarations, single or chained.
    data_declaration: ($) =>
      seq(
        field(
          'kind',
          choice(
            kw('data'),
            kw('class-data'),
            kw('types'),
            kw('constants'),
            kw('statics'),
            kw('field-symbols'),
            kw('parameters'),
            kw('select-options'),
            kw('tables'),
            kw('ranges'),
            kw('nodes'),
          ),
        ),
        choice(seq(':', sep1($.declaration_entry, ',')), $.declaration_entry),
        '.',
      ),

    // DATA(lv_x) = ... / FINAL(lv_x) = ...  — inline declaration as
    // statement head; the rest of the statement is token soup.
    inline_data_statement: ($) =>
      seq(
        choice(kw('data'), kw('final')),
        token.immediate('('),
        field('name', $.identifier),
        ')',
        repeat($._chain_token),
        '.',
      ),

    declaration_entry: ($) =>
      seq(
        field('name', choice($.identifier, $.field_symbol)),
        repeat($._entry_token),
      ),

    // METHODS / CLASS-METHODS declarations, single or chained.
    methods_declaration: ($) =>
      seq(
        field('kind', choice(kw('methods'), kw('class-methods'))),
        choice(seq(':', sep1($.method_entry, ',')), $.method_entry),
        '.',
      ),

    method_entry: ($) =>
      seq(field('name', $._method_name), repeat($._entry_token)),

    // Anything else: token soup terminated by `.` — may contain `:`/`,` chains.
    statement: ($) => seq(repeat1($._chain_token), '.'),

    empty_statement: () => '.',

    // ------------------------------------------------------------------
    // CLASS
    // ------------------------------------------------------------------

    class_definition: ($) =>
      seq(
        kw('class'),
        field('name', $.identifier),
        kw('definition'),
        repeat($._token),
        '.',
        repeat($._class_definition_item),
        kw('endclass'),
        '.',
      ),

    // CLASS x DEFINITION DEFERRED. / CLASS x DEFINITION LOCAL FRIENDS y.
    class_forward_declaration: ($) =>
      seq(
        kw('class'),
        field('name', $.identifier),
        kw('definition'),
        choice(
          seq(kw('deferred'), repeat($._token)),
          seq(kw('local'), kw('friends'), repeat($._token)),
        ),
        '.',
      ),

    _class_definition_item: ($) =>
      choice($.section_declaration, $._simple_statement),

    // PUBLIC SECTION. / PROTECTED SECTION. / PRIVATE SECTION.
    section_declaration: ($) =>
      seq(
        field('visibility', choice(kw('public'), kw('protected'), kw('private'))),
        kw('section'),
        '.',
      ),

    class_implementation: ($) =>
      seq(
        kw('class'),
        field('name', $.identifier),
        kw('implementation'),
        '.',
        repeat(choice($.method_implementation, $._simple_statement)),
        kw('endclass'),
        '.',
      ),

    method_implementation: ($) =>
      seq(
        kw('method'),
        field('name', $._method_name),
        repeat($._token), // BY DATABASE PROCEDURE ..., BY KERNEL MODULE ...
        '.',
        repeat($._simple_statement),
        kw('endmethod'),
        '.',
      ),

    // ------------------------------------------------------------------
    // INTERFACE
    // ------------------------------------------------------------------

    interface_definition: ($) =>
      seq(
        kw('interface'),
        field('name', $.identifier),
        repeat($._token), // PUBLIC
        '.',
        repeat($._simple_statement),
        kw('endinterface'),
        '.',
      ),

    interface_forward_declaration: ($) =>
      seq(
        kw('interface'),
        field('name', $.identifier),
        kw('deferred'),
        repeat($._token),
        '.',
      ),

    // ------------------------------------------------------------------
    // FORM / FUNCTION / MODULE
    // ------------------------------------------------------------------

    form_definition: ($) =>
      seq(
        kw('form'),
        field('name', $.identifier),
        repeat($._chain_token), // USING/CHANGING/TABLES parameter lists
        '.',
        repeat($._simple_statement),
        kw('endform'),
        '.',
      ),

    function_definition: ($) =>
      seq(
        kw('function'),
        field('name', $.identifier),
        repeat($._token),
        '.',
        repeat($._simple_statement),
        kw('endfunction'),
        '.',
      ),

    module_definition: ($) =>
      seq(
        kw('module'),
        field('name', $.identifier),
        repeat($._token), // INPUT / OUTPUT
        '.',
        repeat($._simple_statement),
        kw('endmodule'),
        '.',
      ),

    // ------------------------------------------------------------------
    // Tokens
    // ------------------------------------------------------------------

    // Method names: plain, interface-prefixed (zif~meth), aliased (meth~sub),
    // namespaced (/dru/if_x~y handled by identifier pattern).
    _method_name: ($) =>
      seq($.identifier, optional(seq(token.immediate('~'), $.identifier))),

    // In-statement token, commas/colons excluded (declaration entries).
    _entry_token: ($) => choice($._token, ':'),

    // In-statement token including chain punctuation (generic statements).
    _chain_token: ($) => choice($._token, ':', ','),

    _token: ($) =>
      choice(
        $.identifier,
        $.field_symbol,
        $.number,
        $.string,
        $.backtick_string,
        $.string_template,
        $.text_symbol_key,
        $.macro_parameter,
        $.operator,
        $.parenthesized_group,
        '[',
        ']',
      ),

    // Parens grouped so commas inside VALUE #( a = 1, b = 2 ) etc. don't
    // terminate chained declaration entries.
    parenthesized_group: ($) => seq('(', repeat($._chain_token), ')'),

    // Namespaced (/DRU/CL_X), internal (%_...), sy fields, plain names,
    // and `!`-escaped names in signatures (!iv_param).
    identifier: () => /!?[A-Za-z_%$\/][A-Za-z0-9_%$\/]*/,

    field_symbol: () => /<[A-Za-z_%\/][A-Za-z0-9_%\/]*>/,

    number: () => /\d+/,

    // '...' with '' as escaped quote
    string: () => /'([^'\n]|'')*'/,

    // `...` literal with `` escape
    backtick_string: () => /`([^`\n]|``)*`/,

    // |...{ expr }...| string template
    string_template: ($) =>
      seq(
        '|',
        repeat(choice($.template_text, $.template_substitution)),
        '|',
      ),

    template_text: () => token(prec(1, /([^|{}\\\n]|\\.)+/)),

    template_substitution: ($) => seq('{', repeat($._chain_token), '}'),

    // 'text'(001) — the (key) part after a literal; lexed as tokens anyway,
    // but this keeps `(001)` from looking like a paren group with a number.
    text_symbol_key: () => token(seq('(', /[A-Za-z0-9_]{1,3}/, ')')),

    // &1 ... &9 in macro definitions
    macro_parameter: () => /&[0-9]/,

    operator: () =>
      token(
        choice(
          '->*', '->', '=>', '~',
          '=', '<>', '><', '<=', '>=', '<', '>',
          '?=', '+=', '-=', '*=', '/=', '&&=', '&&', '&',
          '+', '-', '*', '**', '/',
          '@', '#', '?',
        ),
      ),

    // `"` comments to end of line (includes "! ABAP Doc)
    line_comment: () => token(/"[^\n]*/),

    // ##NO_TEXT, ##NEEDED, ##SHADOW[insert] ...
    pragma: () => token(/##[A-Za-z0-9_-]+(\[[^\]\n]*\])*/),
  },
});
