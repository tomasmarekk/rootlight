#include <tree_sitter/parser.h>

#if defined(__GNUC__) || defined(__clang__)
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wmissing-field-initializers"
#endif

#define LANGUAGE_VERSION 14
#define STATE_COUNT 76
#define LARGE_STATE_COUNT 2
#define SYMBOL_COUNT 101
#define ALIAS_COUNT 0
#define TOKEN_COUNT 65
#define EXTERNAL_TOKEN_COUNT 0
#define FIELD_COUNT 0
#define MAX_ALIAS_SEQUENCE_LENGTH 4
#define PRODUCTION_ID_COUNT 1

enum {
  sym_inline_comment = 1,
  sym_block_comment = 2,
  anon_sym_LPAREN = 3,
  anon_sym_RPAREN = 4,
  anon_sym_private = 5,
  anon_sym_public = 6,
  anon_sym_LBRACE = 7,
  anon_sym_RBRACE = 8,
  anon_sym_SEMI = 9,
  anon_sym_COMMA = 10,
  anon_sym_COLON = 11,
  anon_sym_EQ = 12,
  anon_sym_true = 13,
  anon_sym_false = 14,
  anon_sym_DOT = 15,
  anon_sym_e = 16,
  anon_sym_E = 17,
  anon_sym_DQUOTE = 18,
  anon_sym_SQUOTE = 19,
  aux_sym__string_character_token1 = 20,
  anon_sym_void = 21,
  anon_sym_nothing = 22,
  anon_sym_bool = 23,
  anon_sym_int = 24,
  anon_sym_uint = 25,
  anon_sym_real = 26,
  anon_sym_imag = 27,
  anon_sym_complex = 28,
  anon_sym_string = 29,
  anon_sym_bytes = 30,
  anon_sym_enum = 31,
  anon_sym_record = 32,
  anon_sym_class = 33,
  anon_sym_owned = 34,
  anon_sym_shared = 35,
  anon_sym_unmanaged = 36,
  anon_sym_borrowed = 37,
  anon_sym_param = 38,
  anon_sym_const = 39,
  anon_sym_var = 40,
  anon_sym_ref = 41,
  anon_sym_constref = 42,
  anon_sym_return = 43,
  sym_identifier = 44,
  anon_sym_DOLLAR = 45,
  anon_sym__ = 46,
  sym__letter = 47,
  sym__digit = 48,
  sym__binary_digit = 49,
  anon_sym_PLUS = 50,
  anon_sym_DASH = 51,
  anon_sym_PLUS_EQ = 52,
  anon_sym_DASH_EQ = 53,
  anon_sym_STAR_EQ = 54,
  anon_sym_SLASH_EQ = 55,
  anon_sym_PERCENT_EQ = 56,
  anon_sym_STAR_STAR_EQ = 57,
  anon_sym_AMP_EQ = 58,
  anon_sym_PIPE_EQ = 59,
  anon_sym_CARET_EQ = 60,
  anon_sym_AMP_AMP_EQ = 61,
  anon_sym_PIPE_PIPE_EQ = 62,
  anon_sym_LT_LT_EQ = 63,
  anon_sym_GT_GT_EQ = 64,
  sym_source_file = 65,
  sym__comment = 66,
  sym_privacy_specifier = 67,
  sym__statement = 68,
  sym_block_statement = 69,
  sym_expression_statement = 70,
  sym_assignment_statement = 71,
  sym_statements = 72,
  sym_variable_declaration_statement = 73,
  sym_variable_declaration_list = 74,
  sym_variable_declaration = 75,
  sym_type_part = 76,
  sym_initialization_part = 77,
  sym__expression = 78,
  sym_literal_expression = 79,
  sym_variable_expression = 80,
  sym_type_expression = 81,
  sym_lvalue_expression = 82,
  sym_bool_literal = 83,
  sym_integer_literal = 84,
  sym_real_literal = 85,
  sym__exponent_part = 86,
  sym_string_literal = 87,
  sym_interpreted_string_literal = 88,
  sym__double_quote_delimited_characters = 89,
  sym__single_quote_delimited_characters = 90,
  sym__string_character = 91,
  sym_primitive_type = 92,
  sym_variable_kind = 93,
  sym_return_statement = 94,
  sym_identifier_list = 95,
  sym__digits = 96,
  sym__separator_digits = 97,
  sym__sign = 98,
  sym_assignment_operator = 99,
  aux_sym_source_file_repeat1 = 100,
};

static const char * const ts_symbol_names[] = {
  [ts_builtin_sym_end] = "end",
  [sym_inline_comment] = "inline_comment",
  [sym_block_comment] = "block_comment",
  [anon_sym_LPAREN] = "(",
  [anon_sym_RPAREN] = ")",
  [anon_sym_private] = "private",
  [anon_sym_public] = "public",
  [anon_sym_LBRACE] = "{",
  [anon_sym_RBRACE] = "}",
  [anon_sym_SEMI] = ";",
  [anon_sym_COMMA] = ",",
  [anon_sym_COLON] = ":",
  [anon_sym_EQ] = "=",
  [anon_sym_true] = "true",
  [anon_sym_false] = "false",
  [anon_sym_DOT] = ".",
  [anon_sym_e] = "e",
  [anon_sym_E] = "E",
  [anon_sym_DQUOTE] = "\"",
  [anon_sym_SQUOTE] = "'",
  [aux_sym__string_character_token1] = "_string_character_token1",
  [anon_sym_void] = "void",
  [anon_sym_nothing] = "nothing",
  [anon_sym_bool] = "bool",
  [anon_sym_int] = "int",
  [anon_sym_uint] = "uint",
  [anon_sym_real] = "real",
  [anon_sym_imag] = "imag",
  [anon_sym_complex] = "complex",
  [anon_sym_string] = "string",
  [anon_sym_bytes] = "bytes",
  [anon_sym_enum] = "enum",
  [anon_sym_record] = "record",
  [anon_sym_class] = "class",
  [anon_sym_owned] = "owned",
  [anon_sym_shared] = "shared",
  [anon_sym_unmanaged] = "unmanaged",
  [anon_sym_borrowed] = "borrowed",
  [anon_sym_param] = "param",
  [anon_sym_const] = "const",
  [anon_sym_var] = "var",
  [anon_sym_ref] = "ref",
  [anon_sym_constref] = "const ref",
  [anon_sym_return] = "return",
  [sym_identifier] = "identifier",
  [anon_sym_DOLLAR] = "$",
  [anon_sym__] = "_",
  [sym__letter] = "_letter",
  [sym__digit] = "_digit",
  [sym__binary_digit] = "_binary_digit",
  [anon_sym_PLUS] = "+",
  [anon_sym_DASH] = "-",
  [anon_sym_PLUS_EQ] = "+=",
  [anon_sym_DASH_EQ] = "-=",
  [anon_sym_STAR_EQ] = "*=",
  [anon_sym_SLASH_EQ] = "/=",
  [anon_sym_PERCENT_EQ] = "%=",
  [anon_sym_STAR_STAR_EQ] = "**=",
  [anon_sym_AMP_EQ] = "&=",
  [anon_sym_PIPE_EQ] = "|=",
  [anon_sym_CARET_EQ] = "^=",
  [anon_sym_AMP_AMP_EQ] = "&&=",
  [anon_sym_PIPE_PIPE_EQ] = "||=",
  [anon_sym_LT_LT_EQ] = "<<=",
  [anon_sym_GT_GT_EQ] = ">>=",
  [sym_source_file] = "source_file",
  [sym__comment] = "_comment",
  [sym_privacy_specifier] = "privacy_specifier",
  [sym__statement] = "_statement",
  [sym_block_statement] = "block_statement",
  [sym_expression_statement] = "expression_statement",
  [sym_assignment_statement] = "assignment_statement",
  [sym_statements] = "statements",
  [sym_variable_declaration_statement] = "variable_declaration_statement",
  [sym_variable_declaration_list] = "variable_declaration_list",
  [sym_variable_declaration] = "variable_declaration",
  [sym_type_part] = "type_part",
  [sym_initialization_part] = "initialization_part",
  [sym__expression] = "_expression",
  [sym_literal_expression] = "literal_expression",
  [sym_variable_expression] = "variable_expression",
  [sym_type_expression] = "type_expression",
  [sym_lvalue_expression] = "lvalue_expression",
  [sym_bool_literal] = "bool_literal",
  [sym_integer_literal] = "integer_literal",
  [sym_real_literal] = "real_literal",
  [sym__exponent_part] = "_exponent_part",
  [sym_string_literal] = "string_literal",
  [sym_interpreted_string_literal] = "interpreted_string_literal",
  [sym__double_quote_delimited_characters] = "_double_quote_delimited_characters",
  [sym__single_quote_delimited_characters] = "_single_quote_delimited_characters",
  [sym__string_character] = "_string_character",
  [sym_primitive_type] = "primitive_type",
  [sym_variable_kind] = "variable_kind",
  [sym_return_statement] = "return_statement",
  [sym_identifier_list] = "identifier_list",
  [sym__digits] = "_digits",
  [sym__separator_digits] = "_separator_digits",
  [sym__sign] = "_sign",
  [sym_assignment_operator] = "assignment_operator",
  [aux_sym_source_file_repeat1] = "source_file_repeat1",
};

