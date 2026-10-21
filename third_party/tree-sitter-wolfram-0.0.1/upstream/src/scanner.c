#include "tree_sitter/parser.h"

#include <wctype.h>

enum TokenType {
  COMMENT,
  STATEMENT_SEP
};

static bool scan_comment(TSLexer *lexer) {
  int depth = 1;

  while (!lexer->eof(lexer)) {
    switch (lexer->lookahead) {
      case '*':
        lexer->advance(lexer, false);
        if (lexer->lookahead == ')') {
          depth--;
          if (depth == 0) {
            lexer->result_symbol = COMMENT;
            lexer->advance(lexer, false);
            lexer->mark_end(lexer);
            return true;
          }
        }
        break;

      case '(':
        lexer->advance(lexer, false);
        if (lexer->lookahead == '*') {
          depth++;
        }
        break;

      default:
        lexer->advance(lexer, false);
        break;
    }
  }

  return false;
}

void *tree_sitter_wolfram_external_scanner_create(void) {
  return NULL;
}

void tree_sitter_wolfram_external_scanner_destroy(void *payload) {
}

unsigned tree_sitter_wolfram_external_scanner_serialize(void *payload, char *buffer) {
  return 0;
}

void tree_sitter_wolfram_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
}

bool tree_sitter_wolfram_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  // Skip horizontal whitespace, but stop at a newline. A newline at a position
  // where a statement separator is valid (i.e. between complete top-level
  // expressions) is significant: it splits statements rather than acting as the
  // whitespace glue that would otherwise form an implicit_times. Inside brackets
  // STATEMENT_SEP is never valid, so newlines there remain insignificant.
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\f' ||
         lexer->lookahead == '\v') {
    lexer->advance(lexer, true);
  }

  if (valid_symbols[STATEMENT_SEP] && lexer->lookahead == '\n') {
    while (iswspace(lexer->lookahead)) {
      lexer->advance(lexer, false);
    }
    lexer->result_symbol = STATEMENT_SEP;
    lexer->mark_end(lexer);
    return true;
  }

  // Newline is not significant here (mid-expression, or inside brackets): treat
  // all remaining whitespace as ordinary extras before checking for a comment.
  while (iswspace(lexer->lookahead)) {
    lexer->advance(lexer, true);
  }

  if (lexer->lookahead == '(') {
    lexer->advance(lexer, false);
    if (lexer->lookahead == '*') {
      lexer->advance(lexer, false);
      return scan_comment(lexer);
    }
  }

  return false;
}
