/* Complete bounded Astro snapshots preserve component identity and nesting.
 * The private format is versioned; malformed frames discard all prior context.
 */
#ifndef ROOTLIGHT_ASTRO_STATE_H
#define ROOTLIGHT_ASTRO_STATE_H

#define ASTRO_STATE_HEADER 3u
#define ASTRO_MAX_NAME_BYTES (TREE_SITTER_SERIALIZATION_BUFFER_SIZE - ASTRO_STATE_HEADER - 3u)

typedef struct {
    Array(Tag) tags;
    unsigned serialized_bytes;
} Scanner;

static unsigned tag_size(const Tag *tag) {
    return 1u + (tag->type == CUSTOM ? 2u + tag->custom_tag_name.size :
        tag->type == INTERPOLATION ? 2u + tag->js_state.size : 0u);
}

/* Admission owns the tag even on failure, and cannot publish a truncated stack. */
static bool push_tag(Scanner *scanner, Tag tag) {
    unsigned bytes = tag_size(&tag);
    if (bytes > TREE_SITTER_SERIALIZATION_BUFFER_SIZE - scanner->serialized_bytes) {
        tag_free(&tag);
        return false;
    }
    array_push(&scanner->tags, tag);
    scanner->serialized_bytes += bytes;
    return true;
}

static unsigned read_u16(const char *buffer) {
    return (uint8_t)buffer[0] | ((unsigned)(uint8_t)buffer[1] << 8);
}

static void write_u16(char *buffer, unsigned value) {
    buffer[0] = (char)(value & 255u);
    buffer[1] = (char)((value >> 8) & 255u);
}

static unsigned js_saved_size(const JsContext *context) {
    return 7u + context->depth + context->brace_depth;
}

static void encode_js(const JsContext *context, char *buffer) {
    buffer[0] = context->regex_allowed | (context->property_name << 1) |
        (context->control_parenthesis << 2) | (context->statement_start << 3);
    buffer[1] = (char)context->function_parenthesis;
    buffer[2] = (char)context->next_brace;
    write_u16(buffer + 3, context->depth);
    write_u16(buffer + 5, context->brace_depth);
    memcpy(buffer + 7, context->parens, context->depth);
    for (unsigned i = 0; i < context->brace_depth; i++) buffer[7 + context->depth + i] = context->braces[i];
}

static bool restore_js(JsContext *context, const char *buffer, unsigned length) {
    if (length < 7 || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) return false;
    unsigned flags = (uint8_t)buffer[0];
    unsigned function = (uint8_t)buffer[1];
    unsigned brace = (uint8_t)buffer[2];
    unsigned depth = read_u16(buffer + 3);
    unsigned brace_depth = read_u16(buffer + 5);
    if (flags > 15 || (function != 0 && function != 3 && function != 4) || brace > 2 ||
        depth > ASTRO_JS_DEPTH || brace_depth > ASTRO_JS_DEPTH || 7 + depth + brace_depth != length) return false;
    for (unsigned i = 0; i < depth; i++) {
        unsigned kind = (uint8_t)buffer[7 + i];
        if (kind != 0 && kind != 1 && kind != 3 && kind != 4) return false;
    }
    for (unsigned i = 0; i < brace_depth; i++) if ((uint8_t)buffer[7 + depth + i] > 1) return false;
    context->regex_allowed = flags & 1;
    context->property_name = flags & 2;
    context->control_parenthesis = flags & 4;
    context->statement_start = flags & 8;
    context->function_parenthesis = (unsigned char)function;
    context->next_brace = (unsigned char)brace;
    context->depth = depth;
    context->brace_depth = brace_depth;
    memcpy(context->parens, buffer + 7, depth);
    for (unsigned i = 0; i < brace_depth; i++) context->braces[i] = buffer[7 + depth + i] != 0;
    return true;
}

static bool save_js(Scanner *scanner, const JsContext *context) {
    Tag *tag = array_back(&scanner->tags);
    unsigned bytes = js_saved_size(context);
    unsigned retained = scanner->serialized_bytes - tag->js_state.size;
    if (bytes > TREE_SITTER_SERIALIZATION_BUFFER_SIZE - retained) return false;
    array_reserve(&tag->js_state, bytes);
    tag->js_state.size = bytes;
    encode_js(context, tag->js_state.contents);
    scanner->serialized_bytes = retained + bytes;
    return true;
}

static unsigned serialize(Scanner *scanner, char *buffer) {
    buffer[0] = 2;
    write_u16(buffer + 1, scanner->tags.size);
    unsigned size = ASTRO_STATE_HEADER;
    for (unsigned i = 0; i < scanner->tags.size; i++) {
        const Tag *tag = &scanner->tags.contents[i];
        buffer[size++] = (char)tag->type;
        if (tag->type == CUSTOM || tag->type == INTERPOLATION) {
            const String *payload = tag->type == CUSTOM ? &tag->custom_tag_name : &tag->js_state;
            unsigned length = payload->size;
            write_u16(buffer + size, length);
            size += 2;
            memcpy(buffer + size, payload->contents, length);
            size += length;
        }
    }
    return size;
}

static void deserialize(Scanner *scanner, const char *buffer, unsigned length) {
    for (unsigned i = 0; i < scanner->tags.size; i++) tag_free(&scanner->tags.contents[i]);
    array_clear(&scanner->tags);
    scanner->serialized_bytes = ASTRO_STATE_HEADER;
    if (!buffer || length < ASTRO_STATE_HEADER ||
        length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE || buffer[0] != 2) return;
    unsigned count = read_u16(buffer + 1);
    unsigned position = ASTRO_STATE_HEADER;
    /* Validate the entire frame before allocating or exposing any restored tag. */
    for (unsigned i = 0; i < count; i++) {
        if (position >= length) return;
        unsigned type = (uint8_t)buffer[position++];
        if (type >= END_ || type == END_OF_VOID_TAGS) return;
        if (type == CUSTOM || type == INTERPOLATION) {
            if (length - position < 2) return;
            unsigned bytes = read_u16(buffer + position);
            position += 2;
            if (bytes == 0 || bytes > length - position) return;
            if (type == INTERPOLATION) {
                JsContext context = {0};
                if (!restore_js(&context, buffer + position, bytes)) return;
            }
            position += bytes;
        }
    }
    if (position != length) return;
    array_reserve(&scanner->tags, count);
    position = ASTRO_STATE_HEADER;
    for (unsigned i = 0; i < count; i++) {
        Tag tag = tag_new();
        tag.type = (TagType)(uint8_t)buffer[position++];
        if (tag.type == CUSTOM || tag.type == INTERPOLATION) {
            unsigned bytes = read_u16(buffer + position);
            position += 2;
            String *payload = tag.type == CUSTOM ? &tag.custom_tag_name : &tag.js_state;
            array_reserve(payload, bytes);
            memcpy(payload->contents, buffer + position, bytes);
            payload->size = bytes;
            position += bytes;
        }
        array_push(&scanner->tags, tag);
    }
    scanner->serialized_bytes = length;
}

#endif
