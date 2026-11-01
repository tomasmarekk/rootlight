// N-Quads string/comment terminals preserve permitted NUL as input data.
// This stateless scanner uses the lexer EOF contract rather than a zero sentinel.

#include "tree_sitter/parser.h"
#include <stddef.h>
#include <stdint.h>

enum TokenType { STRING, COMMENT };

static int hex_digit(int32_t character) {
  if (character >= '0' && character <= '9') return character - '0';
  if (character >= 'a' && character <= 'f') return character - 'a' + 10;
  if (character >= 'A' && character <= 'F') return character - 'A' + 10;
  return -1;
}

static bool scan_escape(TSLexer *lexer) {
  lexer->advance(lexer, false);
  if (lexer->eof(lexer)) return false;
  switch (lexer->lookahead) {
    case 't': case 'b': case 'n': case 'r': case 'f':
    case '"': case '\'': case '\\':
      lexer->advance(lexer, false);
      return true;
    case 'u': case 'U': {
      const unsigned digits = lexer->lookahead == 'u' ? 4 : 8;
      uint32_t value = 0;
      lexer->advance(lexer, false);
      for (unsigned index = 0; index < digits; index++) {
        if (lexer->eof(lexer)) return false;
        const int digit = hex_digit(lexer->lookahead);
        if (digit < 0) return false;
        value = (value << 4) | (uint32_t)digit;
        lexer->advance(lexer, false);
      }
      return value <= 0x10FFFF && !(value >= 0xD800 && value <= 0xDFFF);
    }
    default:
      return false;
  }
}

void *tree_sitter_nquads_external_scanner_create(void) { return NULL; }

void tree_sitter_nquads_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_nquads_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_nquads_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_nquads_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  (void)payload;
  while (!lexer->eof(lexer) && (lexer->lookahead == ' ' || lexer->lookahead == '\t')) {
    lexer->advance(lexer, true);
  }
  if (lexer->eof(lexer)) return false;
  if (valid_symbols[COMMENT] && lexer->lookahead == '#') {
    do {
      lexer->advance(lexer, false);
    } while (!lexer->eof(lexer) && lexer->lookahead != '\r' && lexer->lookahead != '\n');
    lexer->result_symbol = COMMENT;
    return true;
  }
  if (!valid_symbols[STRING] || lexer->lookahead != '"') return false;
  lexer->advance(lexer, false);
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == '"') {
      lexer->advance(lexer, false);
      lexer->result_symbol = STRING;
      return true;
    }
    if (lexer->lookahead == '\r' || lexer->lookahead == '\n') return false;
    if (lexer->lookahead == '\\') {
      if (!scan_escape(lexer)) return false;
    } else {
      lexer->advance(lexer, false);
    }
  }
  return false;
}
