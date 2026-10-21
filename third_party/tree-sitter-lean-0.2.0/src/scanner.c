/*
 * External scanner for tree-sitter-lean.
 *
 * Externalized tokens:
 *   1. `_scanned_ident` — dotted, `«quoted»`, or Unicode identifiers.
 *      Plain ASCII single-component idents go through tree-sitter's
 *      internal lexer so word/keyword extraction works.
 *   2. Nested block/doc/module-doc comments.
 *   3. Raw string literals with matched `#`-delimiter counts.
 *   4. Layout-sensitive tokens `_indent`, `_dedent`, `_newline` that
 *      delimit tactic and do-block bodies.
 *
 * Lean's character classifications mirror `src/Init/Meta/Defs.lean`
 * upstream.
 */

#include "tree_sitter/parser.h"
#include "tree_sitter/alloc.h"
#include "rootlight_lean_status.h"
#include <limits.h>

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

enum TokenType {
  SCANNED_IDENT,
  BLOCK_COMMENT,
  DOC_COMMENT,
  MODULE_DOC_COMMENT,
  RAW_STRING_LITERAL,
  INDENT,
  DEDENT,
  NEWLINE,
  NORM_OPEN,
  NORM_CLOSE,
  ERROR_SENTINEL,
};

#define NORM_BAR        0x2016 // ‖
#define SUBSCRIPT_PLUS  0x208a // ₊

#define ID_BEGIN_ESCAPE 0x00ab // «
#define ID_END_ESCAPE   0x00bb // »

static inline bool is_alpha_ascii(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

static inline bool is_digit_ascii(int32_t c) {
  return c >= '0' && c <= '9';
}

static bool is_letter_like(int32_t c) {
  return (0x3b1   <= c && c <= 0x3c9   && c != 0x3bb)
      || (0x391   <= c && c <= 0x3a9   && c != 0x3a0 && c != 0x3a3)
      || (0x3ca   <= c && c <= 0x3fb)
      || (0x1f00  <= c && c <= 0x1ffe)
      || (0x2100  <= c && c <= 0x214f)
      || (0x1d400 <= c && c <= 0x1d7ff)  /* Math alphanumeric symbols (𝟙, 𝒜, 𝔽, …) */
      || (0x00c0  <= c && c <= 0x00ff && c != 0x00d7 && c != 0x00f7)
      || (0x0100  <= c && c <= 0x017f);
}

static bool is_subscript_alnum(int32_t c) {
  return (0x2080 <= c && c <= 0x2089)
      || (0x2090 <= c && c <= 0x209c)
      || (0x1d62 <= c && c <= 0x1d6a)
      || c == 0x2c7c;
}

static bool is_id_first(int32_t c) {
  return is_alpha_ascii(c) || c == '_' || is_letter_like(c);
}

static bool is_id_rest(int32_t c) {
  return is_alpha_ascii(c)
      || is_digit_ascii(c)
      || c == '_' || c == '\'' || c == '!' || c == '?'
      || is_letter_like(c) || is_subscript_alnum(c);
}

/*
 * Indent stack: each entry is the column (in columns, not bytes) of an
 * open tactic/do block. Always contains at least one entry — the base
 * (0). Tokens push and pop based on observed line indentation.
 */
#define LEAN_STATE_CAPACITY 510u
#define LEAN_COMMENT_DEPTH_CAPACITY 1024u
#define LEAN_RAW_HASH_CAPACITY 1024u

struct Scanner {
  uint16_t indents[LEAN_STATE_CAPACITY];
  uint16_t count;
};

_Static_assert(sizeof(struct Scanner) <= 1024, "fixed scanner state exceeds envelope");

static bool valid_current(TSLexer *lexer) {
  if (!rootlight_lean_scope_poll()) return false;
  int32_t value = lexer->lookahead;
  if (value < 0 || value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_INVALID_SCALAR);
    return false;
  }
  if (value == 0 && !lexer->eof(lexer)) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_UNSUPPORTED_NUL);
    return false;
  }
  return true;
}

static bool advance(TSLexer *lexer) {
  if (!valid_current(lexer) || !rootlight_lean_scope_step()) return false;
  lexer->advance(lexer, false);
  return true;
}

static bool skip(TSLexer *lexer) {
  if (!valid_current(lexer) || !rootlight_lean_scope_step()) return false;
  lexer->advance(lexer, true);
  return true;
}

/* ---------- identifier --------------------------------------------------- */

