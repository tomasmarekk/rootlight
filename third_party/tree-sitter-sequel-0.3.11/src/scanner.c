/* Dollar-quoted SQL tokens with exact, bounded incremental scanner state.
 * A snapshot is observational: only accepted start/end tokens change context.
 */
#include "tree_sitter/parser.h"
#include <stdlib.h>
#include <string.h>

enum TokenType {
  DOLLAR_QUOTED_STRING_START_TAG,
  DOLLAR_QUOTED_STRING_END_TAG,
  DOLLAR_QUOTED_STRING
};

enum { STATE_VERSION = 1, HEADER_SIZE = 3,
       MAX_TAG_BYTES = TREE_SITTER_SERIALIZATION_BUFFER_SIZE - HEADER_SIZE };

typedef struct {
  uint16_t length;
  unsigned char bytes[MAX_TAG_BYTES];
} Tag;

typedef struct { Tag tag; } LexerState;

static bool space(int32_t c) {
  return c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\f' || c == '\v';
}

static bool tag_character(uint32_t c) {
  /* Preserve the grammar's permissive punctuation tags. Token boundaries are
   * not a claim that every SQL dialect permits the same delimiter syntax. */
  return c != 0 && c != '$' && c <= 0x10ffff && !(c >= 0xd800 && c <= 0xdfff) &&
         !space((int32_t)c);
}

static bool append(Tag *tag, uint32_t c) {
  unsigned n = c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
  if (c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff) || tag->length > MAX_TAG_BYTES - n) return false;
  if (n == 1) tag->bytes[tag->length++] = (unsigned char)c;
  else {
    tag->bytes[tag->length++] = (unsigned char)((n == 2 ? 0xc0 : n == 3 ? 0xe0 : 0xf0) | (c >> (6 * (n - 1))));
    for (unsigned i = n - 1; i > 0; i--) tag->bytes[tag->length++] = (unsigned char)(0x80 | ((c >> (6 * (i - 1))) & 0x3f));
  }
  return true;
}

static bool scalar(const unsigned char *bytes, unsigned length, unsigned *offset, uint32_t *out) {
  if (*offset >= length) return false;
  uint32_t c = bytes[(*offset)++];
  if (c < 0x80) { *out = c; return true; }
  unsigned n;
  uint32_t minimum;
  if (c >= 0xc2 && c <= 0xdf) { n = 1; minimum = 0x80; c &= 0x1f; }
  else if (c >= 0xe0 && c <= 0xef) { n = 2; minimum = 0x800; c &= 0x0f; }
  else if (c >= 0xf0 && c <= 0xf4) { n = 3; minimum = 0x10000; c &= 7; }
  else return false;
  if (n > length - *offset) return false;
  for (unsigned i = 0; i < n; i++) {
    unsigned char next = bytes[(*offset)++];
    if ((next & 0xc0) != 0x80) return false;
    c = (c << 6) | (next & 0x3f);
  }
  if (c < minimum || c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff)) return false;
  *out = c;
  return true;
}

static bool valid_tag(const unsigned char *bytes, unsigned length) {
  if (length < 2 || length > MAX_TAG_BYTES || bytes[0] != '$' || bytes[length - 1] != '$') return false;
  unsigned offset = 1;
  while (offset < length - 1) {
    uint32_t c;
    if (!scalar(bytes, length - 1, &offset, &c) || !tag_character(c)) return false;
  }
  return true;
}

static bool scan_tag(TSLexer *lexer, Tag *tag) {
  tag->length = 0;
  if (lexer->lookahead != '$') return false;
  append(tag, '$');
  lexer->advance(lexer, false);
  while (!lexer->eof(lexer) && lexer->lookahead != '$') {
    if (lexer->lookahead < 0 || !tag_character((uint32_t)lexer->lookahead) ||
        !append(tag, (uint32_t)lexer->lookahead)) return false;
    lexer->advance(lexer, false);
  }
  if (lexer->eof(lexer) || !append(tag, '$')) return false;
  lexer->advance(lexer, false);
  return true;
}

static bool equal(const Tag *a, const Tag *b) {
  return a->length == b->length && memcmp(a->bytes, b->bytes, a->length) == 0;
}

static bool scan_body(TSLexer *lexer, const Tag *tag) {
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead != '$') { lexer->advance(lexer, false); continue; }
    unsigned offset = 0;
    while (offset < tag->length) {
      uint32_t expected;
      if (!scalar(tag->bytes, tag->length, &offset, &expected)) return false;
      if (lexer->eof(lexer) || lexer->lookahead < 0 || (uint32_t)lexer->lookahead != expected) break;
      lexer->advance(lexer, false);
      if (offset == tag->length) return true;
    }
    /* A mismatching '$' can itself open the matching delimiter. The first '$'
     * was consumed, so retrying here both preserves overlaps and advances. */
  }
  return false;
}

void *tree_sitter_sql_external_scanner_create(void) {
  return calloc(1, sizeof(LexerState));
}

void tree_sitter_sql_external_scanner_destroy(void *payload) {
  free(payload);
}

bool tree_sitter_sql_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  LexerState *state = payload;
  if (!state || (!valid_symbols[DOLLAR_QUOTED_STRING_START_TAG] &&
                 !valid_symbols[DOLLAR_QUOTED_STRING_END_TAG] && !valid_symbols[DOLLAR_QUOTED_STRING])) return false;
  while (space(lexer->lookahead) && !lexer->eof(lexer)) lexer->advance(lexer, true);
  Tag candidate = {0};
  if (!scan_tag(lexer, &candidate)) return false;
  if (state->tag.length && equal(&candidate, &state->tag)) {
    if (!valid_symbols[DOLLAR_QUOTED_STRING_END_TAG]) return false;
    state->tag.length = 0;
    lexer->result_symbol = DOLLAR_QUOTED_STRING_END_TAG;
  } else if (!state->tag.length && valid_symbols[DOLLAR_QUOTED_STRING_START_TAG]) {
    state->tag = candidate;
    lexer->result_symbol = DOLLAR_QUOTED_STRING_START_TAG;
  } else {
    if (!valid_symbols[DOLLAR_QUOTED_STRING] || !scan_body(lexer, &candidate)) return false;
    lexer->result_symbol = DOLLAR_QUOTED_STRING;
  }
  lexer->mark_end(lexer);
  return true;
}

unsigned tree_sitter_sql_external_scanner_serialize(void *payload, char *buffer) {
  const LexerState *state = payload;
  if (!state || !buffer || !state->tag.length) return 0;
  unsigned length = state->tag.length;
  buffer[0] = STATE_VERSION;
  buffer[1] = (char)(length & 0xff);
  buffer[2] = (char)(length >> 8);
  memcpy(buffer + HEADER_SIZE, state->tag.bytes, length);
  return HEADER_SIZE + length;
}

void tree_sitter_sql_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  LexerState *state = payload;
  if (!state) return;
  state->tag.length = 0;
  if (!buffer || length < HEADER_SIZE + 2 || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) return;
  const unsigned char *bytes = (const unsigned char *)buffer;
  unsigned tag_length = (unsigned)bytes[1] | ((unsigned)bytes[2] << 8);
  if (bytes[0] != STATE_VERSION || tag_length != length - HEADER_SIZE ||
      !valid_tag(bytes + HEADER_SIZE, tag_length)) return;
  memcpy(state->tag.bytes, bytes + HEADER_SIZE, tag_length);
  state->tag.length = (uint16_t)tag_length;
}
