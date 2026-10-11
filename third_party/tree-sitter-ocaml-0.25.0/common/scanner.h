#ifndef TREE_SITTER_OCAML_SCANNER_H_
#define TREE_SITTER_OCAML_SCANNER_H_

#include <assert.h>
#include <string.h>
#include <wctype.h>

#include "tree_sitter/alloc.h"
#include "tree_sitter/parser.h"

enum TokenType {
  COMMENT,
  LEFT_QUOTED_STRING_DELIM,
  RIGHT_QUOTED_STRING_DELIM,
  STRING_DELIM,
  LINE_NUMBER_DIRECTIVE,
  NULL_CHARACTER,
  ERROR_SENTINEL,
};

typedef struct {
  bool in_string;

  size_t quoted_string_id_length;
  size_t quoted_string_id_capacity;
  int32_t *quoted_string_id;
} Scanner;

static inline void quoted_string_id_clear(Scanner *scanner) {
  scanner->quoted_string_id_length = 0;
}

static inline void quoted_string_id_resize(Scanner *scanner,
                                           size_t min_capacity) {
  size_t capacity = scanner->quoted_string_id_capacity;

  if (capacity >= min_capacity) return;

  if (capacity < 16) capacity = 16;
  while (capacity < min_capacity) capacity <<= 1;

  scanner->quoted_string_id_capacity = capacity;
  scanner->quoted_string_id =
      ts_realloc(scanner->quoted_string_id, capacity * sizeof(int32_t));
}

static inline size_t quoted_string_id_utf8_width(int32_t scalar) {
  if (scalar < 0 || scalar > 0x10ffff ||
      (scalar >= 0xd800 && scalar <= 0xdfff)) return 0;
  if (scalar <= 0x7f) return 1;
  if (scalar <= 0x7ff) return 2;
  if (scalar <= 0xffff) return 3;
  return 4;
}

static inline bool quoted_string_id_assign(Scanner *scanner, const char *buffer,
                                          size_t length) {
  if (length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE - 1) return false;

  // Decode before committing so malformed state cannot retain a valid prefix.
  int32_t decoded[TREE_SITTER_SERIALIZATION_BUFFER_SIZE - 1];
  const unsigned char *bytes = (const unsigned char *)buffer;
  size_t count = 0;
  for (size_t offset = 0; offset < length;) {
    unsigned char lead = bytes[offset];
    size_t width;
    uint32_t scalar;
    if (lead <= 0x7f) {
      width = 1;
      scalar = lead;
    } else if (lead >= 0xc2 && lead <= 0xdf) {
      width = 2;
      scalar = lead & 0x1f;
    } else if (lead >= 0xe0 && lead <= 0xef) {
      width = 3;
      scalar = lead & 0x0f;
    } else if (lead >= 0xf0 && lead <= 0xf4) {
      width = 4;
      scalar = lead & 0x07;
    } else {
      return false;
    }
    if (width > length - offset) return false;
    for (size_t i = 1; i < width; i++) {
      unsigned char continuation = bytes[offset + i];
      if ((continuation & 0xc0) != 0x80) return false;
      scalar = (scalar << 6) | (continuation & 0x3f);
    }
    if (quoted_string_id_utf8_width((int32_t)scalar) != width) return false;
    decoded[count++] = (int32_t)scalar;
    offset += width;
  }

  if (count > 0 && (count > scanner->quoted_string_id_capacity ||
                    scanner->quoted_string_id == NULL)) {
    int32_t *grown = ts_realloc(scanner->quoted_string_id,
                               count * sizeof(int32_t));
    if (grown == NULL) return false;
    scanner->quoted_string_id = grown;
    scanner->quoted_string_id_capacity = count;
  }
  if (count > 0) {
    memcpy(scanner->quoted_string_id, decoded, count * sizeof(int32_t));
  }
  scanner->quoted_string_id_length = count;
  return true;
}

