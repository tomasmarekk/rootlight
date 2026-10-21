// Chapel permits nested block comments, which cannot be a recursive grammar extra.
// This stateless token scanner hides the complete comment, including every nested body.
#include "tree_sitter/parser.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum TokenType { BLOCK_COMMENT };

// No scanner state or allocation survives a scan.
void *tree_sitter_Chapel_external_scanner_create(void) { return NULL; }
void tree_sitter_Chapel_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_Chapel_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}
void tree_sitter_Chapel_external_scanner_deserialize(void *payload,
                                                   const char *buffer,
                                                   unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

// Unterminated comments decline; recovery must not expose their body as clean syntax.
bool tree_sitter_Chapel_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
  (void)payload;
  if (!valid_symbols[BLOCK_COMMENT] || lexer->lookahead != '/') return false;
  lexer->advance(lexer, false);
  if (lexer->lookahead != '*') return false;
  lexer->advance(lexer, false);
  uint32_t depth = 1;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '/') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '*') {
        if (depth == UINT32_MAX) return false;
        ++depth;
        lexer->advance(lexer, false);
      }
    } else if (lexer->lookahead == '*') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '/') {
        lexer->advance(lexer, false);
        if (--depth == 0) {
          lexer->mark_end(lexer);
          lexer->result_symbol = BLOCK_COMMENT;
          return true;
        }
      }
    } else {
      lexer->advance(lexer, false);
    }
  }
  return false;
}
