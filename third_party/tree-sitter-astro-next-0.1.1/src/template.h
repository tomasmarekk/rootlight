/* Iterative template scanning avoids input-dependent native call-stack growth.
 * Overflow or unterminated input fails the token instead of publishing a prefix.
 */
#ifndef ROOTLIGHT_ASTRO_TEMPLATE_H
#define ROOTLIGHT_ASTRO_TEMPLATE_H

/* Matches the adapter runtime's existing hard syntax-depth ceiling. This is
 * local scratch storage, not a new global resource allowance or persisted state.
 */
#define ASTRO_TEMPLATE_DEPTH 4096u

static bool scan_js_quoted_string(TSLexer *lexer) {
    int32_t quote = lexer->lookahead;
    advance(lexer);
    while (!lexer->eof(lexer)) {
        int32_t value = lexer->lookahead;
        advance(lexer);
        if (value == quote) return true;
        if (value == '\\') {
            if (lexer->eof(lexer)) return false;
            advance(lexer);
        }
    }
    return false;
}

static bool scan_js_backtick_string(TSLexer *lexer) {
    unsigned char frames[ASTRO_TEMPLATE_DEPTH];
    unsigned depth = 1;
    JsContext context = {.regex_allowed = true};
    frames[0] = '`';
    advance(lexer);
    while (!lexer->eof(lexer)) {
        int32_t value = lexer->lookahead;
        if (frames[depth - 1] == '`') {
            advance(lexer);
            if (value == '\\') {
                if (lexer->eof(lexer)) return false;
                advance(lexer);
            } else if (value == '`') {
                if (--depth == 0) return true;
                js_literal(&context);
            } else if (value == '$' && lexer->lookahead == '{') {
                if (depth == ASTRO_TEMPLATE_DEPTH) return false;
                frames[depth++] = '}';
                context.regex_allowed = true;
                context.property_name = false;
                context.control_parenthesis = false;
                advance(lexer);
            }
            continue;
        }
        if (value == '\'' || value == '"') {
            if (!scan_js_quoted_string(lexer)) return false;
            js_literal(&context);
            continue;
        }
        if (value == '`' || value == '{') {
            if (depth == ASTRO_TEMPLATE_DEPTH) return false;
            frames[depth++] = value == '`' ? '`' : 'B';
            if (value == '{') {
                if (!js_token(lexer, &context)) return false;
            } else advance(lexer);
            continue;
        }
        if (value == '}') {
            bool brace = frames[depth - 1] == 'B';
            depth--;
            if (brace) {
                if (!js_token(lexer, &context)) return false;
            } else {
                js_literal(&context);
                advance(lexer);
            }
            continue;
        }
        if (value == '/') {
            advance(lexer);
            if (lexer->lookahead == '/') {
                while (!lexer->eof(lexer) && !js_line_end(lexer->lookahead)) advance(lexer);
            } else if (lexer->lookahead == '*') {
                advance(lexer);
                bool closed = false;
                while (!lexer->eof(lexer)) {
                    value = lexer->lookahead;
                    advance(lexer);
                    if (value == '*' && lexer->lookahead == '/') {
                        advance(lexer);
                        closed = true;
                        break;
                    }
                }
                if (!closed) return false;
            } else if (context.regex_allowed) {
                if (!scan_js_regex_body(lexer)) return false;
                js_literal(&context);
            } else {
                context.regex_allowed = true;
                if (lexer->lookahead == '=') advance(lexer);
            }
            continue;
        }
        if (!js_token(lexer, &context)) return false;
    }
    return false;
}

static bool scan_js_string(TSLexer *lexer) {
    return lexer->lookahead == '`' ? scan_js_backtick_string(lexer) : scan_js_quoted_string(lexer);
}

#endif