/*
 * Consume a single identifier part: `«…»` or `isIdFirst isIdRest*`.
 * Three out-params are vestigial from an earlier keyword-deferral
 * approach; kept so callers don't need to change shape.
 */
static bool scan_ident_part(TSLexer *lexer,
                            char *buf, size_t buf_size,
                            size_t *out_len, bool *out_is_ascii) {
  (void)buf; (void)buf_size;
  *out_len = 0;
  *out_is_ascii = true;

  int32_t c = lexer->lookahead;
  if (c == ID_BEGIN_ESCAPE) {
    *out_is_ascii = false;
    if (!advance(lexer)) return false;
    while (lexer->lookahead != 0
        && lexer->lookahead != ID_END_ESCAPE
        && lexer->lookahead != '\n') {
      if (!advance(lexer)) return false;
    }
    if (lexer->lookahead != ID_END_ESCAPE) return false;
    if (!advance(lexer)) return false;
    return true;
  }
  if (!is_id_first(c)) return false;

  do {
    if (lexer->lookahead >= 0x80) *out_is_ascii = false;
    if (!advance(lexer)) return false;
  } while (is_id_rest(lexer->lookahead));

  return true;
}

/*
 * Continue scanning an identifier after `len` chars have already been
 * consumed and buffered in `buf`. Used by the dispatch when a leading
 * character (e.g. `r`, `s`, `m`) was eaten to peek for a non-identifier
 * token but turned out to belong to an identifier after all.
 */
/*
 * Continue scanning an identifier after the first character has been
 * consumed. `seen_unusual` tracks whether anything beyond a plain ASCII
 * single-component identifier has been seen — non-ASCII, an escape, or
 * a dot-continuation. If nothing unusual appears, the scanner returns
 * false so tree-sitter's internal lexer (with its keyword-extraction)
 * handles the token instead.
 */
static bool scan_identifier_continue(TSLexer *lexer, bool seen_unusual) {
  while (is_id_rest(lexer->lookahead)) {
    if (lexer->lookahead >= 0x80) seen_unusual = true;
    if (!advance(lexer)) return false;
  }
  lexer->mark_end(lexer);

  while (lexer->lookahead == '.') {
    if (!advance(lexer)) return false;
    int32_t next = lexer->lookahead;
    if (next != ID_BEGIN_ESCAPE && !is_id_first(next)) break;
    seen_unusual = true;
    char dummy_buf[2];
    size_t dummy_len;
    bool dummy_ascii;
    if (!scan_ident_part(lexer, dummy_buf, sizeof(dummy_buf),
                         &dummy_len, &dummy_ascii)) break;
    lexer->mark_end(lexer);
  }

  if (!seen_unusual) return false;
  lexer->result_symbol = SCANNED_IDENT;
  return true;
}

static bool scan_identifier(TSLexer *lexer) {
  int32_t c = lexer->lookahead;
  if (c == ID_BEGIN_ESCAPE) {
    char dummy_buf[2];
    size_t dummy_len;
    bool dummy_ascii;
    if (!scan_ident_part(lexer, dummy_buf, sizeof(dummy_buf),
                         &dummy_len, &dummy_ascii)) return false;
    lexer->mark_end(lexer);
    while (lexer->lookahead == '.') {
      if (!advance(lexer)) return false;
      int32_t next = lexer->lookahead;
      if (next != ID_BEGIN_ESCAPE && !is_id_first(next)) break;
      if (!scan_ident_part(lexer, dummy_buf, sizeof(dummy_buf),
                           &dummy_len, &dummy_ascii)) break;
      lexer->mark_end(lexer);
    }
    lexer->result_symbol = SCANNED_IDENT;
    return true;
  }
  if (!is_id_first(c)) return false;
  bool seen_unusual = (c >= 0x80);
  if (!advance(lexer)) return false;
  return scan_identifier_continue(lexer, seen_unusual);
}

/* ---------- comments ----------------------------------------------------- */

/*
 * Consume the body of a `/- … -/` style comment, honoring nesting.
 * The opening `/-` (or `/--`, `/-!`) must already have been consumed.
 */