static const TSSymbol ts_symbol_map[] = {
  [ts_builtin_sym_end] = ts_builtin_sym_end,
  [sym_inline_comment] = sym_inline_comment,
  [sym_block_comment] = sym_block_comment,
  [anon_sym_LPAREN] = anon_sym_LPAREN,
  [anon_sym_RPAREN] = anon_sym_RPAREN,
  [anon_sym_private] = anon_sym_private,
  [anon_sym_public] = anon_sym_public,
  [anon_sym_LBRACE] = anon_sym_LBRACE,
  [anon_sym_RBRACE] = anon_sym_RBRACE,
  [anon_sym_SEMI] = anon_sym_SEMI,
  [anon_sym_COMMA] = anon_sym_COMMA,
  [anon_sym_COLON] = anon_sym_COLON,
  [anon_sym_EQ] = anon_sym_EQ,
  [anon_sym_true] = anon_sym_true,
  [anon_sym_false] = anon_sym_false,
  [anon_sym_DOT] = anon_sym_DOT,
  [anon_sym_e] = anon_sym_e,
  [anon_sym_E] = anon_sym_E,
  [anon_sym_DQUOTE] = anon_sym_DQUOTE,
  [anon_sym_SQUOTE] = anon_sym_SQUOTE,
  [aux_sym__string_character_token1] = aux_sym__string_character_token1,
  [anon_sym_void] = anon_sym_void,
  [anon_sym_nothing] = anon_sym_nothing,
  [anon_sym_bool] = anon_sym_bool,
  [anon_sym_int] = anon_sym_int,
  [anon_sym_uint] = anon_sym_uint,
  [anon_sym_real] = anon_sym_real,
  [anon_sym_imag] = anon_sym_imag,
  [anon_sym_complex] = anon_sym_complex,
  [anon_sym_string] = anon_sym_string,
  [anon_sym_bytes] = anon_sym_bytes,
  [anon_sym_enum] = anon_sym_enum,
  [anon_sym_record] = anon_sym_record,
  [anon_sym_class] = anon_sym_class,
  [anon_sym_owned] = anon_sym_owned,
  [anon_sym_shared] = anon_sym_shared,
  [anon_sym_unmanaged] = anon_sym_unmanaged,
  [anon_sym_borrowed] = anon_sym_borrowed,
  [anon_sym_param] = anon_sym_param,
  [anon_sym_const] = anon_sym_const,
  [anon_sym_var] = anon_sym_var,
  [anon_sym_ref] = anon_sym_ref,
  [anon_sym_constref] = anon_sym_constref,
  [anon_sym_return] = anon_sym_return,
  [sym_identifier] = sym_identifier,
  [anon_sym_DOLLAR] = anon_sym_DOLLAR,
  [anon_sym__] = anon_sym__,
  [sym__letter] = sym__letter,
  [sym__digit] = sym__digit,
  [sym__binary_digit] = sym__binary_digit,
  [anon_sym_PLUS] = anon_sym_PLUS,
  [anon_sym_DASH] = anon_sym_DASH,
  [anon_sym_PLUS_EQ] = anon_sym_PLUS_EQ,
  [anon_sym_DASH_EQ] = anon_sym_DASH_EQ,
  [anon_sym_STAR_EQ] = anon_sym_STAR_EQ,
  [anon_sym_SLASH_EQ] = anon_sym_SLASH_EQ,
  [anon_sym_PERCENT_EQ] = anon_sym_PERCENT_EQ,
  [anon_sym_STAR_STAR_EQ] = anon_sym_STAR_STAR_EQ,
  [anon_sym_AMP_EQ] = anon_sym_AMP_EQ,
  [anon_sym_PIPE_EQ] = anon_sym_PIPE_EQ,
  [anon_sym_CARET_EQ] = anon_sym_CARET_EQ,
  [anon_sym_AMP_AMP_EQ] = anon_sym_AMP_AMP_EQ,
  [anon_sym_PIPE_PIPE_EQ] = anon_sym_PIPE_PIPE_EQ,
  [anon_sym_LT_LT_EQ] = anon_sym_LT_LT_EQ,
  [anon_sym_GT_GT_EQ] = anon_sym_GT_GT_EQ,
  [sym_source_file] = sym_source_file,
  [sym__comment] = sym__comment,
  [sym_privacy_specifier] = sym_privacy_specifier,
  [sym__statement] = sym__statement,
  [sym_block_statement] = sym_block_statement,
  [sym_expression_statement] = sym_expression_statement,
  [sym_assignment_statement] = sym_assignment_statement,
  [sym_statements] = sym_statements,
  [sym_variable_declaration_statement] = sym_variable_declaration_statement,
  [sym_variable_declaration_list] = sym_variable_declaration_list,
  [sym_variable_declaration] = sym_variable_declaration,
  [sym_type_part] = sym_type_part,
  [sym_initialization_part] = sym_initialization_part,
  [sym__expression] = sym__expression,
  [sym_literal_expression] = sym_literal_expression,
  [sym_variable_expression] = sym_variable_expression,
  [sym_type_expression] = sym_type_expression,
  [sym_lvalue_expression] = sym_lvalue_expression,
  [sym_bool_literal] = sym_bool_literal,
  [sym_integer_literal] = sym_integer_literal,
  [sym_real_literal] = sym_real_literal,
  [sym__exponent_part] = sym__exponent_part,
  [sym_string_literal] = sym_string_literal,
  [sym_interpreted_string_literal] = sym_interpreted_string_literal,
  [sym__double_quote_delimited_characters] = sym__double_quote_delimited_characters,
  [sym__single_quote_delimited_characters] = sym__single_quote_delimited_characters,
  [sym__string_character] = sym__string_character,
  [sym_primitive_type] = sym_primitive_type,
  [sym_variable_kind] = sym_variable_kind,
  [sym_return_statement] = sym_return_statement,
  [sym_identifier_list] = sym_identifier_list,
  [sym__digits] = sym__digits,
  [sym__separator_digits] = sym__separator_digits,
  [sym__sign] = sym__sign,
  [sym_assignment_operator] = sym_assignment_operator,
  [aux_sym_source_file_repeat1] = aux_sym_source_file_repeat1,
};

static const TSSymbolMetadata ts_symbol_metadata[] = {
  [ts_builtin_sym_end] = {
    .visible = false,
    .named = true,
  },
  [sym_inline_comment] = {
    .visible = true,
    .named = true,
  },
  [sym_block_comment] = {
    .visible = true,
    .named = true,
  },
  [anon_sym_LPAREN] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_RPAREN] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_private] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_public] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_LBRACE] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_RBRACE] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_SEMI] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_COMMA] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_COLON] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_true] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_false] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DOT] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_e] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_E] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DQUOTE] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_SQUOTE] = {
    .visible = true,
    .named = false,
  },
  [aux_sym__string_character_token1] = {
    .visible = false,
    .named = false,
  },
  [anon_sym_void] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_nothing] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_bool] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_int] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_uint] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_real] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_imag] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_complex] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_string] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_bytes] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_enum] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_record] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_class] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_owned] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_shared] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_unmanaged] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_borrowed] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_param] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_const] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_var] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_ref] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_constref] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_return] = {
    .visible = true,
    .named = false,
  },
  [sym_identifier] = {
    .visible = true,
    .named = true,
  },
  [anon_sym_DOLLAR] = {
    .visible = true,
    .named = false,
  },
  [anon_sym__] = {
    .visible = true,
    .named = false,
  },
  [sym__letter] = {
    .visible = false,
    .named = true,
  },
  [sym__digit] = {
    .visible = false,
    .named = true,
  },
  [sym__binary_digit] = {
    .visible = false,
    .named = true,
  },
  [anon_sym_PLUS] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DASH] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_PLUS_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_DASH_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_STAR_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_SLASH_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_PERCENT_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_STAR_STAR_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_AMP_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_PIPE_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_CARET_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_AMP_AMP_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_PIPE_PIPE_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_LT_LT_EQ] = {
    .visible = true,
    .named = false,
  },
  [anon_sym_GT_GT_EQ] = {
    .visible = true,
    .named = false,
  },
  [sym_source_file] = {
    .visible = true,
    .named = true,
  },
  [sym__comment] = {
    .visible = false,
    .named = true,
  },
  [sym_privacy_specifier] = {
    .visible = true,
    .named = true,
  },
  [sym__statement] = {
    .visible = false,
    .named = true,
  },
  [sym_block_statement] = {
    .visible = true,
    .named = true,
  },
  [sym_expression_statement] = {
    .visible = true,
    .named = true,
  },
  [sym_assignment_statement] = {
    .visible = true,
    .named = true,
  },
  [sym_statements] = {
    .visible = true,
    .named = true,
  },
  [sym_variable_declaration_statement] = {
    .visible = true,
    .named = true,
  },
  [sym_variable_declaration_list] = {
    .visible = true,
    .named = true,
  },
  [sym_variable_declaration] = {
    .visible = true,
    .named = true,
  },
  [sym_type_part] = {
    .visible = true,
    .named = true,
  },
  [sym_initialization_part] = {
    .visible = true,
    .named = true,
  },
  [sym__expression] = {
    .visible = false,
    .named = true,
  },
  [sym_literal_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_variable_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_type_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_lvalue_expression] = {
    .visible = true,
    .named = true,
  },
  [sym_bool_literal] = {
    .visible = true,
    .named = true,
  },
  [sym_integer_literal] = {
    .visible = true,
    .named = true,
  },
  [sym_real_literal] = {
    .visible = true,
    .named = true,
  },
  [sym__exponent_part] = {
    .visible = false,
    .named = true,
  },
  [sym_string_literal] = {
    .visible = true,
    .named = true,
  },
  [sym_interpreted_string_literal] = {
    .visible = true,
    .named = true,
  },
  [sym__double_quote_delimited_characters] = {
    .visible = false,
    .named = true,
  },
  [sym__single_quote_delimited_characters] = {
    .visible = false,
    .named = true,
  },
  [sym__string_character] = {
    .visible = false,
    .named = true,
  },
  [sym_primitive_type] = {
    .visible = true,
    .named = true,
  },
  [sym_variable_kind] = {
    .visible = true,
    .named = true,
  },
  [sym_return_statement] = {
    .visible = true,
    .named = true,
  },
  [sym_identifier_list] = {
    .visible = true,
    .named = true,
  },
  [sym__digits] = {
    .visible = false,
    .named = true,
  },
  [sym__separator_digits] = {
    .visible = false,
    .named = true,
  },
  [sym__sign] = {
    .visible = false,
    .named = true,
  },
  [sym_assignment_operator] = {
    .visible = true,
    .named = true,
  },
  [aux_sym_source_file_repeat1] = {
    .visible = false,
    .named = false,
  },
};

static const TSSymbol ts_alias_sequences[PRODUCTION_ID_COUNT][MAX_ALIAS_SEQUENCE_LENGTH] = {
  [0] = {0},
};

static const uint16_t ts_non_terminal_alias_map[] = {
  0,
};

