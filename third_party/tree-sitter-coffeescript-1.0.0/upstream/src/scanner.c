#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "tree_sitter/parser.h"

enum TokenType {
  NEWLINE,
  INDENT,
  DEDENT,
  IMPLICIT_CALL_OPEN,
};

typedef struct {
  uint16_t indent_stack[1024];
  uint16_t indent_stack_size;
  uint16_t dedent_count;
  bool newline_pending;
  bool indent_pending;
} Scanner;

static inline void advance(TSLexer *lexer) {
  lexer->advance(lexer, false);
}

static inline void skip(TSLexer *lexer) {
  lexer->advance(lexer, true);
}

static void reset(Scanner *scanner) {
  scanner->indent_stack_size = 1;
  scanner->indent_stack[0] = 0;
  scanner->dedent_count = 0;
  scanner->newline_pending = false;
  scanner->indent_pending = false;
}

static uint16_t calculate_indent(TSLexer *lexer) {
  uint16_t indent_length = 0;
  while (true) {
    if (lexer->lookahead == ' ') {
      skip(lexer);
      indent_length++;
    } else if (lexer->lookahead == '\t') {
      skip(lexer);
      indent_length += 8 - (indent_length % 8);
    } else if (lexer->lookahead == '\f') {
      skip(lexer);
    } else {
      break;
    }
  }
  return indent_length;
}

static void handle_newline(Scanner *scanner, TSLexer *lexer) {
  // Consume the newline character.
  advance(lexer);
  lexer->mark_end(lexer);
  scanner->newline_pending = true;

  while (true) {
    uint16_t indent_length = calculate_indent(lexer);
    uint16_t current_indent = scanner->indent_stack[scanner->indent_stack_size - 1];

    if (lexer->lookahead == '#') {
      // Preserve comment tokens for the main lexer while keeping indentation unchanged.
      indent_length = current_indent;
      break;
    }

    if (lexer->lookahead == '\r') {
      skip(lexer);
      continue;
    }

    if (lexer->lookahead == '\n') {
      advance(lexer);
      lexer->mark_end(lexer);
      continue;
    }

    if (indent_length > current_indent) {
      if (scanner->indent_stack_size < sizeof(scanner->indent_stack) / sizeof(scanner->indent_stack[0])) {
        scanner->indent_stack[scanner->indent_stack_size++] = indent_length;
        scanner->indent_pending = true;
      }
    } else if (indent_length < current_indent) {
      while (scanner->indent_stack_size > 1 && indent_length < scanner->indent_stack[scanner->indent_stack_size - 1]) {
        scanner->indent_stack_size--;
        scanner->dedent_count++;
      }
    }

    break;
  }
}

static inline bool is_ident_start(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' || c == '$';
}

static inline bool is_ident_part(int32_t c) {
  return is_ident_start(c) || (c >= '0' && c <= '9');
}

// Words that, when they follow `<expr> ` (expression then a space), are NOT the
// start of an implicit-call argument: binary/keyword operators and postfix
// control words. Value-producing prefixes (`new`, `not`, `typeof`, `await`,
// `yes`, `null`, ...) are intentionally absent — they may begin an argument.
static bool is_non_argument_word(const char *word) {
  static const char *const words[] = {
    "and", "or", "is", "isnt", "instanceof", "in", "of",
    "then", "else", "when", "by", "extends",
    "if", "unless", "while", "until", "for", "catch", "finally",
  };
  for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++) {
    if (strcmp(word, words[i]) == 0) {
      return true;
    }
  }
  return false;
}

// Emit a zero-width IMPLICIT_CALL_OPEN when the parser has just finished a
// callable expression and the upcoming text looks like a paren-less argument:
// horizontal space, then an argument-starting token (with CoffeeScript's
// whitespace rule for `-`/`+`: `a -b` is `a(-b)`, but `a - b` is subtraction).
// Called only when lexer->lookahead is a space or tab, so it never competes
// with the newline/indent logic below.
static bool scan_implicit_call_open(TSLexer *lexer) {
  // Zero-width: the marker sits right after the callee, before the space.
  lexer->mark_end(lexer);

  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    advance(lexer);
  }

  int32_t c = lexer->lookahead;

  // End of line / input, or a closing/separating token: not an argument.
  if (c == 0 || c == '\n' || c == '\r') {
    return false;
  }

  switch (c) {
    case '"':
    case '\'':
    case '@':
    case '[':
    case '{':
      lexer->result_symbol = IMPLICIT_CALL_OPEN;
      return true;
    default:
      break;
  }

  if (c >= '0' && c <= '9') {
    lexer->result_symbol = IMPLICIT_CALL_OPEN;
    return true;
  }

  // `-`/`+` begin an argument only as a prefix operator: a space before but no
  // space after (`a -b`). With a trailing space (`a - b`) it is binary.
  if (c == '-' || c == '+') {
    advance(lexer);
    int32_t d = lexer->lookahead;
    // Exclude `->` (function arrow), `-=`/`+=`, `--`/`++`, and a trailing space
    // (which would make `-`/`+` a binary operator: `a - b`).
    if (d == ' ' || d == '\t' || d == '\n' || d == '\r' || d == 0 ||
        d == '=' || d == c || (c == '-' && d == '>')) {
      return false;
    }
    lexer->result_symbol = IMPLICIT_CALL_OPEN;
    return true;
  }

  if (is_ident_start(c)) {
    char word[16];
    int len = 0;
    while (is_ident_part(lexer->lookahead) && len < (int)sizeof(word) - 1) {
      word[len++] = (char)lexer->lookahead;
      advance(lexer);
    }
    word[len] = '\0';
    if (is_non_argument_word(word)) {
      return false;
    }
    lexer->result_symbol = IMPLICIT_CALL_OPEN;
    return true;
  }

  return false;
}

