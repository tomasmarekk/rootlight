// External scanner for tree-sitter-qsharp.
//
// Handles float literals to disambiguate from integer+range.
// The problem: `0..5` = int(0) + range(..) + int(5)
//              `0.5`  = float(0.5)
//              `1.`   = float(1.)
// Q# rule: dot after digits is float only if NOT followed by another dot.
// This requires lookahead that regex can't express.
//
// External tokens have higher priority than internal regex tokens in
// tree-sitter, so this scanner is tried first when both float_literal
// and integer_literal are valid symbols.

#include "tree_sitter/parser.h"

enum TokenType {
  FLOAT_LITERAL,
  ERROR_SENTINEL, // unused — detects error recovery mode
};

// Required tree-sitter callbacks. Scanner is stateless — no heap, no state.
void *tree_sitter_qsharp_external_scanner_create(void) { return NULL; }

void tree_sitter_qsharp_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_qsharp_external_scanner_serialize(void *payload,
                                                       char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_qsharp_external_scanner_deserialize(void *payload,
                                                     const char *buffer,
                                                     unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static inline bool is_digit(int32_t c) {
  return (c >= '0' && c <= '9') ? true : false;
}

// Consume digits and underscores, guarding against EOF.
static void skip_digits(TSLexer *lexer) {
  while (!lexer->eof(lexer) &&
         (is_digit(lexer->lookahead) || lexer->lookahead == '_')) {
    advance(lexer);
  }
}

// Try to scan an exponent part (e.g. `e+3`, `E-10`).
static bool scan_exponent(TSLexer *lexer) {
  if (lexer->eof(lexer)) {
    return false;
  }
  if (lexer->lookahead != 'e' && lexer->lookahead != 'E') {
    return false;
  }
  advance(lexer);
  if (!lexer->eof(lexer) &&
      (lexer->lookahead == '+' || lexer->lookahead == '-')) {
    advance(lexer);
  }
  if (lexer->eof(lexer) || !is_digit(lexer->lookahead)) {
    return false;
  }
  skip_digits(lexer);
  return true;
}

bool tree_sitter_qsharp_external_scanner_scan(void *payload, TSLexer *lexer,
                                              const bool *valid_symbols) {
  (void)payload;

  // During error recovery, all tokens are marked valid.
  // Bail out and let tree-sitter's internal lexer handle it.
  if (valid_symbols[ERROR_SENTINEL]) {
    return false;
  }

  if (!valid_symbols[FLOAT_LITERAL]) {
    return false;
  }

  // Skip whitespace — tree-sitter calls external scanner before skipping it.
  while (!lexer->eof(lexer) &&
         (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
          lexer->lookahead == '\n' || lexer->lookahead == '\r')) {
    lexer->advance(lexer, true);
  }

  if (lexer->eof(lexer) || !is_digit(lexer->lookahead)) {
    return false;
  }

  // Consume leading digits (decimal only — hex/oct/bin can't be floats)
  skip_digits(lexer);

  bool has_dot = false;
  bool has_exp = false;

  // Check for decimal dot — but NOT double-dot (range operator)
  if (!lexer->eof(lexer) && lexer->lookahead == '.') {
    lexer->mark_end(lexer);
    advance(lexer);
    if (lexer->lookahead == '.') {
      return false; // `N..` = integer followed by range
    }
    // After dot: could be `N.` (float at EOF/before non-digit) or `N.123`
    has_dot = true;
    skip_digits(lexer);
  }

  has_exp = scan_exponent(lexer);

  // Without dot or exponent, this is just an integer — let regex handle it
  if (!has_dot && !has_exp) {
    return false;
  }

  // Suffixed literals are handled by other grammar rules
  if (!lexer->eof(lexer) &&
      (lexer->lookahead == 'i' || lexer->lookahead == 'L')) {
    return false;
  }

  lexer->mark_end(lexer);
  lexer->result_symbol = FLOAT_LITERAL;
  return true;
}