static const TSStateId ts_primary_state_ids[STATE_COUNT] = {
  [0] = 0,
  [1] = 1,
  [2] = 2,
  [3] = 3,
  [4] = 4,
  [5] = 5,
  [6] = 6,
  [7] = 7,
  [8] = 8,
  [9] = 9,
  [10] = 10,
  [11] = 11,
  [12] = 12,
  [13] = 13,
  [14] = 14,
  [15] = 15,
  [16] = 16,
  [17] = 17,
  [18] = 18,
  [19] = 19,
  [20] = 20,
  [21] = 21,
  [22] = 22,
  [23] = 23,
  [24] = 24,
  [25] = 25,
  [26] = 26,
  [27] = 27,
  [28] = 28,
  [29] = 29,
  [30] = 30,
  [31] = 31,
  [32] = 32,
  [33] = 33,
  [34] = 34,
  [35] = 35,
  [36] = 36,
  [37] = 37,
  [38] = 38,
  [39] = 39,
  [40] = 40,
  [41] = 41,
  [42] = 42,
  [43] = 43,
  [44] = 44,
  [45] = 45,
  [46] = 46,
  [47] = 47,
  [48] = 48,
  [49] = 49,
  [50] = 50,
  [51] = 51,
  [52] = 52,
  [53] = 53,
  [54] = 54,
  [55] = 55,
  [56] = 56,
  [57] = 57,
  [58] = 58,
  [59] = 59,
  [60] = 60,
  [61] = 61,
  [62] = 62,
  [63] = 63,
  [64] = 64,
  [65] = 65,
  [66] = 66,
  [67] = 67,
  [68] = 68,
  [69] = 69,
  [70] = 70,
  [71] = 71,
  [72] = 72,
  [73] = 73,
  [74] = 74,
  [75] = 75,
};

