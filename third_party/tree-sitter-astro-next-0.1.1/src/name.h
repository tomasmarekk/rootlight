/* Astro names retain case and UTF-8 rather than locale-dependent byte casts.
 * Storage is bounded by the existing complete scanner snapshot capacity.
 */
#ifndef ROOTLIGHT_ASTRO_NAME_H
#define ROOTLIGHT_ASTRO_NAME_H

static bool astro_space(int32_t value) {
    return value == ' ' || value == '\t' || value == '\r' || value == '\n' || value == '\f';
}

static bool append_name(String *name, int32_t codepoint) {
    if (codepoint <= 0 || codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff)) return false;
    uint32_t value = (uint32_t)codepoint;
    unsigned width = value < 0x80 ? 1 : value < 0x800 ? 2 : value < 0x10000 ? 3 : 4;
    if (name->size > ASTRO_MAX_NAME_BYTES - width) return false;
    /* These casts store encoded UTF-8 code units, never truncated scalars. */
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
    if (!IS_ASCII_ALPHA(lexer->lookahead)) return name;
    while (lexer->lookahead && !astro_space(lexer->lookahead) &&
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