static inline bool quoted_string_id_copy(Scanner *scanner, char *buffer,
                                        size_t *length) {
  size_t remaining = TREE_SITTER_SERIALIZATION_BUFFER_SIZE - 1;
  if (scanner->quoted_string_id_length > remaining ||
      (scanner->quoted_string_id_length > 0 &&
       scanner->quoted_string_id == NULL)) return false;

  // Refusal must leave the entire output untouched, not emit a partial state.
  for (size_t i = 0; i < scanner->quoted_string_id_length; i++) {
    size_t width = quoted_string_id_utf8_width(scanner->quoted_string_id[i]);
    if (width == 0 || width > remaining) return false;
    remaining -= width;
  }

  unsigned char *bytes = (unsigned char *)buffer;
  size_t offset = 0;
  for (size_t i = 0; i < scanner->quoted_string_id_length; i++) {
    uint32_t scalar = (uint32_t)scanner->quoted_string_id[i];
    if (scalar <= 0x7f) {
      bytes[offset++] = (unsigned char)scalar;
    } else if (scalar <= 0x7ff) {
      bytes[offset++] = (unsigned char)(0xc0 | (scalar >> 6));
      bytes[offset++] = (unsigned char)(0x80 | (scalar & 0x3f));
    } else if (scalar <= 0xffff) {
      bytes[offset++] = (unsigned char)(0xe0 | (scalar >> 12));
      bytes[offset++] = (unsigned char)(0x80 | ((scalar >> 6) & 0x3f));
      bytes[offset++] = (unsigned char)(0x80 | (scalar & 0x3f));
    } else {
      bytes[offset++] = (unsigned char)(0xf0 | (scalar >> 18));
      bytes[offset++] = (unsigned char)(0x80 | ((scalar >> 12) & 0x3f));
      bytes[offset++] = (unsigned char)(0x80 | ((scalar >> 6) & 0x3f));
      bytes[offset++] = (unsigned char)(0x80 | (scalar & 0x3f));
    }
  }
  *length = offset;
  return true;
}

static inline void quoted_string_id_push(Scanner *scanner, int32_t c) {
  quoted_string_id_resize(scanner, scanner->quoted_string_id_length + 1);
  scanner->quoted_string_id[scanner->quoted_string_id_length++] = c;
}

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static inline void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

static inline bool eof(TSLexer *lexer) { return lexer->eof(lexer); }

static void scan_string(TSLexer *lexer) {
  for (;;) {
    switch (lexer->lookahead) {
      case '\\':
        advance(lexer);
        advance(lexer);
        break;
      case '"':
        advance(lexer);
        return;
      case '\0':
        if (eof(lexer)) return;
        advance(lexer);
        break;
      default:
        advance(lexer);
    }
  }
}

static const int32_t LOWER_UTF8_CHARS[] = {
    // https://github.com/ocaml/ocaml/blob/5.3.0-rc1/utils/misc.ml#L281-L298
    0xdf,   // ß
    0xe0,   // à
    0xe1,   // á
    0xe2,   // â
    0xe3,   // ã
    0xe4,   // ä
    0xe5,   // å
    0xe6,   // æ
    0xe7,   // ç
    0xe8,   // è
    0xe9,   // é
    0xea,   // ê
    0xeb,   // ë
    0xec,   // ì
    0xed,   // í
    0xee,   // î
    0xef,   // ï
    0xf0,   // ð
    0xf1,   // ñ
    0xf2,   // ò
    0xf3,   // ó
    0xf4,   // ô
    0xf5,   // õ
    0xf6,   // ö
    0xf8,   // ø
    0xf9,   // ù
    0xfa,   // ú
    0xfb,   // û
    0xfc,   // ü
    0xfd,   // ý
    0xfe,   // þ
    0xff,   // ÿ
    0x153,  // œ
    0x161,  // š
    0x17e,  // ž
};