static bool ts_lex(TSLexer *lexer, TSStateId state) {
  START_LEXER();
  eof = lexer->eof(lexer);
  switch (state) {
    case 0:
      if (eof) ADVANCE(41);
      if (lookahead == '"') ADVANCE(60);
      if (lookahead == '$') ADVANCE(188);
      if (lookahead == '%') ADVANCE(11);
      if (lookahead == '&') ADVANCE(4);
      if (lookahead == '\'') ADVANCE(61);
      if (lookahead == '(') ADVANCE(45);
      if (lookahead == ')') ADVANCE(46);
      if (lookahead == '*') ADVANCE(9);
      if (lookahead == '+') ADVANCE(193);
      if (lookahead == ',') ADVANCE(52);
      if (lookahead == '-') ADVANCE(195);
      if (lookahead == '.') ADVANCE(57);
      if (lookahead == '/') ADVANCE(6);
      if (lookahead == ':') ADVANCE(53);
      if (lookahead == ';') ADVANCE(51);
      if (lookahead == '<') ADVANCE(10);
      if (lookahead == '=') ADVANCE(54);
      if (lookahead == '>') ADVANCE(22);
      if (lookahead == 'E') ADVANCE(59);
      if (lookahead == '^') ADVANCE(15);
      if (lookahead == '_') ADVANCE(189);
      if (lookahead == 'e') ADVANCE(58);
      if (lookahead == '{') ADVANCE(49);
      if (lookahead == '|') ADVANCE(16);
      if (lookahead == '}') ADVANCE(50);
      if (lookahead == '0' ||
          lookahead == '1') ADVANCE(191);
      if (lookahead == '\t' ||
          lookahead == '\n' ||
          lookahead == '\r' ||
          lookahead == ' ') SKIP(0)
      if (('2' <= lookahead && lookahead <= '9')) ADVANCE(191);
      if (('A' <= lookahead && lookahead <= 'Z') ||
          ('a' <= lookahead && lookahead <= 'z')) ADVANCE(190);
      END_STATE();
    case 1:
      if (lookahead == '\n') SKIP(1)
      if (lookahead == '"') ADVANCE(60);
      if (lookahead == '\'') ADVANCE(61);
      if (lookahead == '\t' ||
          lookahead == '\r' ||
          lookahead == ' ') ADVANCE(63);
      if (lookahead != 0) ADVANCE(62);
      END_STATE();
    case 2:
      if (lookahead == '"') ADVANCE(60);
      if (lookahead == '\'') ADVANCE(61);
      if (lookahead == '.') ADVANCE(57);
      if (lookahead == ';') ADVANCE(51);
      if (lookahead == 'b') ADVANCE(154);
      if (lookahead == 'c') ADVANCE(138);
      if (lookahead == 'e') ADVANCE(147);
      if (lookahead == 'f') ADVANCE(96);
      if (lookahead == 'i') ADVANCE(143);
      if (lookahead == 'n') ADVANCE(158);
      if (lookahead == 'o') ADVANCE(184);
      if (lookahead == 'r') ADVANCE(112);
      if (lookahead == 's') ADVANCE(126);
      if (lookahead == 't') ADVANCE(164);
      if (lookahead == 'u') ADVANCE(131);
      if (lookahead == 'v') ADVANCE(156);
      if (lookahead == '\t' ||
          lookahead == '\n' ||
          lookahead == '\r' ||
          lookahead == ' ') SKIP(2)
      if (('0' <= lookahead && lookahead <= '9')) ADVANCE(191);
      if (('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 3:
      if (lookahead == '%') ADVANCE(11);
      if (lookahead == '&') ADVANCE(4);
      if (lookahead == '*') ADVANCE(9);
      if (lookahead == '+') ADVANCE(12);
      if (lookahead == ',') ADVANCE(52);
      if (lookahead == '-') ADVANCE(13);
      if (lookahead == '.') ADVANCE(57);
      if (lookahead == '/') ADVANCE(14);
      if (lookahead == ';') ADVANCE(51);
      if (lookahead == '<') ADVANCE(10);
      if (lookahead == '=') ADVANCE(54);
      if (lookahead == '>') ADVANCE(22);
      if (lookahead == 'E') ADVANCE(59);
      if (lookahead == '^') ADVANCE(15);
      if (lookahead == '_') ADVANCE(189);
      if (lookahead == 'c') ADVANCE(32);
      if (lookahead == 'e') ADVANCE(58);
      if (lookahead == 'p') ADVANCE(24);
      if (lookahead == 'r') ADVANCE(27);
      if (lookahead == 'v') ADVANCE(25);
      if (lookahead == '|') ADVANCE(16);
      if (lookahead == '\t' ||
          lookahead == '\n' ||
          lookahead == '\r' ||
          lookahead == ' ') SKIP(3)
      if (('0' <= lookahead && lookahead <= '9')) ADVANCE(191);
      END_STATE();
    case 4:
      if (lookahead == '&') ADVANCE(17);
      if (lookahead == '=') ADVANCE(202);
      END_STATE();
    case 5:
      if (lookahead == '*') ADVANCE(8);
      if (lookahead == '/') ADVANCE(42);
      END_STATE();
    case 6:
      if (lookahead == '*') ADVANCE(8);
      if (lookahead == '/') ADVANCE(42);
      if (lookahead == '=') ADVANCE(199);
      END_STATE();
    case 7:
      if (lookahead == '*') ADVANCE(7);
      if (lookahead == '/') ADVANCE(44);
      if (lookahead != 0) ADVANCE(8);
      END_STATE();
    case 8:
      if (lookahead == '*') ADVANCE(7);
      if (lookahead != 0) ADVANCE(8);
      END_STATE();
    case 9:
      if (lookahead == '*') ADVANCE(18);
      if (lookahead == '=') ADVANCE(198);
      END_STATE();
    case 10:
      if (lookahead == '<') ADVANCE(19);
      END_STATE();
    case 11:
      if (lookahead == '=') ADVANCE(200);
      END_STATE();
    case 12:
      if (lookahead == '=') ADVANCE(196);
      END_STATE();
    case 13:
      if (lookahead == '=') ADVANCE(197);
      END_STATE();
    case 14:
      if (lookahead == '=') ADVANCE(199);
      END_STATE();
    case 15:
      if (lookahead == '=') ADVANCE(204);
      END_STATE();
    case 16:
      if (lookahead == '=') ADVANCE(203);
      if (lookahead == '|') ADVANCE(21);
      END_STATE();
    case 17:
      if (lookahead == '=') ADVANCE(205);
      END_STATE();
    case 18:
      if (lookahead == '=') ADVANCE(201);
      END_STATE();
    case 19:
      if (lookahead == '=') ADVANCE(207);
      END_STATE();
    case 20:
      if (lookahead == '=') ADVANCE(208);
      END_STATE();
    case 21:
      if (lookahead == '=') ADVANCE(206);
      END_STATE();
    case 22:
      if (lookahead == '>') ADVANCE(20);
      END_STATE();
    case 23:
      if (lookahead == 'a') ADVANCE(30);
      END_STATE();
    case 24:
      if (lookahead == 'a') ADVANCE(35);
      END_STATE();
    case 25:
      if (lookahead == 'a') ADVANCE(34);
      END_STATE();
    case 26:
      if (lookahead == 'e') ADVANCE(28);
      END_STATE();
    case 27:
      if (lookahead == 'e') ADVANCE(29);
      END_STATE();
    case 28:
      if (lookahead == 'f') ADVANCE(89);
      END_STATE();
    case 29:
      if (lookahead == 'f') ADVANCE(87);
      END_STATE();
    case 30:
      if (lookahead == 'm') ADVANCE(81);
      END_STATE();
    case 31:
      if (lookahead == 'n') ADVANCE(36);
      END_STATE();
    case 32:
      if (lookahead == 'o') ADVANCE(31);
      END_STATE();
    case 33:
      if (lookahead == 'r') ADVANCE(26);
      END_STATE();
    case 34:
      if (lookahead == 'r') ADVANCE(85);
      END_STATE();
    case 35:
      if (lookahead == 'r') ADVANCE(23);
      END_STATE();
    case 36:
      if (lookahead == 's') ADVANCE(37);
      END_STATE();
    case 37:
      if (lookahead == 't') ADVANCE(83);
      END_STATE();
    case 38:
      if (lookahead == '\t' ||
          lookahead == '\n' ||
          lookahead == '\r' ||
          lookahead == ' ') SKIP(38)
      if (('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 39:
      if (lookahead != 0 &&
          lookahead != '\r') ADVANCE(42);
      if (lookahead == '\r') ADVANCE(43);
      END_STATE();
    case 40:
      if (eof) ADVANCE(41);
      if (lookahead == '+') ADVANCE(192);
      if (lookahead == '-') ADVANCE(194);
      if (lookahead == '/') ADVANCE(5);
      if (lookahead == 'c') ADVANCE(153);
      if (lookahead == 'p') ADVANCE(91);
      if (lookahead == 'r') ADVANCE(110);
      if (lookahead == 'v') ADVANCE(94);
      if (lookahead == '{') ADVANCE(49);
      if (lookahead == '}') ADVANCE(50);
      if (lookahead == '\t' ||
          lookahead == '\n' ||
          lookahead == '\r' ||
          lookahead == ' ') SKIP(40)
      if (('0' <= lookahead && lookahead <= '9')) ADVANCE(191);
      if (('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 41:
      ACCEPT_TOKEN(ts_builtin_sym_end);
      END_STATE();
    case 42:
      ACCEPT_TOKEN(sym_inline_comment);
      if (lookahead == '\\') ADVANCE(39);
      if (lookahead != 0 &&
          lookahead != '\n') ADVANCE(42);
      END_STATE();
    case 43:
      ACCEPT_TOKEN(sym_inline_comment);
      if (lookahead != 0 &&
          lookahead != '\\') ADVANCE(42);
      if (lookahead == '\\') ADVANCE(39);
      END_STATE();
    case 44:
      ACCEPT_TOKEN(sym_block_comment);
      END_STATE();
    case 45:
      ACCEPT_TOKEN(anon_sym_LPAREN);
      END_STATE();
    case 46:
      ACCEPT_TOKEN(anon_sym_RPAREN);
      END_STATE();
    case 47:
      ACCEPT_TOKEN(anon_sym_private);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 48:
      ACCEPT_TOKEN(anon_sym_public);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 49:
      ACCEPT_TOKEN(anon_sym_LBRACE);
      END_STATE();
    case 50:
      ACCEPT_TOKEN(anon_sym_RBRACE);
      END_STATE();
    case 51:
      ACCEPT_TOKEN(anon_sym_SEMI);
      END_STATE();
    case 52:
      ACCEPT_TOKEN(anon_sym_COMMA);
      END_STATE();
    case 53:
      ACCEPT_TOKEN(anon_sym_COLON);
      END_STATE();
    case 54:
      ACCEPT_TOKEN(anon_sym_EQ);
      END_STATE();
    case 55:
      ACCEPT_TOKEN(anon_sym_true);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 56:
      ACCEPT_TOKEN(anon_sym_false);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 57:
      ACCEPT_TOKEN(anon_sym_DOT);
      END_STATE();
    case 58:
      ACCEPT_TOKEN(anon_sym_e);
      END_STATE();
    case 59:
      ACCEPT_TOKEN(anon_sym_E);
      END_STATE();
    case 60:
      ACCEPT_TOKEN(anon_sym_DQUOTE);
      END_STATE();
    case 61:
      ACCEPT_TOKEN(anon_sym_SQUOTE);
      END_STATE();
    case 62:
      ACCEPT_TOKEN(aux_sym__string_character_token1);
      END_STATE();
    case 63:
      ACCEPT_TOKEN(aux_sym__string_character_token1);
      if (lookahead == '\t' ||
          lookahead == '\r' ||
          lookahead == ' ') ADVANCE(63);
      if (lookahead != 0 &&
          lookahead != '\n' &&
          lookahead != '"' &&
          lookahead != '\'') ADVANCE(62);
      END_STATE();
    case 64:
      ACCEPT_TOKEN(anon_sym_void);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 65:
      ACCEPT_TOKEN(anon_sym_nothing);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 66:
      ACCEPT_TOKEN(anon_sym_bool);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 67:
      ACCEPT_TOKEN(anon_sym_int);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 68:
      ACCEPT_TOKEN(anon_sym_uint);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 69:
      ACCEPT_TOKEN(anon_sym_real);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 70:
      ACCEPT_TOKEN(anon_sym_imag);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 71:
      ACCEPT_TOKEN(anon_sym_complex);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 72:
      ACCEPT_TOKEN(anon_sym_string);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 73:
      ACCEPT_TOKEN(anon_sym_bytes);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 74:
      ACCEPT_TOKEN(anon_sym_enum);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 75:
      ACCEPT_TOKEN(anon_sym_record);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 76:
      ACCEPT_TOKEN(anon_sym_class);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 77:
      ACCEPT_TOKEN(anon_sym_owned);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 78:
      ACCEPT_TOKEN(anon_sym_shared);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 79:
      ACCEPT_TOKEN(anon_sym_unmanaged);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 80:
      ACCEPT_TOKEN(anon_sym_borrowed);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 81:
      ACCEPT_TOKEN(anon_sym_param);
      END_STATE();
    case 82:
      ACCEPT_TOKEN(anon_sym_param);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 83:
      ACCEPT_TOKEN(anon_sym_const);
      if (lookahead == ' ') ADVANCE(33);
      END_STATE();
    case 84:
      ACCEPT_TOKEN(anon_sym_const);
      if (lookahead == ' ') ADVANCE(33);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 85:
      ACCEPT_TOKEN(anon_sym_var);
      END_STATE();
    case 86:
      ACCEPT_TOKEN(anon_sym_var);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 87:
      ACCEPT_TOKEN(anon_sym_ref);
      END_STATE();
    case 88:
      ACCEPT_TOKEN(anon_sym_ref);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 89:
      ACCEPT_TOKEN(anon_sym_constref);
      END_STATE();
    case 90:
      ACCEPT_TOKEN(anon_sym_return);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 91:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(162);
      if (lookahead == 'r') ADVANCE(128);
      if (lookahead == 'u') ADVANCE(102);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 92:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(140);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 93:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(122);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 94:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(161);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 95:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(172);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 96:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(137);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 97:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(178);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 98:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(135);
      if (lookahead == 'c') ADVANCE(159);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 99:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(125);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 100:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(152);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 101:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'a') ADVANCE(168);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 102:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'b') ADVANCE(136);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 103:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'c') ADVANCE(48);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 104:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(64);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 105:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(77);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 106:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(75);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 107:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(78);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 108:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(80);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 109:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'd') ADVANCE(79);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 110:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(121);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 111:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(47);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 112:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(98);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 113:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(55);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 114:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(56);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 115:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(186);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 116:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(105);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 117:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(170);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 118:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(107);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 119:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(108);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 120:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'e') ADVANCE(109);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 121:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'f') ADVANCE(88);
      if (lookahead == 't') ADVANCE(180);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 122:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'g') ADVANCE(70);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 123:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'g') ADVANCE(72);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 124:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'g') ADVANCE(65);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 125:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'g') ADVANCE(120);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 126:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'h') ADVANCE(101);
      if (lookahead == 't') ADVANCE(166);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 127:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'h') ADVANCE(133);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 128:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(183);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 129:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(103);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 130:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(104);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 131:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(151);
      if (lookahead == 'n') ADVANCE(144);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 132:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(148);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 133:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'i') ADVANCE(149);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 134:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(66);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 135:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(69);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 136:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(129);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 137:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(173);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 138:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(95);
      if (lookahead == 'o') ADVANCE(141);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 139:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'l') ADVANCE(115);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 140:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'm') ADVANCE(82);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 141:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'm') ADVANCE(160);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 142:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'm') ADVANCE(74);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 143:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'm') ADVANCE(93);
      if (lookahead == 'n') ADVANCE(175);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 144:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'm') ADVANCE(100);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 145:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(169);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 146:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(90);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 147:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(181);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 148:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(123);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 149:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(124);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 150:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(116);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 151:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(177);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 152:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'n') ADVANCE(99);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 153:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(145);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 154:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(157);
      if (lookahead == 'y') ADVANCE(179);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 155:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(185);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 156:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(130);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 157:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(134);
      if (lookahead == 'r') ADVANCE(167);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 158:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(176);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 159:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'o') ADVANCE(165);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 160:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'p') ADVANCE(139);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 161:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(86);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 162:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(92);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 163:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(146);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 164:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(182);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 165:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(106);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 166:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(132);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 167:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(155);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 168:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'r') ADVANCE(118);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 169:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 's') ADVANCE(174);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 170:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 's') ADVANCE(73);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 171:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 's') ADVANCE(76);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 172:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 's') ADVANCE(171);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 173:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 's') ADVANCE(114);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 174:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(84);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 175:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(67);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 176:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(127);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 177:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(68);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 178:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(111);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 179:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 't') ADVANCE(117);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 180:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'u') ADVANCE(163);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 181:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'u') ADVANCE(142);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 182:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'u') ADVANCE(113);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 183:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'v') ADVANCE(97);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 184:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'w') ADVANCE(150);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 185:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'w') ADVANCE(119);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 186:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == 'x') ADVANCE(71);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 187:
      ACCEPT_TOKEN(sym_identifier);
      if (lookahead == '$' ||
          ('0' <= lookahead && lookahead <= '9') ||
          ('A' <= lookahead && lookahead <= 'z')) ADVANCE(187);
      END_STATE();
    case 188:
      ACCEPT_TOKEN(anon_sym_DOLLAR);
      END_STATE();
    case 189:
      ACCEPT_TOKEN(anon_sym__);
      END_STATE();
    case 190:
      ACCEPT_TOKEN(sym__letter);
      END_STATE();
    case 191:
      ACCEPT_TOKEN(sym__digit);
      END_STATE();
    case 192:
      ACCEPT_TOKEN(anon_sym_PLUS);
      END_STATE();
    case 193:
      ACCEPT_TOKEN(anon_sym_PLUS);
      if (lookahead == '=') ADVANCE(196);
      END_STATE();
    case 194:
      ACCEPT_TOKEN(anon_sym_DASH);
      END_STATE();
    case 195:
      ACCEPT_TOKEN(anon_sym_DASH);
      if (lookahead == '=') ADVANCE(197);
      END_STATE();
    case 196:
      ACCEPT_TOKEN(anon_sym_PLUS_EQ);
      END_STATE();
    case 197:
      ACCEPT_TOKEN(anon_sym_DASH_EQ);
      END_STATE();
    case 198:
      ACCEPT_TOKEN(anon_sym_STAR_EQ);
      END_STATE();
    case 199:
      ACCEPT_TOKEN(anon_sym_SLASH_EQ);
      END_STATE();
    case 200:
      ACCEPT_TOKEN(anon_sym_PERCENT_EQ);
      END_STATE();
    case 201:
      ACCEPT_TOKEN(anon_sym_STAR_STAR_EQ);
      END_STATE();
    case 202:
      ACCEPT_TOKEN(anon_sym_AMP_EQ);
      END_STATE();
    case 203:
      ACCEPT_TOKEN(anon_sym_PIPE_EQ);
      END_STATE();
    case 204:
      ACCEPT_TOKEN(anon_sym_CARET_EQ);
      END_STATE();
    case 205:
      ACCEPT_TOKEN(anon_sym_AMP_AMP_EQ);
      END_STATE();
    case 206:
      ACCEPT_TOKEN(anon_sym_PIPE_PIPE_EQ);
      END_STATE();
    case 207:
      ACCEPT_TOKEN(anon_sym_LT_LT_EQ);
      END_STATE();
    case 208:
      ACCEPT_TOKEN(anon_sym_GT_GT_EQ);
      END_STATE();
    default:
      return false;
  }
}

