// Operator ends must precede both characters of a comment or delimiter pair.
// The scanner has no persistent state, allocation or Eiffel execution.
#include "tree_sitter/parser.h"
#include "operator_symbols.h"

enum TokenType { FREE_OPERATOR };

static bool is_break(int32_t character) {
  return character == ' ' || (character >= '\t' && character <= '\r') ||
         character == 0xa0 || character == 0x1680 ||
         (character >= 0x2000 && character <= 0x200a) ||
         character == 0x202f || character == 0x205f || character == 0x3000;
}

static bool is_symbol(int32_t character) {
  return set_contains(operator_symbols,
                      (uint32_t)(sizeof(operator_symbols) / sizeof(operator_symbols[0])),
                      character);
}

static bool is_legacy_tail(int32_t character) {
  return (character >= 'a' && character <= 'z') ||
         (character >= 'A' && character <= 'Z') ||
         (character >= '0' && character <= '9') ||
         character == '+' || character == '-' || character == '*' ||
         character == '/' || character == '\\' || character == '.' ||
         character == '<' || character == '=' || character == '>' ||
         character == '#' || character == '|' || character == '&';
}

static bool begins_delimiter(int32_t character, int32_t next) {
  return (character == '-' && (next == '-' || next == '>')) ||
         (character == '<' && next == '<') ||
         (character == '>' && next == '>');
}

static bool is_reserved(int32_t first, int32_t second, unsigned length) {
  if (length == 1) {
    switch (first) {
      case '+': case '-': case '*': case '/': case '^': case '<': case '>':
      case '~': case '!': case '?': case '@':
      case 0x2227: case 0x2228: case 0x22bb: case 0x21d2: case 0x00ac:
      case 0x2200: case 0x2203: case 0x00a6: case 0x27f3: case 0x27f2:
        return true;
      default:
        return false;
    }
  }
  return length == 2 &&
         ((first == '/' && (second == '/' || second == '~')) ||
          (first == '\\' && second == '\\') ||
          ((first == 0x2227 || first == 0x2228) && second == 0x2026));
}

void *tree_sitter_eiffel_external_scanner_create(void) { return NULL; }
void tree_sitter_eiffel_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_eiffel_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}
void tree_sitter_eiffel_external_scanner_deserialize(void *payload, const char *buffer,
                                                    unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_eiffel_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  (void)payload;
  if (!valid_symbols[FREE_OPERATOR]) return false;
  while (is_break(lexer->lookahead)) lexer->advance(lexer, true);
  const int32_t first = lexer->lookahead;
  const bool legacy = first == '#' || first == '|' || first == '&';
  if (!is_symbol(first)) return false;
  int32_t second = 0;
  unsigned length = 0;
  bool accepted = false;
  while (is_symbol(lexer->lookahead) ||
         (legacy && length > 0 && is_legacy_tail(lexer->lookahead))) {
    const int32_t character = lexer->lookahead;
    lexer->advance(lexer, false);
    // Keep the previous mark: the first delimiter character belongs to the
    // following token too, even though lookahead has already advanced past it.
    // Legacy tails admit shifts and arrows, but neither branch consumes comments.
    if ((character == '-' && lexer->lookahead == '-') ||
        (!legacy && begins_delimiter(character, lexer->lookahead))) break;
    if (length == 1) second = character;
    if (length < 3) length++;
    accepted = !is_reserved(first, second, length);
    if (accepted) lexer->mark_end(lexer);
  }
  if (!accepted) return false;
  lexer->result_symbol = FREE_OPERATOR;
  return true;
}
