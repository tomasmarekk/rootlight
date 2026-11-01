// Preserve embedded NUL in native Turtle/TriG strings and comments.
// The generated negated character classes otherwise treat zero as end of input.

#include "tree_sitter/parser.h"
#include <stddef.h>

enum TokenType { NUL_CHARACTER, COMMENT, IRI_ANCHOR, IRI_CLOSE };

void *tree_sitter_turtle_external_scanner_create(void) { return NULL; }

void tree_sitter_turtle_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_turtle_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_turtle_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_turtle_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  (void)payload;
  if (lexer->eof(lexer)) return false;
  if (valid_symbols[NUL_CHARACTER]) {
    // NUL is valid only inside a quoted body; '#' there is string data.
    if (lexer->lookahead != 0) return false;
    lexer->advance(lexer, false);
    lexer->result_symbol = NUL_CHARACTER;
    return true;
  }
  if (valid_symbols[IRI_CLOSE]) {
    // The optional IRI body keeps its closing delimiter valid at every position.
    if (valid_symbols[IRI_ANCHOR] && lexer->lookahead == '#') {
      lexer->advance(lexer, false);
      lexer->result_symbol = IRI_ANCHOR;
      return true;
    }
    if (lexer->lookahead != '>') return false;
    lexer->advance(lexer, false);
    lexer->result_symbol = IRI_CLOSE;
    return true;
  }
  if (!valid_symbols[COMMENT]) return false;
  while (!lexer->eof(lexer) &&
         (lexer->lookahead == ' ' || (lexer->lookahead >= '\t' && lexer->lookahead <= '\r'))) {
    lexer->advance(lexer, true);
  }
  if (lexer->eof(lexer) || lexer->lookahead != '#') return false;
  do {
    lexer->advance(lexer, false);
  } while (!lexer->eof(lexer) && lexer->lookahead != '\n');
  lexer->result_symbol = COMMENT;
  return true;
}
