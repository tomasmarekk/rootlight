/* Lexical context keeps JavaScript regex bodies opaque to Astro delimiters.
 * This finds source boundaries; embedded parsing still validates JavaScript syntax.
 */
#ifndef ROOTLIGHT_ASTRO_JAVASCRIPT_H
#define ROOTLIGHT_ASTRO_JAVASCRIPT_H

#define ASTRO_JS_DEPTH 4096u

typedef struct {
    bool regex_allowed;
    bool property_name;
    bool control_parenthesis;
    bool statement_start;
    unsigned char function_parenthesis;
    unsigned char next_brace;
    unsigned char parens[ASTRO_JS_DEPTH];
    bool braces[ASTRO_JS_DEPTH];
    unsigned depth;
    unsigned brace_depth;
} JsContext;

static bool js_line_end(int32_t value) {
    return value == '\n' || value == '\r' || value == 0x2028 || value == 0x2029;
}

static bool js_space(int32_t value) {
    return js_line_end(value) || value == '\t' || value == '\v' || value == '\f' ||
        value == ' ' || value == 0xa0 || value == 0xfeff || value == 0x1680 ||
        (value >= 0x2000 && value <= 0x200a) || value == 0x202f || value == 0x205f || value == 0x3000;
}

static bool js_identifier(int32_t value) {
    return IS_ASCII_ALPHA(value) || (value >= '0' && value <= '9') ||
        value == '_' || value == '$' || value == '\\' || (value >= 0x80 && !js_space(value));
}

static void js_literal(JsContext *context) {
    context->regex_allowed = false;
    context->property_name = false;
    context->control_parenthesis = false;
    context->statement_start = false;
}

static void js_word(TSLexer *lexer, JsContext *context) {
    char word[16] = {0};
    unsigned length = 0;
    bool too_long = false;
    while (js_identifier(lexer->lookahead)) {
        if (length < sizeof(word) - 1 && lexer->lookahead < 0x80) word[length++] = (char)lexer->lookahead;
        else too_long = true;
        advance(lexer);
    }
    bool property = context->property_name;
    bool statement = context->statement_start;
    js_literal(context);
    if (property || too_long) return;
    context->control_parenthesis = !strcmp(word, "if") || !strcmp(word, "while") ||
        !strcmp(word, "for") || !strcmp(word, "with") || !strcmp(word, "switch") || !strcmp(word, "catch");
    context->regex_allowed = context->control_parenthesis ||
        !strcmp(word, "return") || !strcmp(word, "throw") || !strcmp(word, "case") ||
        !strcmp(word, "delete") || !strcmp(word, "void") || !strcmp(word, "typeof") ||
        !strcmp(word, "yield") || !strcmp(word, "await") || !strcmp(word, "new") ||
        !strcmp(word, "in") || !strcmp(word, "instanceof") ||
        !strcmp(word, "else") || !strcmp(word, "do");
    if (!strcmp(word, "function")) context->function_parenthesis = statement ? 3 : 4;
    if (!strcmp(word, "else") || !strcmp(word, "do") || !strcmp(word, "try") || !strcmp(word, "finally")) {
        context->next_brace = 1;
        context->statement_start = true;
        context->regex_allowed = true;
    }
}

/* The opening slash has already been consumed, and comment prefixes excluded. */
static bool scan_js_regex_body(TSLexer *lexer) {
    bool character_class = false;
    while (!lexer->eof(lexer)) {
        int32_t value = lexer->lookahead;
        if (js_line_end(value)) return false;
        advance(lexer);
        if (value == '\\') {
            if (lexer->eof(lexer) || js_line_end(lexer->lookahead)) return false;
            advance(lexer);
        } else if (value == '[') character_class = true;
        else if (value == ']') character_class = false;
        else if (value == '/' && !character_class) {
            while (js_identifier(lexer->lookahead)) advance(lexer);
            return true;
        }
    }
    return false;
}

static bool js_token(TSLexer *lexer, JsContext *context) {
    int32_t value = lexer->lookahead;
    if (js_space(value)) { advance(lexer); return true; }
    if (js_identifier(value)) { js_word(lexer, context); return true; }
    bool was_operand = context->regex_allowed;
    bool control = context->control_parenthesis;
    bool statement = context->statement_start;
    context->control_parenthesis = false;
    context->property_name = false;
    context->statement_start = false;
    advance(lexer);
    if (value == '(') {
        if (context->depth + context->brace_depth == ASTRO_JS_DEPTH) return false;
        context->parens[context->depth++] = context->function_parenthesis ? context->function_parenthesis : control;
        context->function_parenthesis = 0;
        context->regex_allowed = true;
    } else if (value == ')') {
        unsigned kind = context->depth > 0 ? context->parens[--context->depth] : 0;
        context->regex_allowed = kind == 1;
        context->next_brace = kind == 1 || kind == 3 ? 1 : kind == 4 ? 2 : 0;
        context->statement_start = kind == 1;
    } else if (value == '{') {
        if (context->depth + context->brace_depth == ASTRO_JS_DEPTH) return false;
        context->braces[context->brace_depth++] = context->next_brace == 1 || (!context->next_brace && statement);
        context->statement_start = context->next_brace != 0 || statement;
        context->next_brace = 0;
        context->regex_allowed = true;
    } else if (value == '}') {
        context->regex_allowed = context->brace_depth > 0 && context->braces[--context->brace_depth];
        context->statement_start = context->regex_allowed;
    } else if (value == ']') context->regex_allowed = false;
    else if (value == '.') {
        if (lexer->lookahead == '.') {
            advance(lexer);
            if (lexer->lookahead != '.') return false;
            advance(lexer);
            context->regex_allowed = true;
        } else {
            context->regex_allowed = false;
            context->property_name = true;
        }
    } else if (value == '=' && lexer->lookahead == '>') {
        advance(lexer);
        context->next_brace = 2;
        context->regex_allowed = true;
    } else if (value == ';') {
        context->regex_allowed = true;
        context->statement_start = true;
    } else if ((value == '+' || value == '-') && lexer->lookahead == value) {
        advance(lexer);
        context->regex_allowed = was_operand;
    } else context->regex_allowed = true;
    return true;
}

#endif