static bool scan_block_comment_body(TSLexer *lexer) {
  unsigned depth = 1;
  while (depth > 0) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == '/') {
      if (!advance(lexer)) return false;
      if (lexer->lookahead == '-') {
        if (!advance(lexer)) return false;
        if (depth == LEAN_COMMENT_DEPTH_CAPACITY) {
          rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_CAPACITY);
          return false;
        }
        depth++;
      }
    } else if (lexer->lookahead == '-') {
      if (!advance(lexer)) return false;
      if (lexer->lookahead == '/') {
        if (!advance(lexer)) return false;
        depth--;
      }
    } else {
      if (!advance(lexer)) return false;
    }
  }
  return true;
}

/*
 * Dispatch among `/- … -/`, `/-- … -/`, `/-! … -/` based on the third
 * character after the opening `/-`. The caller has consumed nothing yet.
 */
static bool scan_block_or_doc_comment(TSLexer *lexer,
                                      const bool *valid_symbols) {
  if (lexer->lookahead != '/') return false;
  if (!advance(lexer)) return false;
  if (lexer->lookahead != '-') return false;
  if (!advance(lexer)) return false;

  int kind;
  if (lexer->lookahead == '-' && valid_symbols[DOC_COMMENT]) {
    if (!advance(lexer)) return false;
    /* Avoid eating `/---` style ASCII rulers as doc comments? Lean does
       treat them as doc comments, so accept. */
    kind = DOC_COMMENT;
  } else if (lexer->lookahead == '!' && valid_symbols[MODULE_DOC_COMMENT]) {
    if (!advance(lexer)) return false;
    kind = MODULE_DOC_COMMENT;
  } else if (valid_symbols[BLOCK_COMMENT]) {
    kind = BLOCK_COMMENT;
  } else {
    return false;
  }

  if (!scan_block_comment_body(lexer)) return false;
  lexer->result_symbol = kind;
  return true;
}

/* ---------- raw strings -------------------------------------------------- */

/*
 * Recognize r"…", r#"…"#, r##"…"##, … with matching `#` counts. The
 * leading `r` is assumed already consumed by the dispatcher.
 */
static bool scan_raw_string_after_r(TSLexer *lexer) {
  unsigned hashes = 0;
  while (lexer->lookahead == '#') {
    if (hashes == LEAN_RAW_HASH_CAPACITY) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_CAPACITY); return false; }
    if (!advance(lexer)) return false;
    hashes++;
  }
  if (lexer->lookahead != '"') return false;
  if (!advance(lexer)) return false;

  for (;;) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == '"') {
      if (!advance(lexer)) return false;
      unsigned seen = 0;
      while (seen < hashes && lexer->lookahead == '#') {
        if (!advance(lexer)) return false;
        seen++;
      }
      if (seen == hashes) {
        lexer->result_symbol = RAW_STRING_LITERAL;
        return true;
      }
    } else {
      if (!advance(lexer)) return false;
    }
  }
}

/* ---------- layout (indent / dedent / newline) -------------------------- */

/*
 * If we're sitting on a newline AND any layout token is valid, walk
 * forward to the next non-blank, non-comment line and report what
 * happened to the indent level.
 *
 *   indent > top  → push, emit INDENT
 *   indent < top  → pop one, emit DEDENT (caller will re-invoke us if
 *                   more pops are needed)
 *   indent == top → emit NEWLINE
 *
 * Lines that are entirely blank or contain only a comment don't
 * participate — they're skipped, and we use the indent of the next
 * real content line.
 */