static bool scan(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
  if (scanner->newline_pending && valid_symbols[NEWLINE]) {
    scanner->newline_pending = false;
    lexer->result_symbol = NEWLINE;
    return true;
  }

  if (scanner->dedent_count > 0 && valid_symbols[DEDENT]) {
    scanner->dedent_count--;
    lexer->result_symbol = DEDENT;
    return true;
  }

  if (scanner->indent_pending && valid_symbols[INDENT]) {
    scanner->indent_pending = false;
    lexer->result_symbol = INDENT;
    return true;
  }

  // Implicit (paren-less) call detection. Only when nothing indentation-related
  // is pending and the next character is horizontal whitespace separating a
  // callee from its argument; this keeps it disjoint from the newline scan.
  if (valid_symbols[IMPLICIT_CALL_OPEN] && !scanner->newline_pending &&
      scanner->dedent_count == 0 && !scanner->indent_pending &&
      (lexer->lookahead == ' ' || lexer->lookahead == '\t')) {
    return scan_implicit_call_open(lexer);
  }

  if (!valid_symbols[NEWLINE] && !valid_symbols[INDENT] && !valid_symbols[DEDENT]) {
    return false;
  }

  while (lexer->lookahead == '\r') {
    skip(lexer);
  }

  if (lexer->lookahead == 0) {
    if (valid_symbols[DEDENT] && scanner->indent_stack_size > 1) {
      scanner->indent_stack_size--;
      lexer->result_symbol = DEDENT;
      return true;
    }
    return false;
  }

  if (lexer->lookahead != '\n') {
    return false;
  }

  handle_newline(scanner, lexer);

  if (scanner->newline_pending && valid_symbols[NEWLINE]) {
    scanner->newline_pending = false;
    lexer->result_symbol = NEWLINE;
    return true;
  }

  if (scanner->dedent_count > 0 && valid_symbols[DEDENT]) {
    scanner->dedent_count--;
    lexer->result_symbol = DEDENT;
    return true;
  }

  if (scanner->indent_pending && valid_symbols[INDENT]) {
    scanner->indent_pending = false;
    lexer->result_symbol = INDENT;
    return true;
  }

  return false;
}

void *tree_sitter_coffeescript_external_scanner_create() {
  Scanner *scanner = (Scanner *)calloc(1, sizeof(Scanner));
  reset(scanner);
  return scanner;
}

void tree_sitter_coffeescript_external_scanner_destroy(void *payload) {
  free((Scanner *)payload);
}

void tree_sitter_coffeescript_external_scanner_reset(void *payload) {
  reset((Scanner *)payload);
}

unsigned tree_sitter_coffeescript_external_scanner_serialize(void *payload, char *buffer) {
  Scanner *scanner = (Scanner *)payload;
  size_t size = 0;

  memcpy(buffer + size, &scanner->indent_stack_size, sizeof(scanner->indent_stack_size));
  size += sizeof(scanner->indent_stack_size);

  size_t stack_bytes = scanner->indent_stack_size * sizeof(uint16_t);
  memcpy(buffer + size, scanner->indent_stack, stack_bytes);
  size += stack_bytes;

  memcpy(buffer + size, &scanner->dedent_count, sizeof(scanner->dedent_count));
  size += sizeof(scanner->dedent_count);

  buffer[size++] = scanner->newline_pending ? 1 : 0;
  buffer[size++] = scanner->indent_pending ? 1 : 0;

  return (unsigned)size;
}

void tree_sitter_coffeescript_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  Scanner *scanner = (Scanner *)payload;

  if (length < sizeof(scanner->indent_stack_size)) {
    reset(scanner);
    return;
  }

  size_t size = 0;

  memcpy(&scanner->indent_stack_size, buffer + size, sizeof(scanner->indent_stack_size));
  size += sizeof(scanner->indent_stack_size);

  size_t stack_bytes = scanner->indent_stack_size * sizeof(uint16_t);
  if (size + stack_bytes > length) {
    reset(scanner);
    return;
  }

  memcpy(scanner->indent_stack, buffer + size, stack_bytes);
  size += stack_bytes;

  if (size + sizeof(scanner->dedent_count) > length) {
    reset(scanner);
    return;
  }

  memcpy(&scanner->dedent_count, buffer + size, sizeof(scanner->dedent_count));
  size += sizeof(scanner->dedent_count);

  if (size >= length) {
    scanner->newline_pending = false;
    scanner->indent_pending = false;
    return;
  }

  scanner->newline_pending = buffer[size++] != 0;
  scanner->indent_pending = size < length ? buffer[size++] != 0 : false;
}

bool tree_sitter_coffeescript_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  return scan((Scanner *)payload, lexer, valid_symbols);
}
