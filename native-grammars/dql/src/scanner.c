// Native DQL strings require Dgraph's escape whitelist before Go-style decoding.
// EOF checks keep physical NUL bytes distinct from the end of the source.
#include "tree_sitter/parser.h"

enum TokenType { STRING, COMMENT };

static int hex_value(int32_t c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

void *tree_sitter_dql_external_scanner_create(void) { return NULL; }
void tree_sitter_dql_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_dql_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}
void tree_sitter_dql_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_dql_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  (void)payload;
  // External tokens must pass trivia before the internal lexer can consume it.
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    lexer->advance(lexer, true);
  }
  if (valid_symbols[COMMENT] && lexer->lookahead == '#') {
    do {
      lexer->advance(lexer, false);
    } while (!lexer->eof(lexer) && lexer->lookahead != '\r' && lexer->lookahead != '\n');
    lexer->mark_end(lexer);
    lexer->result_symbol = COMMENT;
    return true;
  }
  if (!valid_symbols[STRING] || lexer->lookahead != '"') return false;
  lexer->advance(lexer, false);
  while (!lexer->eof(lexer)) {
    int32_t c = lexer->lookahead;
    if (c == '"') {
      lexer->advance(lexer, false);
      lexer->mark_end(lexer);
      lexer->result_symbol = STRING;
      return true;
    }
    if (c == '\n') return false;
    lexer->advance(lexer, false);
    if (c != '\\') continue;
    if (lexer->eof(lexer)) return false;
    c = lexer->lookahead;
    lexer->advance(lexer, false);
    if (c == 'u') {
      unsigned value = 0;
      for (unsigned i = 0; i < 4; ++i) {
        int digit = hex_value(lexer->lookahead);
        if (lexer->eof(lexer) || digit < 0) return false;
        value = value * 16 + (unsigned)digit;
        lexer->advance(lexer, false);
      }
      if (value >= 0xd800 && value <= 0xdfff) return false;
    } else if (c != '"' && c != '\\' && c != 'v' && c != 't' && c != 'b' &&
               c != 'n' && c != 'r' && c != 'f') {
      return false;
    }
  }
  return false;
}