static bool scan_layout(struct Scanner *scanner, TSLexer *lexer,
                        const bool *valid_symbols) {
  if (!(valid_symbols[INDENT]
     || valid_symbols[DEDENT]
     || valid_symbols[NEWLINE])) {
    return false;
  }

  /* Skip trailing spaces/tabs on the current line; we only care about
     layout once we reach a newline. */
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t'
      || lexer->lookahead == '\r') {
    if (!skip(lexer)) return false;
  }

  /* If we're not at a newline (or EOF), no layout token applies. */
  if (lexer->lookahead != '\n' && !lexer->eof(lexer)) return false;

  uint32_t indent = 0;
  bool saw_newline = false;
  for (;;) {
    if (lexer->lookahead == '\n') {
      indent = 0;
      saw_newline = true;
      if (!skip(lexer)) return false;
    } else if (lexer->lookahead == ' ') {
      if (indent == UINT16_MAX) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_UNSUPPORTED_INDENTATION); return false; }
      indent++;
      if (!skip(lexer)) return false;
    } else if (lexer->lookahead == '\t') {
      if (indent > UINT16_MAX - 8u) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_UNSUPPORTED_INDENTATION); return false; }
      indent += 8;
      if (!skip(lexer)) return false;
    } else if (lexer->lookahead == '\r') {
      if (!skip(lexer)) return false;
    } else if (lexer->eof(lexer)) {
      /* EOF closes every open block. */
      if (valid_symbols[DEDENT] && scanner->count > 1) {
        --scanner->count;
        lexer->result_symbol = DEDENT;
        return true;
      }
      return false;
    } else {
      break;
    }
  }
  if (!saw_newline) return false;

  uint16_t top = scanner->count > 0
    ? scanner->indents[scanner->count - 1]
    : 0;

  /* `|` always continues the surrounding rule (match alt, ctor alt,
     pipe-operator) — never terminates a layout block. Suppress
     NEWLINE/DEDENT at the start of a `|`-line so the parser stays
     inside the match/inductive/etc. */
  if (lexer->lookahead == '|') return false;

  if (indent > top) {
    if (valid_symbols[INDENT]) {
      if (scanner->count == LEAN_STATE_CAPACITY) {
        rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_CAPACITY);
        return false;
      }
      scanner->indents[scanner->count++] = (uint16_t)indent;
      lexer->result_symbol = INDENT;
      return true;
    }
    /* Continuation of a multi-line expression: the new line is more
       indented than the surrounding block, so neither INDENT, DEDENT,
       nor NEWLINE applies. Returning nothing lets the parser keep
       consuming tokens as part of the same enclosing rule. */
    return false;
  }
  if (indent < top && valid_symbols[DEDENT]) {
    bool known = false;
    for (uint16_t index = 0; index < scanner->count; ++index) {
      if (!rootlight_lean_scope_step()) return false;
      if (scanner->indents[index] == indent) known = true;
    }
    if (!known) {
      rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_UNSUPPORTED_INDENTATION);
      return false;
    }
    --scanner->count;
    lexer->result_symbol = DEDENT;
    return true;
  }
  if (indent == top && valid_symbols[NEWLINE]) {
    lexer->result_symbol = NEWLINE;
    return true;
  }
  return false;
}

/* ---------- dispatch ----------------------------------------------------- */

static bool scan(struct Scanner *scanner, TSLexer *lexer,
                 const bool *valid_symbols) {
  if (valid_symbols[ERROR_SENTINEL]) return false;

  /* Layout tokens have to be considered before the whitespace skip —
     they're triggered by newlines and leading-line indentation. */
  if (valid_symbols[INDENT] || valid_symbols[DEDENT] || valid_symbols[NEWLINE]) {
    if (scan_layout(scanner, lexer, valid_symbols)) return true;
  }

  /* Skip plain whitespace so the rest of the scanner's view matches
     the lexer's. */
  while (lexer->lookahead == ' '
      || lexer->lookahead == '\t'
      || lexer->lookahead == '\r'
      || lexer->lookahead == '\n') {
    if (!skip(lexer)) return false;
  }

  /* Norm bars `‖ … ‖`. The same glyph opens and closes, disambiguated
     by parser context: prefer CLOSE (only valid once a norm is open and
     its term parsed) so non-nested `‖x‖` closes eagerly; otherwise OPEN.
     A trailing `₊` (non-negative norm `‖x‖₊`) folds into the close. */
  if (lexer->lookahead == NORM_BAR
      && (valid_symbols[NORM_OPEN] || valid_symbols[NORM_CLOSE])) {
    if (!advance(lexer)) return false;
    if (valid_symbols[NORM_CLOSE]) {
      if (lexer->lookahead == SUBSCRIPT_PLUS) if (!advance(lexer)) return false;
      lexer->mark_end(lexer);
      lexer->result_symbol = NORM_CLOSE;
      return true;
    }
    lexer->mark_end(lexer);
    lexer->result_symbol = NORM_OPEN;
    return true;
  }

  if ((valid_symbols[BLOCK_COMMENT]
    || valid_symbols[DOC_COMMENT]
    || valid_symbols[MODULE_DOC_COMMENT])
      && lexer->lookahead == '/') {
    if (scan_block_or_doc_comment(lexer, valid_symbols)) return true;
    return false;
  }

  /* `r` may start either a raw string or an identifier. Both can be
     valid at the same parser position, so we peek past the `r` and
     dispatch based on what follows. If neither path matches, we still
     have to consume the `r` we just ate — fall through as a 1-char
     identifier. */
  if (lexer->lookahead == 'r'
      && (valid_symbols[RAW_STRING_LITERAL] || valid_symbols[SCANNED_IDENT])) {
    if (!advance(lexer)) return false;
    int32_t next = lexer->lookahead;
    if (valid_symbols[RAW_STRING_LITERAL]
        && (next == '"' || next == '#')) {
      return scan_raw_string_after_r(lexer);
    }
    if (valid_symbols[SCANNED_IDENT]) {
      /* Continue an identifier that already has one ASCII char. Plain
         single-component idents are handled by tree-sitter's internal
         lexer, so this path only succeeds if a dot continuation or a
         non-ASCII char shows up. */
      return scan_identifier_continue(lexer, false);
    }
    return false;
  }

  if (valid_symbols[SCANNED_IDENT]
      && (lexer->lookahead == ID_BEGIN_ESCAPE
       || is_id_first(lexer->lookahead))) {
    if (scan_identifier(lexer)) return true;
  }

  return false;
}

