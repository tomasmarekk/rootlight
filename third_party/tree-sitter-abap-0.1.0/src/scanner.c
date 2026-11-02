// External scanner for tree-sitter-abap.
//
// Handles the one ABAP lexical rule that needs column awareness: a `*` in
// column 1 comments out the entire line, while `*` anywhere else is the
// multiplication operator (or part of `**`). tree-sitter's declarative
// lexer has no column anchor, so this lives here.

#include "tree_sitter/parser.h"

enum TokenType {
  STAR_COMMENT,
};

void *tree_sitter_abap_external_scanner_create(void) { return NULL; }

void tree_sitter_abap_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_abap_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_abap_external_scanner_deserialize(void *payload, const char *buffer,
                                                   unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_abap_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
  (void)payload;

  if (!valid_symbols[STAR_COMMENT]) {
    return false;
  }

  // The internal lexer consumes whitespace extras and the following token in
  // one pass, so this scanner must skip whitespace itself to ever observe a
  // `*` sitting at the start of a line.
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    lexer->advance(lexer, true);
  }

  // Star comments are only recognized when `*` is the very first character
  // of a line; indentation disqualifies (it advances the column).
  if (lexer->lookahead != '*' || lexer->get_column(lexer) != 0) {
    return false;
  }

  lexer->advance(lexer, false);
  while (lexer->lookahead != '\n' && lexer->lookahead != 0) {
    lexer->advance(lexer, false);
  }

  lexer->result_symbol = STAR_COMMENT;
  return true;
}