static const int32_t LOWER_UTF8_PAIRS[][3] = {
    // https://github.com/ocaml/ocaml/blob/5.3.0-rc1/utils/misc.ml#L326-L340
    {'a', 0x300, 0xe0},   // à
    {'a', 0x301, 0xe1},   // á
    {'a', 0x302, 0xe2},   // â
    {'a', 0x303, 0xe3},   // ã
    {'a', 0x308, 0xe4},   // ä
    {'a', 0x30a, 0xe5},   // å
    {'c', 0x327, 0xe7},   // ç
    {'e', 0x300, 0xe8},   // è
    {'e', 0x301, 0xe9},   // é
    {'e', 0x302, 0xea},   // ê
    {'e', 0x308, 0xeb},   // ë
    {'i', 0x300, 0xec},   // ì
    {'i', 0x301, 0xed},   // í
    {'i', 0x302, 0xee},   // î
    {'i', 0x308, 0xef},   // ï
    {'n', 0x303, 0xf1},   // ñ
    {'o', 0x300, 0xf2},   // ò
    {'o', 0x301, 0xf3},   // ó
    {'o', 0x302, 0xf4},   // ô
    {'o', 0x303, 0xf5},   // õ
    {'o', 0x308, 0xf6},   // ö
    {'s', 0x30c, 0x161},  // š
    {'u', 0x300, 0xf9},   // ù
    {'u', 0x301, 0xfa},   // ú
    {'u', 0x302, 0xfb},   // û
    {'u', 0x308, 0xfc},   // ü
    {'y', 0x301, 0xfd},   // ý
    {'y', 0x308, 0xff},   // ÿ
    {'z', 0x30c, 0x17e},  // ž
};

static inline int32_t search_lower_utf8_char(const int32_t c) {
  const int32_t *base = LOWER_UTF8_CHARS;
  const size_t size = sizeof(LOWER_UTF8_CHARS) / sizeof(LOWER_UTF8_CHARS[0]);

  for (size_t range = size; range != 0; range >>= 1) {
    const int32_t *probe = base + (range >> 1);
    if (c == *probe) {
      return *probe;
    } else if (c > *probe) {
      base = probe + 1;
      range--;
    }
  }

  return 0;
}

static inline int32_t search_lower_utf8_pair(const int32_t c,
                                             const int32_t diacritic) {
  const int32_t(*base)[3] = LOWER_UTF8_PAIRS;
  const size_t size = sizeof(LOWER_UTF8_PAIRS) / sizeof(LOWER_UTF8_PAIRS[0]);

  for (size_t range = size; range != 0; range >>= 1) {
    const int32_t *probe = base[range >> 1];

    if (c == probe[0] && diacritic == probe[1]) {
      return probe[2];
    } else if (c > probe[0] || (c == probe[0] && diacritic > probe[1])) {
      base = base + (range >> 1) + 1;
      range--;
    }
  }

  return 0;
}

static inline int32_t scan_quoted_string_delim_char(TSLexer *lexer) {
  const int32_t c = lexer->lookahead;
  int32_t r;

  if (c == '|') return 0;

  if (c == '_') {
    advance(lexer);
    return c;
  }

  if (c >= 'a' && c <= 'z') {
    advance(lexer);

    if (lexer->lookahead >= 0x300 && lexer->lookahead <= 0x327 &&
        (r = search_lower_utf8_pair(c, lexer->lookahead))) {
      advance(lexer);
      return r;
    } else {
      return c;
    }
  }

  if ((r = search_lower_utf8_char(c))) {
    advance(lexer);
    return r;
  }

  return 0;
}

static bool scan_left_quoted_string_delimiter(Scanner *scanner,
                                              TSLexer *lexer) {
  int32_t c;

  while ((c = scan_quoted_string_delim_char(lexer))) {
    quoted_string_id_push(scanner, c);
  }

  if (lexer->lookahead == '|') {
    advance(lexer);
    scanner->in_string = true;
    return true;
  } else {
    quoted_string_id_clear(scanner);
    return false;
  }
}

static bool scan_right_quoted_string_delimiter(Scanner *scanner,
                                               TSLexer *lexer) {
  for (size_t i = 0; i < scanner->quoted_string_id_length; i++) {
    if (scan_quoted_string_delim_char(lexer) != scanner->quoted_string_id[i]) {
      return false;
    }
  }

  if (lexer->lookahead == '}') {
    scanner->in_string = false;
    quoted_string_id_clear(scanner);
    return true;
  } else {
    return false;
  }
}