/* ---------- tree-sitter ABI --------------------------------------------- */

void *tree_sitter_lean_external_scanner_create(void) {
  if (!rootlight_lean_scope_poll()) return NULL;
  struct Scanner *scanner = ts_calloc(1, sizeof(struct Scanner));
  if (!scanner) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_ALLOCATION); return NULL; }
  scanner->count = 1;
  return scanner;
}

void tree_sitter_lean_external_scanner_destroy(void *payload) {
  ts_free(payload);
}

unsigned tree_sitter_lean_external_scanner_serialize(void *payload, char *buffer) {
  if (!rootlight_lean_scope_step()) return 0;
  if (!payload) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_ALLOCATION); return 0; }
  struct Scanner *scanner = payload;
  if (!buffer || scanner->count == 0 || scanner->count > LEAN_STATE_CAPACITY || scanner->indents[0] != 0) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return 0;
  }
  unsigned length = 4u + 2u * scanner->count;
  if (length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_CAPACITY); return 0; }
  buffer[0] = 'L'; buffer[1] = 1;
  buffer[2] = (char)(scanner->count & 0xffu); buffer[3] = (char)(scanner->count >> 8);
  for (uint16_t index = 0; index < scanner->count; ++index) {
    if (!rootlight_lean_scope_step()) return 0;
    uint16_t value = scanner->indents[index];
    if (index && value <= scanner->indents[index - 1]) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return 0; }
    buffer[4 + 2 * index] = (char)(value & 0xffu);
    buffer[5 + 2 * index] = (char)(value >> 8);
  }
  return length;
}

void tree_sitter_lean_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  if (!rootlight_lean_scope_step()) return;
  if (!payload) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_ALLOCATION); return; }
  struct Scanner temporary = {0};
  temporary.count = 1;
  if (length == 0) { *(struct Scanner *)payload = temporary; return; }
  if (!buffer || length < 6 || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE || buffer[0] != 'L' || buffer[1] != 1) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return;
  }
  uint16_t count = (uint16_t)((uint8_t)buffer[2] | ((uint16_t)(uint8_t)buffer[3] << 8));
  if (count == 0 || count > LEAN_STATE_CAPACITY || length != 4u + 2u * count) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return;
  }
  temporary.count = count;
  for (uint16_t index = 0; index < count; ++index) {
    if (!rootlight_lean_scope_step()) return;
    uint16_t value = (uint16_t)((uint8_t)buffer[4 + 2 * index] | ((uint16_t)(uint8_t)buffer[5 + 2 * index] << 8));
    if ((!index && value != 0) || (index && value <= temporary.indents[index - 1])) {
      rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return;
    }
    temporary.indents[index] = value;
  }
  if (!rootlight_lean_scope_poll()) return;
  *(struct Scanner *)payload = temporary;
}

bool tree_sitter_lean_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  if (!payload) { rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_ALLOCATION); return false; }
  if (!valid_current(lexer) || !rootlight_lean_scope_step()) return false;
  struct Scanner *scanner = payload;
  if (scanner->count == 0 || scanner->count > LEAN_STATE_CAPACITY || scanner->indents[0] != 0) {
    rootlight_lean_scope_refuse(ROOTLIGHT_LEAN_MALFORMED_STATE); return false;
  }
  struct Scanner before = *scanner;
  bool accepted = scan(scanner, lexer, valid_symbols);
  if (!accepted || !rootlight_lean_scope_poll()) { *scanner = before; return false; }
  return true;
}
