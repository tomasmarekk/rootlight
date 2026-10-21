/* Bounded derivative of WolframResearch/tree-sitter-wolfram 15b3244e, MIT.
 * The scanner is stateless; request refusal stays outside parser rollback.
 * A host must finish its exclusive scope before accepting any recovered tree. */
#include "tree_sitter/parser.h"
#include "rootlight_wolfram_status.h"
#include <wctype.h>

enum TokenType { COMMENT, STATEMENT_SEP };
enum { WOLFRAM_MAX_COMMENT_DEPTH = 1024 };

static bool scalar_valid(const TSLexer *lexer) {
  const int32_t scalar = lexer->lookahead;
  if (scalar == 0 && !lexer->eof(lexer)) {
    rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_UNSUPPORTED_NUL);
    return false;
  }
  if (scalar < 0 || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) {
    rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_INVALID_SCALAR);
    return false;
  }
  return true;
}

static bool advance(TSLexer *lexer, bool skip) {
  if (!rootlight_wolfram_scope_step()) return false;
  lexer->advance(lexer, skip);
  return scalar_valid(lexer);
}

static bool whitespace(int32_t scalar) {
  /* Windows wint_t cannot represent supplementary scalars. Truncating one
   * could misclassify its low word as whitespace and silently discard it. */
  return scalar >= 0 && scalar <= 0xffff && iswspace((wint_t)scalar) != 0;
}

static bool scan_comment(TSLexer *lexer) {
  unsigned depth = 1;
  while (!lexer->eof(lexer)) {
    switch (lexer->lookahead) {
      case '*':
        if (!advance(lexer, false)) return false;
        if (lexer->lookahead == ')') {
          if (!advance(lexer, false)) return false;
          if (--depth == 0) {
            lexer->result_symbol = COMMENT;
            lexer->mark_end(lexer);
            return true;
          }
        }
        break;
      case '(':
        if (!advance(lexer, false)) return false;
        if (lexer->lookahead == '*') {
          if (depth == WOLFRAM_MAX_COMMENT_DEPTH) {
            rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_CAPACITY);
            return false;
          }
          /* Consume both opening scalars: its star cannot also close it. */
          if (!advance(lexer, false)) return false;
          ++depth;
        }
        break;
      default:
        if (!advance(lexer, false)) return false;
        break;
    }
  }
  return false;
}

/* No scanner allocation or persistent state is required. Even NULL payloads
 * must be used inside an active scope, including native parser creation. */
void *tree_sitter_wolfram_external_scanner_create(void) {
  rootlight_wolfram_scope_step();
  return NULL;
}

/* The stateless payload owns nothing; parser destruction may follow finish. */
void tree_sitter_wolfram_external_scanner_destroy(void *payload) {
  (void)payload;
}

/* The only supported serialized state is empty; status is never serialized. */
unsigned tree_sitter_wolfram_external_scanner_serialize(void *payload, char *buffer) {
  (void)buffer;
  if (payload != NULL) rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_MALFORMED_STATE);
  rootlight_wolfram_scope_step();
  return 0;
}

/* Reject foreign state instead of allowing rollback to clear a refusal. */
void tree_sitter_wolfram_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)buffer;
  if (payload != NULL || length != 0) {
    rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_MALFORMED_STATE);
  }
  rootlight_wolfram_scope_step();
}

/* Charge entry and each scalar advance. A false token result alone never
 * proves successful scanning: only matching scope finish can admit syntax. */
bool tree_sitter_wolfram_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  if (!rootlight_wolfram_scope_step()) return false;
  if (payload != NULL || !lexer || !valid_symbols || !lexer->advance || !lexer->mark_end || !lexer->eof) {
    rootlight_wolfram_scope_refuse(ROOTLIGHT_WOLFRAM_MALFORMED_STATE);
    return false;
  }
  if (!scalar_valid(lexer)) return false;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\f' || lexer->lookahead == '\v') {
    if (!advance(lexer, true)) return false;
  }
  if (valid_symbols[STATEMENT_SEP] && lexer->lookahead == '\n') {
    while (whitespace(lexer->lookahead)) {
      if (!advance(lexer, false)) return false;
    }
    lexer->result_symbol = STATEMENT_SEP;
    lexer->mark_end(lexer);
    return true;
  }
  while (whitespace(lexer->lookahead)) {
    if (!advance(lexer, true)) return false;
  }
  if (valid_symbols[COMMENT] && lexer->lookahead == '(') {
    if (!advance(lexer, false)) return false;
    if (lexer->lookahead == '*') {
      if (!advance(lexer, false)) return false;
      return scan_comment(lexer);
    }
  }
  return false;
}
