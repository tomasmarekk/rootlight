/* Native Nim scanner state controls. Valid scan-created layout states and
 * malformed ABI input are distinct cases; canaries make copy bounds observable. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../src/scanner.c"

static unsigned failures;
#define CHECK(value) do { if (!(value)) { fprintf(stderr, "failed line %d: %s\n", __LINE__, #value); failures++; } } while (0)
struct input { TSLexer lexer; const char *text; size_t position; };
static void advance(TSLexer *lexer, bool skip) {
  (void)skip; struct input *input = (struct input *)lexer;
  if (input->text[input->position]) input->position++;
  lexer->lookahead = (unsigned char)input->text[input->position];
}
static void mark(TSLexer *lexer) { (void)lexer; }
static uint32_t column(TSLexer *lexer) {
  struct input *input = (struct input *)lexer; size_t start = input->position;
  while (start && input->text[start - 1] != '\n') start--;
  return (uint32_t)(input->position - start);
}
static bool included(const TSLexer *lexer) { (void)lexer; return false; }
static bool eof(const TSLexer *lexer) { return lexer->lookahead == 0; }
static struct input input(const char *text) {
  struct input result = {.text = text};
  result.lexer.advance = advance; result.lexer.mark_end = mark;
  result.lexer.get_column = column; result.lexer.is_at_included_range_start = included;
  result.lexer.eof = eof; result.lexer.lookahead = (unsigned char)text[0]; return result;
}
static void bounded_serialization(void) {
  void *state = tree_sitter_nim_external_scanner_create(); CHECK(state != NULL);
  uint8_t invalid[1025]; memset(invalid, 1, sizeof invalid);
  tree_sitter_nim_external_scanner_deserialize(state, invalid, sizeof invalid);
  uint8_t buffer[2048]; memset(buffer, 0xa5, sizeof buffer);
  unsigned length = tree_sitter_nim_external_scanner_serialize(state, buffer);
  CHECK(length <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE);
  for (size_t i = TREE_SITTER_SERIALIZATION_BUFFER_SIZE; i < sizeof buffer; i++) CHECK(buffer[i] == 0xa5);
  tree_sitter_nim_external_scanner_destroy(state);
}
static void scan_created_states(void) {
  void *state = tree_sitter_nim_external_scanner_create();
  bool valid[TOKEN_TYPE_LEN] = {0}; valid[SYNCHRONIZE] = true;
  struct input first = input("x");
  CHECK(tree_sitter_nim_external_scanner_scan(state, &first.lexer, valid));
  CHECK(first.lexer.result_symbol == SYNCHRONIZE);
  memset(valid, 0, sizeof valid); valid[LAYOUT_START] = true;
  for (unsigned i = 1; i < 255; i++) {
    char text[258] = {'\n'}; memset(text + 1, ' ', i); text[i + 1] = 'x'; text[i + 2] = 0;
    struct input line = input(text);
    CHECK(tree_sitter_nim_external_scanner_scan(state, &line.lexer, valid));
    CHECK(line.lexer.result_symbol == LAYOUT_START);
  }
  uint8_t buffer[1024]; unsigned length = tree_sitter_nim_external_scanner_serialize(state, buffer);
  CHECK(length == 255);
  for (unsigned i = 0; i < length; i++) CHECK(buffer[i] == i);
  void *copy = tree_sitter_nim_external_scanner_create();
  tree_sitter_nim_external_scanner_deserialize(copy, buffer, length);
  uint8_t other[1024]; CHECK(tree_sitter_nim_external_scanner_serialize(copy, other) == length);
  CHECK(memcmp(buffer, other, length) == 0);
  tree_sitter_nim_external_scanner_deserialize(copy, NULL, 0);
  CHECK(tree_sitter_nim_external_scanner_serialize(copy, other) == 0);
  tree_sitter_nim_external_scanner_destroy(copy); tree_sitter_nim_external_scanner_destroy(state);
}
int main(int argc, char **argv) {
  bounded_serialization(); scan_created_states();
  if (argc > 1 && strcmp(argv[1], "--verify-check-failure") == 0) CHECK(false);
  printf("failures=%u\n", failures); return failures ? 1 : 0;
}
