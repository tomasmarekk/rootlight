/* Rootlight: HTML tag names use ASCII case folding and preserve other UTF-8.
 * Name/state storage stays inside Tree-sitter's existing serialization ceiling.
 */
#ifndef ROOTLIGHT_HTML_NAME_H
#define ROOTLIGHT_HTML_NAME_H

static bool html_space(int32_t value) {
    return value == ' ' || value == '\t' || value == '\r' || value == '\n' || value == '\f';
}

static int32_t ascii_upper(int32_t value) {
    return value >= 'a' && value <= 'z' ? value - ('a' - 'A') : value;
}

static bool append_name(String *name, int32_t codepoint) {
    if (codepoint <= 0 || codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff)) return false;
    uint32_t value = (uint32_t)ascii_upper(codepoint);
    unsigned width = value < 0x80 ? 1 : value < 0x800 ? 2 : value < 0x10000 ? 3 : 4;
    if (name->size > HTML_MAX_NAME_BYTES - width) return false;
    /* These casts intentionally store UTF-8 code units, not truncated scalars. */
    if (width == 1) array_push(name, (char)value);
    else {
        array_push(name, (char)((width == 2 ? 0xc0 : width == 3 ? 0xe0 : 0xf0) | (value >> (6 * (width - 1)))));
        for (unsigned remaining = width - 1; remaining > 0; remaining--)
            array_push(name, (char)(0x80 | ((value >> (6 * (remaining - 1))) & 0x3f)));
    }
    return true;
}

static String scan_tag_name(TSLexer *lexer) {
    String name = array_new();
    int32_t first = ascii_upper(lexer->lookahead);
    if (first < 'A' || first > 'Z') return name;
    while (lexer->lookahead && !html_space(lexer->lookahead) &&
           lexer->lookahead != '/' && lexer->lookahead != '>' && lexer->lookahead != '<') {
        if (!append_name(&name, lexer->lookahead)) {
            array_delete(&name);
            return (String)array_new();
        }
        advance(lexer);
    }
    return name;
}

#endif