static bool scan_quoted_string(Scanner *scanner, TSLexer *lexer) {
  if (!scan_left_quoted_string_delimiter(scanner, lexer)) return false;

  for (;;) {
    switch (lexer->lookahead) {
      case '|':
        advance(lexer);
        if (scan_right_quoted_string_delimiter(scanner, lexer)) return true;
        break;
      case '\0':
        if (eof(lexer)) return false;
        advance(lexer);
        break;
      default:
        advance(lexer);
    }
  }
}

static int32_t scan_character(TSLexer *lexer) {
  int32_t last = 0;

  switch (lexer->lookahead) {
    case '\\':
      advance(lexer);
      if (iswdigit(lexer->lookahead)) {
        advance(lexer);
        for (size_t i = 0; i < 2; i++) {
          if (!iswdigit(lexer->lookahead)) return 0;
          advance(lexer);
        }
      } else {
        switch (lexer->lookahead) {
          case 'x':
            advance(lexer);
            for (size_t i = 0; i < 2; i++) {
              if (!iswdigit(lexer->lookahead) &&
                  (towupper(lexer->lookahead) < 'A' ||
                   towupper(lexer->lookahead) > 'F')) {
                return 0;
              }
              advance(lexer);
            }
            break;
          case 'o':
            advance(lexer);
            for (size_t i = 0; i < 3; i++) {
              if (!iswdigit(lexer->lookahead) || lexer->lookahead > '7') {
                return 0;
              }
              advance(lexer);
            }
            break;
          case '\'':
          case '"':
          case '\\':
          case 'n':
          case 't':
          case 'b':
          case 'r':
          case ' ':
            last = lexer->lookahead;
            advance(lexer);
            break;
          default:
            return 0;
        }
      }
      break;
    case '\'':
      break;
    case '\r':
      advance(lexer);
      while (lexer->lookahead == '\r') advance(lexer);
      if (lexer->lookahead != '\n') return 0;
      advance(lexer);
      break;
    case '\0':
      if (eof(lexer)) return 0;
      advance(lexer);
      break;
    default:
      if (lexer->lookahead < 256) {
        last = lexer->lookahead;
        advance(lexer);
      } else {
        return 0;
      }
  }

  if (lexer->lookahead == '\'') {
    advance(lexer);
    return 0;
  }
  return last;
}

static inline bool is_lowercase_ext(const int32_t c) {
  return (c >= 'a' && c <= 'z') || c == '_' || c >= 192;
}

static inline bool is_identstart(const int32_t c) {
  return is_lowercase_ext(c) || (c >= 'A' && c <= 'Z');
}

static inline bool is_identchar(const int32_t c) {
  return is_identstart(c) || (c >= '0' && c <= '9') || c == '\'';
}

static inline bool scan_identifier(TSLexer *lexer) {
  if (is_identstart(lexer->lookahead)) {
    advance(lexer);
    while (is_identchar(lexer->lookahead)) advance(lexer);
    return true;
  }
  return false;
}

static inline bool scan_extattrident(TSLexer *lexer) {
  while (scan_identifier(lexer)) {
    if (lexer->lookahead != '.') return true;
    advance(lexer);
  }
  return false;
}

static bool scan_comment(Scanner *scanner, TSLexer *lexer) {
  int32_t last = 0;

  if (lexer->lookahead != '*') return false;
  advance(lexer);

  for (;;) {
    switch (last ? last : lexer->lookahead) {
      case '(':
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        scan_comment(scanner, lexer);
        break;
      case '*':
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        if (lexer->lookahead == ')') {
          advance(lexer);
          return true;
        }
        break;
      case '\'':
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        last = scan_character(lexer);
        break;
      case '"':
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        scan_string(lexer);
        break;
      case '{':
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        if (lexer->lookahead == '%') {
          advance(lexer);
          if (lexer->lookahead == '%') advance(lexer);
          if (scan_extattrident(lexer)) {
            while (iswspace(lexer->lookahead)) advance(lexer);
          } else {
            break;
          }
        }
        if (scan_quoted_string(scanner, lexer)) advance(lexer);
        break;
      case '\0':
        if (eof(lexer)) return true;
        if (last) {
          last = 0;
        } else {
          advance(lexer);
        }
        break;
      default:
        if (scan_identifier(lexer) || last) {
          last = 0;
        } else {
          advance(lexer);
        }
    }
  }
}

