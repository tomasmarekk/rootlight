# Native MATLAB grammar provenance

This directory contains the MIT-licensed published `tree-sitter-matlab` 1.3.0
crate from `acristoffers/tree-sitter-matlab`, revision
`c892c8530e70e4a96e0b130c820e8989925fb9a1`. The original crate SHA-256 is
`8e8d8831a78547f54860dd8467166b1ab0c466d03d43c4bb447af55c024281f7`.

## Scanner restoration

Deserialization resets all five fields before accepting a complete frame. An
empty Tree-sitter reset must not retain command, continuation, shell, string,
or pending-delimiter context from an earlier parse. Boolean bytes must be
canonical and the string delimiter must be absent, a single quote, or a double
quote; malformed frames retain the fresh state instead of partial context.

The isolated `matlab_state` tests cover all flag combinations, both quote modes,
empty and truncated frames, invalid field values, serialization canaries, and
repeated non-consuming serialization. The empty-frame regression fails against
the published scanner. The grammar and generated parser are unchanged.

## CRLF continuations

The whitespace helper consumes CRLF as one line ending after `...`. Consuming
only the carriage return leaves a newline that incorrectly terminates the
continued expression. This backports the helper from upstream revision
`f03d0347acd8bb05d4edd8c845ac1718729e1fad`. Native tests cover LF and CRLF in
full function fixtures and incremental edits without normalizing source bytes.

## Bounded scanner storage

Consecutive comment merging uses an iterative restart instead of one recursive
call per comment line. An isolated child-process regression parses 8,000 comment
lines on a 128 KiB thread stack; the published recursion overflows that stack.

Identifier scanning consumes the entire token but stores at most 255 bytes plus
a NUL terminator in the existing 256-byte keyword-comparison buffer. Previously
an exactly 256-byte token overwrote the last zero, and longer tokens could be
split by error recovery. The prefix remains much longer than every keyword, so
this does not change keyword recognition or introduce a source-token quota.
Native tests check complete identifier spans on both sides of the buffer boundary.
These are lexical provenance checks, not claims about MATLAB runtime name limits.