static const TSLexMode ts_lex_modes[STATE_COUNT] = {
  [0] = {.lex_state = 0},
  [1] = {.lex_state = 40},
  [2] = {.lex_state = 2},
  [3] = {.lex_state = 2},
  [4] = {.lex_state = 2},
  [5] = {.lex_state = 2},
  [6] = {.lex_state = 40},
  [7] = {.lex_state = 40},
  [8] = {.lex_state = 2},
  [9] = {.lex_state = 40},
  [10] = {.lex_state = 40},
  [11] = {.lex_state = 3},
  [12] = {.lex_state = 3},
  [13] = {.lex_state = 3},
  [14] = {.lex_state = 40},
  [15] = {.lex_state = 40},
  [16] = {.lex_state = 40},
  [17] = {.lex_state = 40},
  [18] = {.lex_state = 40},
  [19] = {.lex_state = 40},
  [20] = {.lex_state = 40},
  [21] = {.lex_state = 3},
  [22] = {.lex_state = 3},
  [23] = {.lex_state = 3},
  [24] = {.lex_state = 3},
  [25] = {.lex_state = 3},
  [26] = {.lex_state = 3},
  [27] = {.lex_state = 0},
  [28] = {.lex_state = 3},
  [29] = {.lex_state = 3},
  [30] = {.lex_state = 1},
  [31] = {.lex_state = 1},
  [32] = {.lex_state = 40},
  [33] = {.lex_state = 3},
  [34] = {.lex_state = 1},
  [35] = {.lex_state = 1},
  [36] = {.lex_state = 3},
  [37] = {.lex_state = 0},
  [38] = {.lex_state = 0},
  [39] = {.lex_state = 38},
  [40] = {.lex_state = 0},
  [41] = {.lex_state = 38},
  [42] = {.lex_state = 38},
  [43] = {.lex_state = 0},
  [44] = {.lex_state = 0},
  [45] = {.lex_state = 0},
  [46] = {.lex_state = 0},
  [47] = {.lex_state = 0},
  [48] = {.lex_state = 0},
  [49] = {.lex_state = 0},
  [50] = {.lex_state = 0},
  [51] = {.lex_state = 0},
  [52] = {.lex_state = 0},
  [53] = {.lex_state = 0},
  [54] = {.lex_state = 0},
  [55] = {.lex_state = 0},
  [56] = {.lex_state = 0},
  [57] = {.lex_state = 40},
  [58] = {.lex_state = 0},
  [59] = {.lex_state = 40},
  [60] = {.lex_state = 0},
  [61] = {.lex_state = 0},
  [62] = {.lex_state = 0},
  [63] = {.lex_state = 0},
  [64] = {.lex_state = 0},
  [65] = {.lex_state = 0},
  [66] = {.lex_state = 0},
  [67] = {.lex_state = 0},
  [68] = {.lex_state = 0},
  [69] = {.lex_state = 0},
  [70] = {.lex_state = 0},
  [71] = {.lex_state = 38},
  [72] = {.lex_state = 0},
  [73] = {.lex_state = 0},
  [74] = {.lex_state = 0},
  [75] = {.lex_state = 0},
};

static const uint16_t ts_parse_table[LARGE_STATE_COUNT][SYMBOL_COUNT] = {
  [0] = {
    [ts_builtin_sym_end] = ACTIONS(1),
    [sym_inline_comment] = ACTIONS(1),
    [sym_block_comment] = ACTIONS(1),
    [anon_sym_LPAREN] = ACTIONS(1),
    [anon_sym_RPAREN] = ACTIONS(1),
    [anon_sym_LBRACE] = ACTIONS(1),
    [anon_sym_RBRACE] = ACTIONS(1),
    [anon_sym_SEMI] = ACTIONS(1),
    [anon_sym_COMMA] = ACTIONS(1),
    [anon_sym_COLON] = ACTIONS(1),
    [anon_sym_EQ] = ACTIONS(1),
    [anon_sym_DOT] = ACTIONS(1),
    [anon_sym_e] = ACTIONS(1),
    [anon_sym_E] = ACTIONS(1),
    [anon_sym_DQUOTE] = ACTIONS(1),
    [anon_sym_SQUOTE] = ACTIONS(1),
    [anon_sym_DOLLAR] = ACTIONS(1),
    [anon_sym__] = ACTIONS(1),
    [sym__letter] = ACTIONS(1),
    [sym__digit] = ACTIONS(1),
    [sym__binary_digit] = ACTIONS(1),
    [anon_sym_PLUS] = ACTIONS(1),
    [anon_sym_DASH] = ACTIONS(1),
    [anon_sym_PLUS_EQ] = ACTIONS(1),
    [anon_sym_DASH_EQ] = ACTIONS(1),
    [anon_sym_STAR_EQ] = ACTIONS(1),
    [anon_sym_SLASH_EQ] = ACTIONS(1),
    [anon_sym_PERCENT_EQ] = ACTIONS(1),
    [anon_sym_STAR_STAR_EQ] = ACTIONS(1),
    [anon_sym_AMP_EQ] = ACTIONS(1),
    [anon_sym_PIPE_EQ] = ACTIONS(1),
    [anon_sym_CARET_EQ] = ACTIONS(1),
    [anon_sym_AMP_AMP_EQ] = ACTIONS(1),
    [anon_sym_PIPE_PIPE_EQ] = ACTIONS(1),
    [anon_sym_LT_LT_EQ] = ACTIONS(1),
    [anon_sym_GT_GT_EQ] = ACTIONS(1),
  },
  [1] = {
    [sym_source_file] = STATE(70),
    [sym__comment] = STATE(7),
    [sym_privacy_specifier] = STATE(29),
    [sym__statement] = STATE(7),
    [sym_block_statement] = STATE(7),
    [sym_expression_statement] = STATE(7),
    [sym_assignment_statement] = STATE(7),
    [sym_variable_declaration_statement] = STATE(7),
    [sym_variable_expression] = STATE(12),
    [sym_lvalue_expression] = STATE(13),
    [sym_variable_kind] = STATE(39),
    [sym_return_statement] = STATE(7),
    [aux_sym_source_file_repeat1] = STATE(7),
    [ts_builtin_sym_end] = ACTIONS(3),
    [sym_inline_comment] = ACTIONS(5),
    [sym_block_comment] = ACTIONS(5),
    [anon_sym_private] = ACTIONS(7),
    [anon_sym_public] = ACTIONS(7),
    [anon_sym_LBRACE] = ACTIONS(9),
    [anon_sym_param] = ACTIONS(11),
    [anon_sym_const] = ACTIONS(11),
    [anon_sym_var] = ACTIONS(11),
    [anon_sym_ref] = ACTIONS(11),
    [anon_sym_constref] = ACTIONS(13),
    [anon_sym_return] = ACTIONS(15),
    [sym_identifier] = ACTIONS(17),
  },
};

