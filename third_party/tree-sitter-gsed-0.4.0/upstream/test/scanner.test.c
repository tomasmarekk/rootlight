#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>

#include "../common/scanner.h"

typedef struct {
  TSLexer lexer;
  const char *source;
  size_t size;
  size_t offset;
  size_t end;
} Input;

static void input_advance(TSLexer *lexer, bool skip) {
  (void)skip;
  Input *input = (Input *)lexer;
  if (input->offset < input->size) input->offset++;
  lexer->lookahead = input->offset < input->size
    ? (unsigned char)input->source[input->offset]
    : 0;
}

static void input_mark(TSLexer *lexer) {
  Input *input = (Input *)lexer;
  input->end = input->offset;
}

static bool input_eof(const TSLexer *lexer) {
  const Input *input = (const Input *)lexer;
  return input->offset == input->size;
}

static Input input_for(const char *source, size_t length) {
  Input input = {0};
  input.source = source;
  input.size = length;
  input.lexer.advance = input_advance;
  input.lexer.mark_end = input_mark;
  input.lexer.eof = input_eof;
  input.lexer.lookahead = length ? (unsigned char)source[0] : 0;
  return input;
}

static void rewind_to_end(Input *input) {
  input->offset = input->end;
  input->lexer.lookahead = input->offset < input->size
    ? (unsigned char)input->source[input->offset]
    : 0;
}

