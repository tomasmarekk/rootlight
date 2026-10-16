#include <stdint.h>
#include <tree_sitter/alloc.h>
#include <tree_sitter/parser.h>

enum TokenType {
  BRACKET_ARGUMENT_OPEN, BRACKET_ARGUMENT_CONTENT, BRACKET_ARGUMENT_CLOSE,
  BRACKET_COMMENT_OPEN, BRACKET_COMMENT_CONTENT, BRACKET_COMMENT_CLOSE,
  LINE_COMMENT, NO_BRACKET,
};

struct TreeSitterCMakeState {
  uint32_t level;
  enum TokenType token;
};

#define SERIALIZED_STATE_SIZE 6
#define MAX_BRACKET_LEVEL (UINT32_MAX - 4)

static void reset(struct TreeSitterCMakeState *state) {
  state->level = 0;
  state->token = NO_BRACKET;
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static bool increment_level(uint32_t *level) {
  if (*level == MAX_BRACKET_LEVEL) return false;
  ++*level;
  return true;
}

static bool open_brackets(struct TreeSitterCMakeState *state, TSLexer *lexer) {
  if (lexer->lookahead != '[') return false;
  advance(lexer);
  uint32_t level = 0;
  while (lexer->lookahead == '=') {
    if (!increment_level(&level)) return false;
    advance(lexer);
  }
  if (lexer->lookahead != '[') return false;
  advance(lexer);
  lexer->mark_end(lexer);
  state->level = level;
  return true;
}

static bool bracket_content(const struct TreeSitterCMakeState *state,
                            TSLexer *lexer) {
  // Preserve every content byte, including spaces and an initial newline.
  lexer->mark_end(lexer);
  while (!lexer->eof(lexer)) {
    if (lexer->lookahead == ']') {
      lexer->mark_end(lexer);
      advance(lexer);
      uint32_t level = 0;
      while (lexer->lookahead == '=') {
        if (!increment_level(&level)) return false;
        advance(lexer);
      }
      if (level == state->level && lexer->lookahead == ']') return true;
      // A failed delimiter attempt remains content, including an EOF tail.
      lexer->mark_end(lexer);
      continue;
    }
    advance(lexer);
    lexer->mark_end(lexer);
  }
  return true;
}

static bool close_brackets(struct TreeSitterCMakeState *state, TSLexer *lexer) {
  if (lexer->lookahead != ']') return false;
  advance(lexer);
  uint32_t level = 0;
  while (lexer->lookahead == '=') {
    if (!increment_level(&level)) return false;
    advance(lexer);
  }
  if (level != state->level || lexer->lookahead != ']') return false;
  advance(lexer);
  lexer->mark_end(lexer);
  reset(state);
  return true;
}

void *tree_sitter_cmake_external_scanner_create(void) {
  struct TreeSitterCMakeState *state = ts_calloc(1, sizeof(*state));
  if (state != NULL) reset(state);
  return state;
}

void tree_sitter_cmake_external_scanner_destroy(void *payload) { ts_free(payload); }

unsigned tree_sitter_cmake_external_scanner_serialize(void *payload, char *buffer) {
  const struct TreeSitterCMakeState *state = payload;
  if (state == NULL) return 0;
  // Fixed bytes exclude struct padding, platform enum width and stale fields.
  buffer[0] = 1;
  buffer[1] = (char)state->token;
  for (unsigned index = 0; index < 4; ++index)
    buffer[2 + index] = (char)((state->level >> (index * 8)) & UINT32_C(255));
  return SERIALIZED_STATE_SIZE;
}

void tree_sitter_cmake_external_scanner_deserialize(void *payload,
                                                  const char *buffer,
                                                  unsigned length) {
  struct TreeSitterCMakeState *state = payload;
  if (state == NULL) return;
  reset(state);
  if (length != SERIALIZED_STATE_SIZE || (unsigned char)buffer[0] != 1) return;
  const unsigned char token = (unsigned char)buffer[1];
  if (token != BRACKET_ARGUMENT_OPEN && token != BRACKET_ARGUMENT_CONTENT &&
      token != BRACKET_COMMENT_OPEN && token != BRACKET_COMMENT_CONTENT &&
      token != NO_BRACKET) return;
  uint32_t level = 0;
  for (unsigned index = 0; index < 4; ++index)
    level |= (uint32_t)(unsigned char)buffer[2 + index] << (index * 8);
  if (level > MAX_BRACKET_LEVEL || (token == NO_BRACKET && level != 0)) return;
  state->level = level;
  state->token = (enum TokenType)token;
}

bool tree_sitter_cmake_external_scanner_scan(void *payload, TSLexer *lexer,
                                           const bool *valid_symbols) {
  struct TreeSitterCMakeState *state = payload;
  if (state == NULL) return false;
  if (valid_symbols[BRACKET_ARGUMENT_CONTENT] && state->token == BRACKET_ARGUMENT_OPEN) {
    if (!bracket_content(state, lexer)) return false;
    state->token = BRACKET_ARGUMENT_CONTENT;
    lexer->result_symbol = BRACKET_ARGUMENT_CONTENT;
    return true;
  }
  if (valid_symbols[BRACKET_COMMENT_CONTENT] && state->token == BRACKET_COMMENT_OPEN) {
    if (!bracket_content(state, lexer)) return false;
    state->token = BRACKET_COMMENT_CONTENT;
    lexer->result_symbol = BRACKET_COMMENT_CONTENT;
    return true;
  }
  if (valid_symbols[BRACKET_ARGUMENT_CLOSE] && state->token == BRACKET_ARGUMENT_CONTENT) {
    if (!close_brackets(state, lexer)) return false;
    lexer->result_symbol = BRACKET_ARGUMENT_CLOSE;
    return true;
  }
  if (valid_symbols[BRACKET_COMMENT_CLOSE] && state->token == BRACKET_COMMENT_CONTENT) {
    if (!close_brackets(state, lexer)) return false;
    lexer->result_symbol = BRACKET_COMMENT_CLOSE;
    return true;
  }
  // Only ASCII CMake layout precedes a new comment or bracket argument.
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n')
    lexer->advance(lexer, true);
  if (valid_symbols[BRACKET_ARGUMENT_OPEN] && lexer->lookahead == '[') {
    if (open_brackets(state, lexer)) {
      state->token = BRACKET_ARGUMENT_OPEN;
      lexer->result_symbol = BRACKET_ARGUMENT_OPEN;
      return true;
    }
    return false;
  }
  if (lexer->lookahead != '#' ||
      (!valid_symbols[BRACKET_COMMENT_OPEN] && !valid_symbols[LINE_COMMENT])) return false;
  advance(lexer);
  if (valid_symbols[BRACKET_COMMENT_OPEN] && open_brackets(state, lexer)) {
    state->token = BRACKET_COMMENT_OPEN;
    lexer->result_symbol = BRACKET_COMMENT_OPEN;
    return true;
  }
  if (!valid_symbols[LINE_COMMENT]) return false;
  while (!lexer->eof(lexer) && lexer->lookahead != '\r' && lexer->lookahead != '\n')
    advance(lexer);
  lexer->mark_end(lexer);
  lexer->result_symbol = LINE_COMMENT;
  return true;
}