static const uint16_t ts_small_parse_table[] = {
  [0] = 14,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(19), 1,
      anon_sym_SEMI,
    ACTIONS(23), 1,
      anon_sym_DOT,
    ACTIONS(25), 1,
      anon_sym_DQUOTE,
    ACTIONS(27), 1,
      anon_sym_SQUOTE,
    ACTIONS(31), 1,
      sym__digit,
    STATE(23), 1,
      sym__digits,
    STATE(45), 1,
      sym_variable_expression,
    STATE(54), 1,
      sym_primitive_type,
    STATE(55), 1,
      sym_interpreted_string_literal,
    ACTIONS(21), 2,
      anon_sym_true,
      anon_sym_false,
    STATE(44), 4,
      sym_bool_literal,
      sym_integer_literal,
      sym_real_literal,
      sym_string_literal,
    STATE(69), 4,
      sym__expression,
      sym_literal_expression,
      sym_type_expression,
      sym_lvalue_expression,
    ACTIONS(29), 17,
      anon_sym_void,
      anon_sym_nothing,
      anon_sym_bool,
      anon_sym_int,
      anon_sym_uint,
      anon_sym_real,
      anon_sym_imag,
      anon_sym_complex,
      anon_sym_string,
      anon_sym_bytes,
      anon_sym_enum,
      anon_sym_record,
      anon_sym_class,
      anon_sym_owned,
      anon_sym_shared,
      anon_sym_unmanaged,
      anon_sym_borrowed,
  [66] = 13,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(23), 1,
      anon_sym_DOT,
    ACTIONS(25), 1,
      anon_sym_DQUOTE,
    ACTIONS(27), 1,
      anon_sym_SQUOTE,
    ACTIONS(31), 1,
      sym__digit,
    STATE(23), 1,
      sym__digits,
    STATE(45), 1,
      sym_variable_expression,
    STATE(54), 1,
      sym_primitive_type,
    STATE(55), 1,
      sym_interpreted_string_literal,
    ACTIONS(21), 2,
      anon_sym_true,
      anon_sym_false,
    STATE(44), 4,
      sym_bool_literal,
      sym_integer_literal,
      sym_real_literal,
      sym_string_literal,
    STATE(60), 4,
      sym__expression,
      sym_literal_expression,
      sym_type_expression,
      sym_lvalue_expression,
    ACTIONS(29), 17,
      anon_sym_void,
      anon_sym_nothing,
      anon_sym_bool,
      anon_sym_int,
      anon_sym_uint,
      anon_sym_real,
      anon_sym_imag,
      anon_sym_complex,
      anon_sym_string,
      anon_sym_bytes,
      anon_sym_enum,
      anon_sym_record,
      anon_sym_class,
      anon_sym_owned,
      anon_sym_shared,
      anon_sym_unmanaged,
      anon_sym_borrowed,
  [129] = 13,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(23), 1,
      anon_sym_DOT,
    ACTIONS(25), 1,
      anon_sym_DQUOTE,
    ACTIONS(27), 1,
      anon_sym_SQUOTE,
    ACTIONS(31), 1,
      sym__digit,
    STATE(23), 1,
      sym__digits,
    STATE(45), 1,
      sym_variable_expression,
    STATE(54), 1,
      sym_primitive_type,
    STATE(55), 1,
      sym_interpreted_string_literal,
    ACTIONS(21), 2,
      anon_sym_true,
      anon_sym_false,
    STATE(44), 4,
      sym_bool_literal,
      sym_integer_literal,
      sym_real_literal,
      sym_string_literal,
    STATE(65), 4,
      sym__expression,
      sym_literal_expression,
      sym_type_expression,
      sym_lvalue_expression,
    ACTIONS(29), 17,
      anon_sym_void,
      anon_sym_nothing,
      anon_sym_bool,
      anon_sym_int,
      anon_sym_uint,
      anon_sym_real,
      anon_sym_imag,
      anon_sym_complex,
      anon_sym_string,
      anon_sym_bytes,
      anon_sym_enum,
      anon_sym_record,
      anon_sym_class,
      anon_sym_owned,
      anon_sym_shared,
      anon_sym_unmanaged,
      anon_sym_borrowed,
  [192] = 13,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(23), 1,
      anon_sym_DOT,
    ACTIONS(25), 1,
      anon_sym_DQUOTE,
    ACTIONS(27), 1,
      anon_sym_SQUOTE,
    ACTIONS(31), 1,
      sym__digit,
    STATE(23), 1,
      sym__digits,
    STATE(45), 1,
      sym_variable_expression,
    STATE(48), 1,
      sym_type_expression,
    STATE(55), 1,
      sym_interpreted_string_literal,
    ACTIONS(21), 2,
      anon_sym_true,
      anon_sym_false,
    STATE(44), 4,
      sym_bool_literal,
      sym_integer_literal,
      sym_real_literal,
      sym_string_literal,
    STATE(54), 4,
      sym__expression,
      sym_literal_expression,
      sym_lvalue_expression,
      sym_primitive_type,
    ACTIONS(29), 17,
      anon_sym_void,
      anon_sym_nothing,
      anon_sym_bool,
      anon_sym_int,
      anon_sym_uint,
      anon_sym_real,
      anon_sym_imag,
      anon_sym_complex,
      anon_sym_string,
      anon_sym_bytes,
      anon_sym_enum,
      anon_sym_record,
      anon_sym_class,
      anon_sym_owned,
      anon_sym_shared,
      anon_sym_unmanaged,
      anon_sym_borrowed,
  [255] = 13,
    ACTIONS(33), 1,
      ts_builtin_sym_end,
    ACTIONS(41), 1,
      anon_sym_LBRACE,
    ACTIONS(47), 1,
      anon_sym_constref,
    ACTIONS(50), 1,
      anon_sym_return,
    ACTIONS(53), 1,
      sym_identifier,
    STATE(12), 1,
      sym_variable_expression,
    STATE(13), 1,
      sym_lvalue_expression,
    STATE(29), 1,
      sym_privacy_specifier,
    STATE(39), 1,
      sym_variable_kind,
    ACTIONS(35), 2,
      sym_inline_comment,
      sym_block_comment,
    ACTIONS(38), 2,
      anon_sym_private,
      anon_sym_public,
    ACTIONS(44), 4,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
    STATE(6), 8,
      sym__comment,
      sym__statement,
      sym_block_statement,
      sym_expression_statement,
      sym_assignment_statement,
      sym_variable_declaration_statement,
      sym_return_statement,
      aux_sym_source_file_repeat1,
  [307] = 13,
    ACTIONS(9), 1,
      anon_sym_LBRACE,
    ACTIONS(13), 1,
      anon_sym_constref,
    ACTIONS(15), 1,
      anon_sym_return,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(56), 1,
      ts_builtin_sym_end,
    STATE(12), 1,
      sym_variable_expression,
    STATE(13), 1,
      sym_lvalue_expression,
    STATE(29), 1,
      sym_privacy_specifier,
    STATE(39), 1,
      sym_variable_kind,
    ACTIONS(7), 2,
      anon_sym_private,
      anon_sym_public,
    ACTIONS(58), 2,
      sym_inline_comment,
      sym_block_comment,
    ACTIONS(11), 4,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
    STATE(6), 8,
      sym__comment,
      sym__statement,
      sym_block_statement,
      sym_expression_statement,
      sym_assignment_statement,
      sym_variable_declaration_statement,
      sym_return_statement,
      aux_sym_source_file_repeat1,
  [359] = 2,
    ACTIONS(62), 4,
      anon_sym_DOT,
      anon_sym_DQUOTE,
      anon_sym_SQUOTE,
      sym__digit,
    ACTIONS(60), 20,
      anon_sym_true,
      anon_sym_false,
      anon_sym_void,
      anon_sym_nothing,
      anon_sym_bool,
      anon_sym_int,
      anon_sym_uint,
      anon_sym_real,
      anon_sym_imag,
      anon_sym_complex,
      anon_sym_string,
      anon_sym_bytes,
      anon_sym_enum,
      anon_sym_record,
      anon_sym_class,
      anon_sym_owned,
      anon_sym_shared,
      anon_sym_unmanaged,
      anon_sym_borrowed,
      sym_identifier,
  [388] = 13,
    ACTIONS(9), 1,
      anon_sym_LBRACE,
    ACTIONS(13), 1,
      anon_sym_constref,
    ACTIONS(15), 1,
      anon_sym_return,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(64), 1,
      anon_sym_RBRACE,
    STATE(12), 1,
      sym_variable_expression,
    STATE(13), 1,
      sym_lvalue_expression,
    STATE(29), 1,
      sym_privacy_specifier,
    STATE(39), 1,
      sym_variable_kind,
    STATE(67), 1,
      sym_statements,
    ACTIONS(7), 2,
      anon_sym_private,
      anon_sym_public,
    ACTIONS(11), 4,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
    STATE(10), 6,
      sym__statement,
      sym_block_statement,
      sym_expression_statement,
      sym_assignment_statement,
      sym_variable_declaration_statement,
      sym_return_statement,
  [437] = 13,
    ACTIONS(9), 1,
      anon_sym_LBRACE,
    ACTIONS(13), 1,
      anon_sym_constref,
    ACTIONS(15), 1,
      anon_sym_return,
    ACTIONS(17), 1,
      sym_identifier,
    ACTIONS(66), 1,
      anon_sym_RBRACE,
    STATE(12), 1,
      sym_variable_expression,
    STATE(13), 1,
      sym_lvalue_expression,
    STATE(29), 1,
      sym_privacy_specifier,
    STATE(39), 1,
      sym_variable_kind,
    STATE(63), 1,
      sym_statements,
    ACTIONS(7), 2,
      anon_sym_private,
      anon_sym_public,
    ACTIONS(11), 4,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
    STATE(10), 6,
      sym__statement,
      sym_block_statement,
      sym_expression_statement,
      sym_assignment_statement,
      sym_variable_declaration_statement,
      sym_return_statement,
  [486] = 1,
    ACTIONS(68), 16,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
      anon_sym_PLUS_EQ,
      anon_sym_DASH_EQ,
      anon_sym_STAR_EQ,
      anon_sym_SLASH_EQ,
      anon_sym_PERCENT_EQ,
      anon_sym_STAR_STAR_EQ,
      anon_sym_AMP_EQ,
      anon_sym_PIPE_EQ,
      anon_sym_CARET_EQ,
      anon_sym_AMP_AMP_EQ,
      anon_sym_PIPE_PIPE_EQ,
      anon_sym_LT_LT_EQ,
      anon_sym_GT_GT_EQ,
  [505] = 2,
    ACTIONS(70), 1,
      anon_sym_SEMI,
    ACTIONS(72), 14,
      anon_sym_EQ,
      anon_sym_PLUS_EQ,
      anon_sym_DASH_EQ,
      anon_sym_STAR_EQ,
      anon_sym_SLASH_EQ,
      anon_sym_PERCENT_EQ,
      anon_sym_STAR_STAR_EQ,
      anon_sym_AMP_EQ,
      anon_sym_PIPE_EQ,
      anon_sym_CARET_EQ,
      anon_sym_AMP_AMP_EQ,
      anon_sym_PIPE_PIPE_EQ,
      anon_sym_LT_LT_EQ,
      anon_sym_GT_GT_EQ,
  [525] = 2,
    STATE(4), 1,
      sym_assignment_operator,
    ACTIONS(74), 14,
      anon_sym_EQ,
      anon_sym_PLUS_EQ,
      anon_sym_DASH_EQ,
      anon_sym_STAR_EQ,
      anon_sym_SLASH_EQ,
      anon_sym_PERCENT_EQ,
      anon_sym_STAR_STAR_EQ,
      anon_sym_AMP_EQ,
      anon_sym_PIPE_EQ,
      anon_sym_CARET_EQ,
      anon_sym_AMP_AMP_EQ,
      anon_sym_PIPE_PIPE_EQ,
      anon_sym_LT_LT_EQ,
      anon_sym_GT_GT_EQ,
  [545] = 2,
    ACTIONS(76), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(78), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [564] = 2,
    ACTIONS(80), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(82), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [583] = 2,
    ACTIONS(84), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(86), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [602] = 2,
    ACTIONS(88), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(90), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [621] = 2,
    ACTIONS(92), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(94), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [640] = 2,
    ACTIONS(96), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(98), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [659] = 2,
    ACTIONS(100), 6,
      ts_builtin_sym_end,
      sym_inline_comment,
      sym_block_comment,
      anon_sym_LBRACE,
      anon_sym_RBRACE,
      anon_sym_constref,
    ACTIONS(102), 8,
      anon_sym_private,
      anon_sym_public,
      anon_sym_param,
      anon_sym_const,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_return,
      sym_identifier,
  [678] = 3,
    STATE(28), 1,
      sym__separator_digits,
    ACTIONS(106), 2,
      anon_sym__,
      sym__digit,
    ACTIONS(104), 6,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
      anon_sym_DOT,
      anon_sym_e,
      anon_sym_E,
  [694] = 3,
    STATE(25), 1,
      sym__separator_digits,
    ACTIONS(106), 2,
      anon_sym__,
      sym__digit,
    ACTIONS(108), 6,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
      anon_sym_DOT,
      anon_sym_e,
      anon_sym_E,
  [710] = 4,
    ACTIONS(112), 1,
      anon_sym_DOT,
    STATE(51), 1,
      sym__exponent_part,
    ACTIONS(114), 2,
      anon_sym_e,
      anon_sym_E,
    ACTIONS(110), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [726] = 3,
    STATE(53), 1,
      sym__exponent_part,
    ACTIONS(114), 2,
      anon_sym_e,
      anon_sym_E,
    ACTIONS(116), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [739] = 1,
    ACTIONS(118), 6,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
      anon_sym_DOT,
      anon_sym_e,
      anon_sym_E,
  [748] = 3,
    STATE(47), 1,
      sym__exponent_part,
    ACTIONS(114), 2,
      anon_sym_e,
      anon_sym_E,
    ACTIONS(120), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [761] = 5,
    ACTIONS(124), 1,
      anon_sym_COLON,
    ACTIONS(126), 1,
      anon_sym_EQ,
    STATE(37), 1,
      sym_type_part,
    STATE(58), 1,
      sym_initialization_part,
    ACTIONS(122), 2,
      anon_sym_SEMI,
      anon_sym_COMMA,
  [778] = 1,
    ACTIONS(128), 6,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
      anon_sym_DOT,
      anon_sym_e,
      anon_sym_E,
  [787] = 3,
    ACTIONS(11), 1,
      anon_sym_const,
    STATE(41), 1,
      sym_variable_kind,
    ACTIONS(13), 4,
      anon_sym_param,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_constref,
  [800] = 5,
    ACTIONS(130), 1,
      anon_sym_DQUOTE,
    ACTIONS(132), 1,
      anon_sym_SQUOTE,
    ACTIONS(134), 1,
      aux_sym__string_character_token1,
    STATE(34), 1,
      sym__string_character,
    STATE(72), 1,
      sym__single_quote_delimited_characters,
  [816] = 5,
    ACTIONS(132), 1,
      anon_sym_DQUOTE,
    ACTIONS(136), 1,
      anon_sym_SQUOTE,
    ACTIONS(138), 1,
      aux_sym__string_character_token1,
    STATE(35), 1,
      sym__string_character,
    STATE(75), 1,
      sym__double_quote_delimited_characters,
  [832] = 4,
    ACTIONS(31), 1,
      sym__digit,
    STATE(49), 1,
      sym__digits,
    STATE(59), 1,
      sym__sign,
    ACTIONS(140), 2,
      anon_sym_PLUS,
      anon_sym_DASH,
  [846] = 4,
    ACTIONS(31), 1,
      sym__digit,
    STATE(26), 1,
      sym__digits,
    STATE(53), 1,
      sym__exponent_part,
    ACTIONS(114), 2,
      anon_sym_e,
      anon_sym_E,
  [860] = 5,
    ACTIONS(130), 1,
      anon_sym_DQUOTE,
    ACTIONS(134), 1,
      aux_sym__string_character_token1,
    ACTIONS(142), 1,
      anon_sym_SQUOTE,
    STATE(34), 1,
      sym__string_character,
    STATE(66), 1,
      sym__single_quote_delimited_characters,
  [876] = 5,
    ACTIONS(136), 1,
      anon_sym_SQUOTE,
    ACTIONS(138), 1,
      aux_sym__string_character_token1,
    ACTIONS(144), 1,
      anon_sym_DQUOTE,
    STATE(35), 1,
      sym__string_character,
    STATE(64), 1,
      sym__double_quote_delimited_characters,
  [892] = 2,
    ACTIONS(148), 1,
      anon_sym_const,
    ACTIONS(146), 4,
      anon_sym_param,
      anon_sym_var,
      anon_sym_ref,
      anon_sym_constref,
  [902] = 3,
    ACTIONS(126), 1,
      anon_sym_EQ,
    STATE(61), 1,
      sym_initialization_part,
    ACTIONS(150), 2,
      anon_sym_SEMI,
      anon_sym_COMMA,
  [913] = 1,
    ACTIONS(152), 4,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_COLON,
      anon_sym_EQ,
  [920] = 4,
    ACTIONS(154), 1,
      sym_identifier,
    STATE(27), 1,
      sym_identifier_list,
    STATE(62), 1,
      sym_variable_declaration,
    STATE(74), 1,
      sym_variable_declaration_list,
  [933] = 1,
    ACTIONS(156), 4,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_COLON,
      anon_sym_EQ,
  [940] = 4,
    ACTIONS(154), 1,
      sym_identifier,
    STATE(27), 1,
      sym_identifier_list,
    STATE(62), 1,
      sym_variable_declaration,
    STATE(68), 1,
      sym_variable_declaration_list,
  [953] = 4,
    ACTIONS(154), 1,
      sym_identifier,
    STATE(27), 1,
      sym_identifier_list,
    STATE(62), 1,
      sym_variable_declaration,
    STATE(73), 1,
      sym_variable_declaration_list,
  [966] = 1,
    ACTIONS(158), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [972] = 1,
    ACTIONS(160), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [978] = 1,
    ACTIONS(72), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [984] = 1,
    ACTIONS(162), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [990] = 1,
    ACTIONS(164), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [996] = 1,
    ACTIONS(166), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1002] = 1,
    ACTIONS(168), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1008] = 1,
    ACTIONS(170), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1014] = 1,
    ACTIONS(116), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1020] = 1,
    ACTIONS(172), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1026] = 1,
    ACTIONS(120), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1032] = 1,
    ACTIONS(174), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1038] = 1,
    ACTIONS(176), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1044] = 1,
    ACTIONS(178), 3,
      anon_sym_SEMI,
      anon_sym_COMMA,
      anon_sym_EQ,
  [1050] = 2,
    ACTIONS(31), 1,
      sym__digit,
    STATE(24), 1,
      sym__digits,
  [1057] = 1,
    ACTIONS(150), 2,
      anon_sym_SEMI,
      anon_sym_COMMA,
  [1062] = 2,
    ACTIONS(31), 1,
      sym__digit,
    STATE(43), 1,
      sym__digits,
  [1069] = 1,
    ACTIONS(180), 2,
      anon_sym_SEMI,
      anon_sym_COMMA,
  [1074] = 1,
    ACTIONS(182), 2,
      anon_sym_SEMI,
      anon_sym_COMMA,
  [1079] = 2,
    ACTIONS(184), 1,
      anon_sym_SEMI,
    ACTIONS(186), 1,
      anon_sym_COMMA,
  [1086] = 1,
    ACTIONS(188), 1,
      anon_sym_RBRACE,
  [1090] = 1,
    ACTIONS(190), 1,
      anon_sym_DQUOTE,
  [1094] = 1,
    ACTIONS(192), 1,
      anon_sym_SEMI,
  [1098] = 1,
    ACTIONS(194), 1,
      anon_sym_SQUOTE,
  [1102] = 1,
    ACTIONS(196), 1,
      anon_sym_RBRACE,
  [1106] = 1,
    ACTIONS(198), 1,
      anon_sym_SEMI,
  [1110] = 1,
    ACTIONS(174), 1,
      anon_sym_SEMI,
  [1114] = 1,
    ACTIONS(200), 1,
      ts_builtin_sym_end,
  [1118] = 1,
    ACTIONS(202), 1,
      sym_identifier,
  [1122] = 1,
    ACTIONS(204), 1,
      anon_sym_SQUOTE,
  [1126] = 1,
    ACTIONS(206), 1,
      anon_sym_SEMI,
  [1130] = 1,
    ACTIONS(208), 1,
      anon_sym_SEMI,
  [1134] = 1,
    ACTIONS(204), 1,
      anon_sym_DQUOTE,
};