static void check_regex_restoration(
  const char *source,
  const enum TokenType *expected,
  size_t count
) {
  Scanner scanner = {0};
  Input input = input_for(source, strlen(source));
  bool valid[TOKEN_COUNT] = {false};
  valid[PATTERN_START] = true;
  assert(scan(&scanner, &input.lexer, valid));
  assert(input.lexer.result_symbol == PATTERN_START);
  rewind_to_end(&input);
  for (size_t i = 0; i < sizeof(valid) / sizeof(valid[0]); i++) valid[i] = true;
  valid[ERROR_SENTINEL] = false;
  for (size_t i = 0; i < count; i++) {
    char state[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    unsigned length = serialize(&scanner, state);
    assert(length <= sizeof(state));
    Scanner restored = {0};
    deserialize(&restored, state, length);
    Input replay = input;
    assert(scan(&scanner, &input.lexer, valid));
    assert(scan(&restored, &replay.lexer, valid));
    assert(input.lexer.result_symbol == expected[i]);
    assert(replay.lexer.result_symbol == input.lexer.result_symbol);
    assert(replay.offset == input.offset);
    assert(replay.end == input.end);
    char replay_state[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    char next_state[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    unsigned next_length = serialize(&scanner, next_state);
    unsigned replay_length = serialize(&restored, replay_state);
    assert(next_length <= sizeof(next_state));
    assert(replay_length <= sizeof(replay_state));
    assert(next_length == replay_length);
    assert(memcmp(next_state, replay_state, next_length) == 0);
    rewind_to_end(&input);
  }
  assert(input.offset == input.size);
}

static void test_regex_state_restoration(void) {
  const enum TokenType bracket[] = {
    BRACKET_OPEN_ENCODED,
    BRACKET_CHARACTER,
    BRACKET_CLOSE_ENCODED,
    QUANTIFIER,
    PATTERN_END
  };
  check_regex_restoration(
    "/\\x5ba\\x5d*/",
    bracket,
    sizeof(bracket) / sizeof(bracket[0])
  );
  const enum TokenType host_bracket[] = {
    BRACKET_OPEN,
    BRACKET_CHARACTER,
    BRACKET_CLOSE_ENCODED,
    REGEX_CHARACTER,
    REGEX_CHARACTER,
    PATTERN_END
  };
  check_regex_restoration(
    "/[a\\x5d/]/",
    host_bracket,
    sizeof(host_bracket) / sizeof(host_bracket[0])
  );
  const enum TokenType control_bracket[] = {
    REGEX_CHARACTER_ESCAPE,
    PATTERN_END
  };
  check_regex_restoration(
    "[\\c[[",
    control_bracket,
    sizeof(control_bracket) / sizeof(control_bracket[0])
  );
  const enum TokenType misplaced[] = {
    BRACKET_OPEN,
    MISPLACED_CLASS_START,
    TERM_TEXT,
    TERM_CLOSE_MARKER,
    BRACKET_CLOSE,
    PATTERN_END
  };
  check_regex_restoration(
    "/[:alpha:]/",
    misplaced,
    sizeof(misplaced) / sizeof(misplaced[0])
  );
  const enum TokenType utf8[] =
    {UTF8_START, UTF8_CONTINUATION, QUANTIFIER, PATTERN_END};
  check_regex_restoration(
    "/\\xce\\xa3*/",
    utf8,
    sizeof(utf8) / sizeof(utf8[0])
  );
  const enum TokenType group[] =
    {GROUP_OPEN, REGEX_CHARACTER, GROUP_CLOSE, PATTERN_END};
  check_regex_restoration(
    GSED_EXTENDED ? "/(a)/" : "/\\(a\\)/",
    group,
    sizeof(group) / sizeof(group[0])
  );
  const enum TokenType lookahead[] =
    {REGEX_ANCHOR, ALTERNATION, REGEX_CHARACTER, PATTERN_END};
  check_regex_restoration(
    GSED_EXTENDED ? "/$|a/" : "/$\\|a/",
    lookahead,
    sizeof(lookahead) / sizeof(lookahead[0])
  );
  const enum TokenType paired_backslash[] = {
    REGEX_CHARACTER_ESCAPE,
    REGEX_ESCAPE,
    REGEX_CHARACTER,
    REGEX_CHARACTER,
    PATTERN_END
  };
  check_regex_restoration(
    "/\\x5c\\\\x5e/",
    paired_backslash,
    sizeof(paired_backslash) / sizeof(paired_backslash[0])
  );
  const enum TokenType control_pair[] =
    {REGEX_CHARACTER_ESCAPE, REGEX_DOT, PATTERN_END};
  check_regex_restoration(
    "/\\c\\\\./",
    control_pair,
    sizeof(control_pair) / sizeof(control_pair[0])
  );
  const enum TokenType control_delimiter[] =
    {REGEX_ESCAPE, REGEX_ESCAPE, PATTERN_END};
  check_regex_restoration(
    "c\\c\\.c",
    control_delimiter,
    sizeof(control_delimiter) / sizeof(control_delimiter[0])
  );
  const enum TokenType boundary_interval[] = {
    REGEX_CHARACTER,
    REGEX_BOUNDARY,
    GSED_EXTENDED ? REGEX_ESCAPE : INVALID_INTERVAL_SYNTAX,
    PATTERN_END
  };
  check_regex_restoration(
    "/a\\b\\{/",
    boundary_interval,
    sizeof(boundary_interval) / sizeof(boundary_interval[0])
  );
  const enum TokenType escaped_digit[] = {REGEX_CHARACTER_ESCAPE, PATTERN_END};
  check_regex_restoration(
    "5\\x\\55",
    escaped_digit,
    sizeof(escaped_digit) / sizeof(escaped_digit[0])
  );
  const enum TokenType unused_backslash[] =
    {REGEX_CHARACTER_ESCAPE, REGEX_BACKREFERENCE, PATTERN_END};
  check_regex_restoration(
    "5\\x6\\65",
    unused_backslash,
    sizeof(unused_backslash) / sizeof(unused_backslash[0])
  );
  const enum TokenType escaped_bracket[] = {
    BRACKET_OPEN_ENCODED,
    BRACKET_CHARACTER,
    BRACKET_CLOSE_ENCODED,
    PATTERN_END
  };
  check_regex_restoration(
    "5\\x\\5ba\\x\\5d5",
    escaped_bracket,
    sizeof(escaped_bracket) / sizeof(escaped_bracket[0])
  );
  const enum TokenType backslash_delimiter[] = {REGEX_CHARACTER, PATTERN_END};
  check_regex_restoration(
    "\\a\\",
    backslash_delimiter,
    sizeof(backslash_delimiter) / sizeof(backslash_delimiter[0])
  );
}

static Scanner
scan_source(const char *source, enum TokenType start, enum Mode mode) {
  Scanner scanner = {0};
  Input input = input_for(source, strlen(source));
  bool valid[TOKEN_COUNT] = {false};
  valid[start] = true;
  assert(scan(&scanner, &input.lexer, valid));
  assert(input.lexer.result_symbol == start);
  rewind_to_end(&input);
  for (size_t i = 0; i < sizeof(valid) / sizeof(valid[0]); i++) valid[i] = true;
  valid[ERROR_SENTINEL] = false;
  while (scanner.mode != mode || input.offset < input.size) {
    assert(scan(&scanner, &input.lexer, valid));
    rewind_to_end(&input);
  }
  return scanner;
}

static void check_same_state(Scanner actual, Scanner expected) {
  char state[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
  char expected_state[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
  unsigned length = serialize(&actual, state);
  assert(length == serialize(&expected, expected_state));
  assert(memcmp(state, expected_state, length) == 0);
}

static void test_construct_end_resets_state(void) {
  check_same_state(
    scan_source("/a/", REGEX_START, NORMAL),
    (Scanner){.mode = NORMAL}
  );
  check_same_state(
    scan_source("/a/b/", PATTERN_START, FLAGS),
    (Scanner){.mode = FLAGS}
  );
  check_same_state(
    scan_source("/a/b/", TRANSLATION_START, NORMAL),
    (Scanner){.mode = NORMAL}
  );
  check_same_state(
    scan_source("/a/", TRANSLATION_START, NORMAL),
    (Scanner){.mode = NORMAL}
  );
  check_same_state(
    scan_source(GSED_EXTENDED ? "/a{1}" : "/a\\{1\\}", REGEX_START, ADDRESS),
    scan_source("/a", REGEX_START, ADDRESS)
  );
}

static void test_nul_is_a_regex_character(void) {
  const char source[] = "/\0/";
  const struct {
    enum TokenType token;
    size_t end;
  } expected[] = {{PATTERN_START, 1}, {REGEX_CHARACTER, 2}, {PATTERN_END, 3}};
  Scanner scanner = {0};
  Input input = input_for(source, sizeof(source) - 1);
  for (size_t i = 0; i < sizeof(expected) / sizeof(expected[0]); i++) {
    bool valid[TOKEN_COUNT] = {false};
    valid[expected[i].token] = true;
    assert(scan(&scanner, &input.lexer, valid));
    assert(input.lexer.result_symbol == expected[i].token);
    assert(input.offset == expected[i].end);
    rewind_to_end(&input);
  }
  assert(input.offset == input.size);
}

int main(void) {
  test_regex_state_restoration();
  test_construct_end_resets_state();
  test_nul_is_a_regex_character();
  return 0;
}