static Scanner *create() {
  Scanner *scanner = ts_calloc(1, sizeof(Scanner));
  return scanner;
}

static void destroy(Scanner *scanner) {
  ts_free(scanner->quoted_string_id);
  ts_free(scanner);
}

static unsigned serialize(Scanner *scanner, char *buffer) {
  size_t length;
  if (!quoted_string_id_copy(scanner, buffer + 1, &length)) return 0;
  buffer[0] = scanner->in_string;
  return (unsigned)(length + 1);
}

static void deserialize(Scanner *scanner, const char *buffer, unsigned length) {
  if (length == 0 || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE ||
      buffer == NULL || (buffer[0] != 0 && buffer[0] != 1) ||
      !quoted_string_id_assign(scanner, buffer + 1, length - 1)) {
    scanner->in_string = false;
    quoted_string_id_clear(scanner);
    return;
  }
  scanner->in_string = buffer[0] != 0;
}

static bool scan(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
  if (!valid_symbols[ERROR_SENTINEL] &&
      valid_symbols[LEFT_QUOTED_STRING_DELIM] &&
      (is_lowercase_ext(lexer->lookahead) || lexer->lookahead == '|')) {
    lexer->result_symbol = LEFT_QUOTED_STRING_DELIM;
    return scan_left_quoted_string_delimiter(scanner, lexer);
  }

  if (!valid_symbols[ERROR_SENTINEL] &&
      valid_symbols[RIGHT_QUOTED_STRING_DELIM] && (lexer->lookahead == '|')) {
    advance(lexer);
    lexer->result_symbol = RIGHT_QUOTED_STRING_DELIM;
    return scan_right_quoted_string_delimiter(scanner, lexer);
  }

  if (scanner->in_string && valid_symbols[STRING_DELIM] &&
      lexer->lookahead == '"') {
    advance(lexer);
    scanner->in_string = false;
    lexer->result_symbol = STRING_DELIM;
    return true;
  }

  while (iswspace(lexer->lookahead)) {
    skip(lexer);
  }

  if (!scanner->in_string && valid_symbols[STRING_DELIM] &&
      lexer->lookahead == '"') {
    advance(lexer);
    scanner->in_string = true;
    lexer->result_symbol = STRING_DELIM;
    return true;
  }

  if (!scanner->in_string && valid_symbols[LINE_NUMBER_DIRECTIVE] &&
      lexer->lookahead == '#' && lexer->get_column(lexer) == 0) {
    advance(lexer);

    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
      advance(lexer);
    }

    if (!iswdigit(lexer->lookahead)) return false;
    while (iswdigit(lexer->lookahead)) advance(lexer);

    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
      advance(lexer);
    }

    if (lexer->lookahead != '"') return false;
    advance(lexer);

    while (lexer->lookahead != '\n' && lexer->lookahead != '\r' &&
           lexer->lookahead != '"' && !eof(lexer)) {
      advance(lexer);
    }

    if (lexer->lookahead != '"') return false;
    advance(lexer);

    while (lexer->lookahead != '\n' && lexer->lookahead != '\r' &&
           !eof(lexer)) {
      advance(lexer);
    }

    lexer->result_symbol = LINE_NUMBER_DIRECTIVE;
    return true;
  }

  if (!scanner->in_string && valid_symbols[COMMENT] &&
      lexer->lookahead == '(') {
    advance(lexer);
    lexer->result_symbol = COMMENT;
    return scan_comment(scanner, lexer);
  }

  if (valid_symbols[NULL_CHARACTER] && lexer->lookahead == '\0' &&
      !eof(lexer)) {
    advance(lexer);
    lexer->result_symbol = NULL_CHARACTER;
    return true;
  }

  return false;
}

#endif  // TREE_SITTER_OCAML_SCANNER_H_
