/* Authored repair for balanced Common Lisp block comments.
 * Retained upstream grammar notices remain in the adjacent notices directory.
 * The token preserves physical extent, not reader or runtime macro identity.
 */

#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>

enum TokenType { BLOCK_COMMENT };

void *tree_sitter_commonlisp_external_scanner_create(void) { return NULL; }

void tree_sitter_commonlisp_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_commonlisp_external_scanner_serialize(void *payload,
                                                        char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_commonlisp_external_scanner_deserialize(void *payload,
                                                      const char *buffer,
                                                      unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_commonlisp_external_scanner_scan(void *payload, TSLexer *lexer,
                                               const bool *valid_symbols) {
  (void)payload;
  if (!valid_symbols[BLOCK_COMMENT] || lexer->eof(lexer) ||
      lexer->lookahead != '#') {
    return false;
  }
  lexer->advance(lexer, false);
  if (lexer->eof(lexer) || lexer->lookahead != '|') {
    return false;
  }
  lexer->advance(lexer, false);

  uint32_t depth = 1;
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '#') {
      lexer->advance(lexer, false);
      if (!lexer->eof(lexer) && lexer->lookahead == '|') {
        if (depth == UINT32_MAX) {
          return false;
        }
        lexer->advance(lexer, false);
        ++depth;
      }
    } else if (lexer->lookahead == '|') {
      lexer->advance(lexer, false);
      if (!lexer->eof(lexer) && lexer->lookahead == '#') {
        lexer->advance(lexer, false);
        --depth;
        if (depth == 0) {
          lexer->mark_end(lexer);
          lexer->result_symbol = BLOCK_COMMENT;
          return true;
        }
      }
    } else {
      lexer->advance(lexer, false);
    }
  }
  // An unterminated or overflowing comment never becomes a successful token.
  return false;
}