static const uint32_t ts_small_parse_table_map[] = {
  [SMALL_STATE(2)] = 0,
  [SMALL_STATE(3)] = 66,
  [SMALL_STATE(4)] = 129,
  [SMALL_STATE(5)] = 192,
  [SMALL_STATE(6)] = 255,
  [SMALL_STATE(7)] = 307,
  [SMALL_STATE(8)] = 359,
  [SMALL_STATE(9)] = 388,
  [SMALL_STATE(10)] = 437,
  [SMALL_STATE(11)] = 486,
  [SMALL_STATE(12)] = 505,
  [SMALL_STATE(13)] = 525,
  [SMALL_STATE(14)] = 545,
  [SMALL_STATE(15)] = 564,
  [SMALL_STATE(16)] = 583,
  [SMALL_STATE(17)] = 602,
  [SMALL_STATE(18)] = 621,
  [SMALL_STATE(19)] = 640,
  [SMALL_STATE(20)] = 659,
  [SMALL_STATE(21)] = 678,
  [SMALL_STATE(22)] = 694,
  [SMALL_STATE(23)] = 710,
  [SMALL_STATE(24)] = 726,
  [SMALL_STATE(25)] = 739,
  [SMALL_STATE(26)] = 748,
  [SMALL_STATE(27)] = 761,
  [SMALL_STATE(28)] = 778,
  [SMALL_STATE(29)] = 787,
  [SMALL_STATE(30)] = 800,
  [SMALL_STATE(31)] = 816,
  [SMALL_STATE(32)] = 832,
  [SMALL_STATE(33)] = 846,
  [SMALL_STATE(34)] = 860,
  [SMALL_STATE(35)] = 876,
  [SMALL_STATE(36)] = 892,
  [SMALL_STATE(37)] = 902,
  [SMALL_STATE(38)] = 913,
  [SMALL_STATE(39)] = 920,
  [SMALL_STATE(40)] = 933,
  [SMALL_STATE(41)] = 940,
  [SMALL_STATE(42)] = 953,
  [SMALL_STATE(43)] = 966,
  [SMALL_STATE(44)] = 972,
  [SMALL_STATE(45)] = 978,
  [SMALL_STATE(46)] = 984,
  [SMALL_STATE(47)] = 990,
  [SMALL_STATE(48)] = 996,
  [SMALL_STATE(49)] = 1002,
  [SMALL_STATE(50)] = 1008,
  [SMALL_STATE(51)] = 1014,
  [SMALL_STATE(52)] = 1020,
  [SMALL_STATE(53)] = 1026,
  [SMALL_STATE(54)] = 1032,
  [SMALL_STATE(55)] = 1038,
  [SMALL_STATE(56)] = 1044,
  [SMALL_STATE(57)] = 1050,
  [SMALL_STATE(58)] = 1057,
  [SMALL_STATE(59)] = 1062,
  [SMALL_STATE(60)] = 1069,
  [SMALL_STATE(61)] = 1074,
  [SMALL_STATE(62)] = 1079,
  [SMALL_STATE(63)] = 1086,
  [SMALL_STATE(64)] = 1090,
  [SMALL_STATE(65)] = 1094,
  [SMALL_STATE(66)] = 1098,
  [SMALL_STATE(67)] = 1102,
  [SMALL_STATE(68)] = 1106,
  [SMALL_STATE(69)] = 1110,
  [SMALL_STATE(70)] = 1114,
  [SMALL_STATE(71)] = 1118,
  [SMALL_STATE(72)] = 1122,
  [SMALL_STATE(73)] = 1126,
  [SMALL_STATE(74)] = 1130,
  [SMALL_STATE(75)] = 1134,
};

