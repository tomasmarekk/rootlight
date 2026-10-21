#define GSED_EXTENDED 0
#include "../common/scanner.h"

void *tree_sitter_gsed_external_scanner_create(void) {
  if (!rootlight_sed_scope_step()) return NULL;
  Scanner *scanner = calloc(1, sizeof(Scanner));
  if (!scanner) rootlight_sed_scope_refuse(ROOTLIGHT_SED_ALLOCATION);
  return scanner;
}
void tree_sitter_gsed_external_scanner_destroy(void *payload) {
  free(payload);
}
unsigned
tree_sitter_gsed_external_scanner_serialize(void *payload, char *buffer) {
  return serialize(payload, buffer);
}
void tree_sitter_gsed_external_scanner_deserialize(
  void *payload,
  const char *buffer,
  unsigned length
) {
  deserialize(payload, buffer, length);
}
bool tree_sitter_gsed_external_scanner_scan(
  void *payload,
  TSLexer *lexer,
  const bool *valid
) {
  return scan(payload, lexer, valid);
}