static const TSParseActionEntry ts_parse_actions[] = {
  [0] = {.entry = {.count = 0, .reusable = false}},
  [1] = {.entry = {.count = 1, .reusable = false}}, RECOVER(),
  [3] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_source_file, 0),
  [5] = {.entry = {.count = 1, .reusable = true}}, SHIFT(7),
  [7] = {.entry = {.count = 1, .reusable = false}}, SHIFT(36),
  [9] = {.entry = {.count = 1, .reusable = true}}, SHIFT(9),
  [11] = {.entry = {.count = 1, .reusable = false}}, SHIFT(71),
  [13] = {.entry = {.count = 1, .reusable = true}}, SHIFT(71),
  [15] = {.entry = {.count = 1, .reusable = false}}, SHIFT(2),
  [17] = {.entry = {.count = 1, .reusable = false}}, SHIFT(11),
  [19] = {.entry = {.count = 1, .reusable = true}}, SHIFT(17),
  [21] = {.entry = {.count = 1, .reusable = false}}, SHIFT(56),
  [23] = {.entry = {.count = 1, .reusable = true}}, SHIFT(57),
  [25] = {.entry = {.count = 1, .reusable = true}}, SHIFT(31),
  [27] = {.entry = {.count = 1, .reusable = true}}, SHIFT(30),
  [29] = {.entry = {.count = 1, .reusable = false}}, SHIFT(50),
  [31] = {.entry = {.count = 1, .reusable = true}}, SHIFT(22),
  [33] = {.entry = {.count = 1, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2),
  [35] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(6),
  [38] = {.entry = {.count = 2, .reusable = false}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(36),
  [41] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(9),
  [44] = {.entry = {.count = 2, .reusable = false}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(71),
  [47] = {.entry = {.count = 2, .reusable = true}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(71),
  [50] = {.entry = {.count = 2, .reusable = false}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(2),
  [53] = {.entry = {.count = 2, .reusable = false}}, REDUCE(aux_sym_source_file_repeat1, 2), SHIFT_REPEAT(11),
  [56] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_source_file, 1),
  [58] = {.entry = {.count = 1, .reusable = true}}, SHIFT(6),
  [60] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_assignment_operator, 1),
  [62] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_assignment_operator, 1),
  [64] = {.entry = {.count = 1, .reusable = true}}, SHIFT(15),
  [66] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_statements, 1),
  [68] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_expression, 1),
  [70] = {.entry = {.count = 1, .reusable = true}}, SHIFT(20),
  [72] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_lvalue_expression, 1),
  [74] = {.entry = {.count = 1, .reusable = true}}, SHIFT(8),
  [76] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration_statement, 4),
  [78] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_variable_declaration_statement, 4),
  [80] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_block_statement, 2),
  [82] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_block_statement, 2),
  [84] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_block_statement, 3),
  [86] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_block_statement, 3),
  [88] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_return_statement, 2),
  [90] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_return_statement, 2),
  [92] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration_statement, 3),
  [94] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_variable_declaration_statement, 3),
  [96] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_assignment_statement, 4),
  [98] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_assignment_statement, 4),
  [100] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_expression_statement, 2),
  [102] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_expression_statement, 2),
  [104] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__separator_digits, 1),
  [106] = {.entry = {.count = 1, .reusable = true}}, SHIFT(21),
  [108] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__digits, 1),
  [110] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_integer_literal, 1),
  [112] = {.entry = {.count = 1, .reusable = true}}, SHIFT(33),
  [114] = {.entry = {.count = 1, .reusable = true}}, SHIFT(32),
  [116] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_real_literal, 2),
  [118] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__digits, 2),
  [120] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_real_literal, 3),
  [122] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration, 1),
  [124] = {.entry = {.count = 1, .reusable = true}}, SHIFT(5),
  [126] = {.entry = {.count = 1, .reusable = true}}, SHIFT(3),
  [128] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__separator_digits, 2),
  [130] = {.entry = {.count = 1, .reusable = false}}, SHIFT(34),
  [132] = {.entry = {.count = 1, .reusable = false}}, SHIFT(46),
  [134] = {.entry = {.count = 1, .reusable = true}}, SHIFT(34),
  [136] = {.entry = {.count = 1, .reusable = false}}, SHIFT(35),
  [138] = {.entry = {.count = 1, .reusable = true}}, SHIFT(35),
  [140] = {.entry = {.count = 1, .reusable = true}}, SHIFT(59),
  [142] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym__single_quote_delimited_characters, 1),
  [144] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym__double_quote_delimited_characters, 1),
  [146] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_privacy_specifier, 1),
  [148] = {.entry = {.count = 1, .reusable = false}}, REDUCE(sym_privacy_specifier, 1),
  [150] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration, 2),
  [152] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_identifier_list, 3),
  [154] = {.entry = {.count = 1, .reusable = true}}, SHIFT(40),
  [156] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_identifier_list, 1),
  [158] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__exponent_part, 3),
  [160] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_literal_expression, 1),
  [162] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_interpreted_string_literal, 2),
  [164] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_real_literal, 4),
  [166] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_type_part, 2),
  [168] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__exponent_part, 2),
  [170] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_primitive_type, 1),
  [172] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_interpreted_string_literal, 3),
  [174] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_type_expression, 1),
  [176] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_string_literal, 1),
  [178] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_bool_literal, 1),
  [180] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_initialization_part, 2),
  [182] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration, 3),
  [184] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration_list, 1),
  [186] = {.entry = {.count = 1, .reusable = true}}, SHIFT(42),
  [188] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_statements, 2),
  [190] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__double_quote_delimited_characters, 2),
  [192] = {.entry = {.count = 1, .reusable = true}}, SHIFT(19),
  [194] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym__single_quote_delimited_characters, 2),
  [196] = {.entry = {.count = 1, .reusable = true}}, SHIFT(16),
  [198] = {.entry = {.count = 1, .reusable = true}}, SHIFT(14),
  [200] = {.entry = {.count = 1, .reusable = true}},  ACCEPT_INPUT(),
  [202] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_kind, 1),
  [204] = {.entry = {.count = 1, .reusable = true}}, SHIFT(52),
  [206] = {.entry = {.count = 1, .reusable = true}}, REDUCE(sym_variable_declaration_list, 3),
  [208] = {.entry = {.count = 1, .reusable = true}}, SHIFT(18),
};

#ifdef __cplusplus
extern "C" {
#endif
#ifdef _WIN32
#define extern __declspec(dllexport)
#endif

extern const TSLanguage *tree_sitter_Chapel(void) {
  static const TSLanguage language = {
    .version = LANGUAGE_VERSION,
    .symbol_count = SYMBOL_COUNT,
    .alias_count = ALIAS_COUNT,
    .token_count = TOKEN_COUNT,
    .external_token_count = EXTERNAL_TOKEN_COUNT,
    .state_count = STATE_COUNT,
    .large_state_count = LARGE_STATE_COUNT,
    .production_id_count = PRODUCTION_ID_COUNT,
    .field_count = FIELD_COUNT,
    .max_alias_sequence_length = MAX_ALIAS_SEQUENCE_LENGTH,
    .parse_table = &ts_parse_table[0][0],
    .small_parse_table = ts_small_parse_table,
    .small_parse_table_map = ts_small_parse_table_map,
    .parse_actions = ts_parse_actions,
    .symbol_names = ts_symbol_names,
    .symbol_metadata = ts_symbol_metadata,
    .public_symbol_map = ts_symbol_map,
    .alias_map = ts_non_terminal_alias_map,
    .alias_sequences = &ts_alias_sequences[0][0],
    .lex_modes = ts_lex_modes,
    .lex_fn = ts_lex,
    .primary_state_ids = ts_primary_state_ids,
  };
  return &language;
}
#ifdef __cplusplus
}
#endif
