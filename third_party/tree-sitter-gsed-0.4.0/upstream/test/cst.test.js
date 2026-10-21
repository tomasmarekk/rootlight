import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { grammars, root } from "../scripts/tree-sitter.js";
import { parseCorpus } from "./support/corpus.js";
import { issueSignatures, parseCst, publicNodes } from "./support/cst.js";
import {
  applyEdits,
  assertIncrementalContract,
  parse,
  parseSuccessfully,
  parseSummary,
} from "./support/parser.js";

const largeCommandCount = 100_000;
const largeTextCharacters = 1024 * 1024;
const largeRegexCharacters = 100_000;
const nestedBlockDepth = 2_000;
const nestedGroupDepth = 2_000;
const resourceTimeoutMicros = 10_000_000;
const invalidUtf8 = Buffer.from([0xff, 0xfe]);

test("gsed: corpus reader preserves source and language selection", () => {
  const header = "==================\n";
  const separator = "\n------------------\n";
  const source = "p\0\n";
  assert.deepEqual(
    parseCorpus(
      `${header}Shared input\n:language(gsed)\n:language(gsed_ere)\n:cst\n${header}${source}${separator}expected\n\n` +
        `${header}Input at EOF\n:language(gsed)\n${header}p${separator}(script)\n`,
    ),
    [
      { name: "Shared input", languages: ["gsed", "gsed_ere"], source },
      { name: "Input at EOF", languages: ["gsed"], source: "p" },
    ],
  );
});

const corpusDirectory = join(root, "test", "corpus");
for (const file of readdirSync(corpusDirectory).sort()) {
  const cases = parseCorpus(readFileSync(join(corpusDirectory, file), "utf8"));
  for (const { name, languages, source } of cases) {
    for (const language of languages) {
      const grammar = grammars.find(({ name }) => name === language);
      assert.ok(grammar, `${file}: unknown language ${language}`);
      test(`${language}: ${file}: ${name}: CST invariants and fresh determinism`, () => {
        const first = parseSuccessfully(grammar.scope, source);
        const second = parseSuccessfully(grammar.scope, source);
        issueSignatures(first.nodes);
        assert.deepEqual(second.nodes, first.nodes);
      });
    }
  }
}

const cstSource = "# é\n1,";
const cstOutput = [
  "0:0 - 1:2   script",
  "0:0 - 1:2     command_list",
  "0:0 - 0:4       comment",
  "0:0 - 0:4         comment_text `# é`",
  "0:4 - 1:0       command_separator",
  "0:4 - 0:5         `\\n`",
  "1:0 - 1:2       command",
  "1:0 - 1:2         address: address_clause",
  "1:0 - 1:1           address",
  "1:0 - 1:1             line_address",
  "1:0 - 1:1               first: number `1`",
  "1:1 - 1:2           address_separator `,`",
  "1:2 - 1:2           issue: syntax_issue",
  "1:2 - 1:2             incomplete_syntax",
  "1:2 - 1:2               missing_address",
  "1:2 - 1:2         issue: syntax_issue",
  "1:2 - 1:2           incomplete_syntax",
  "1:2 - 1:2             missing_command",
].join("\n");

test("gsed: CST helpers retain fields, UTF-8 byte offsets and zero-width issues", () => {
  const nodes = parseCst(cstOutput, cstSource);
  assert.deepEqual(
    nodes.map(({ kind, field, parent, start, end, startByte, endByte }) => [
      kind,
      field,
      parent,
      start,
      end,
      startByte,
      endByte,
    ]),
    [
      ["script", null, null, [0, 0], [1, 2], 0, 7],
      ["command_list", null, 0, [0, 0], [1, 2], 0, 7],
      ["comment", null, 1, [0, 0], [0, 4], 0, 4],
      ["comment_text", null, 2, [0, 0], [0, 4], 0, 4],
      ["command_separator", null, 1, [0, 4], [1, 0], 4, 5],
      ["command", null, 1, [1, 0], [1, 2], 5, 7],
      ["address_clause", "address", 5, [1, 0], [1, 2], 5, 7],
      ["address", null, 6, [1, 0], [1, 1], 5, 6],
      ["line_address", null, 7, [1, 0], [1, 1], 5, 6],
      ["number", "first", 8, [1, 0], [1, 1], 5, 6],
      ["address_separator", null, 6, [1, 1], [1, 2], 6, 7],
      ["syntax_issue", "issue", 6, [1, 2], [1, 2], 7, 7],
      ["incomplete_syntax", null, 11, [1, 2], [1, 2], 7, 7],
      ["missing_address", null, 12, [1, 2], [1, 2], 7, 7],
      ["syntax_issue", "issue", 5, [1, 2], [1, 2], 7, 7],
      ["incomplete_syntax", null, 14, [1, 2], [1, 2], 7, 7],
      ["missing_command", null, 15, [1, 2], [1, 2], 7, 7],
    ],
  );
  assert.deepEqual(issueSignatures(nodes), [
    {
      outcome: "incomplete_syntax",
      reason: "missing_address",
      range: "[1, 2] - [1, 2]",
    },
    {
      outcome: "incomplete_syntax",
      reason: "missing_command",
      range: "[1, 2] - [1, 2]",
    },
  ]);
});

test("gsed: CST helpers reject truncated roots and malformed issue ownership", () => {
  assert.throws(() => parseCst("", cstSource), assert.AssertionError);
  assert.throws(
    () => parseCst(cstOutput, `${cstSource}p`),
    /root must reach the edited source end/,
  );
  for (const [name, before, after] of [
    ["wrong field", "issue: syntax_issue", "reason: syntax_issue"],
    [
      "outcome without an issue owner",
      "1:2 - 1:2             incomplete_syntax",
      "1:2 - 1:2           incomplete_syntax",
    ],
    [
      "reason without an outcome owner",
      "1:2 - 1:2               missing_address",
      "1:2 - 1:2             missing_address",
    ],
    [
      "outcome range differs",
      "1:2 - 1:2             incomplete_syntax",
      "1:1 - 1:2             incomplete_syntax",
    ],
    [
      "reason range differs",
      "1:2 - 1:2               missing_address",
      "1:1 - 1:2               missing_address",
    ],
  ]) {
    const nodes = parseCst(cstOutput.replace(before, after), cstSource);
    assert.throws(() => issueSignatures(nodes), assert.AssertionError, name);
  }
});

test("gsed: public CST comparison retains source children inside native recovery", () => {
  const nodes = parseCst(
    [
      "0:0 - 0:1   script",
      "0:0 - 0:1     command_list",
      "0:0 - 0:1       •ERROR",
      "0:0 - 0:1         command: command",
      "0:0 - 0:1           function: simple_command",
      '0:0 - 0:1             "p"',
      "0:1 - 0:1       •MISSING",
      "0:1 - 0:1         command",
    ].join("\n"),
    "p",
  );
  assert.deepEqual(
    publicNodes(nodes).map(({ kind, field, parent }) => [kind, field, parent]),
    [
      ["script", null, null],
      ["command_list", null, 0],
      ["command", null, 1],
      ["simple_command", "function", 2],
      ['"p"', null, 3],
    ],
  );
});

for (const scope of ["source.gsed", "source.gsed.ere"]) {
  for (const [prefix, suffix, owner] of [
    ["s/a/", "/", "replacement"],
    ["y/", "/x/", "translation_string"],
    ["a ", "\n", "text"],
    ["r ", "\n", "filename"],
    ["b ", "\n", "label"],
    ["v ", "\n", "version"],
    ["e ", "\n", "shell_command"],
    ["s/[[:", ":]]/x/", "class_name"],
    ["s/[[.", ".]]/x/", "collating_element"],
    ["#", "\n", "comment"],
    ["s/", "/x/", "regex_branch"],
  ]) {
    test(`${scope}: invalid UTF-8 remains inside ${owner}`, () => {
      const before = Buffer.from(`${prefix}a`);
      const source = Buffer.concat([
        before,
        invalidUtf8,
        Buffer.from(`b${suffix}`),
      ]);
      const result = parseSuccessfully(scope, source);
      assert.deepEqual(
        issueSignatures(result.nodes).map(
          ({ outcome, reason }) => `${outcome}/${reason}`,
        ),
        ["invalid_syntax/invalid_encoding"],
        result.stdout,
      );
      const issue = result.nodes.find((node) => node.kind === "syntax_issue");
      assert.equal(result.nodes[issue.parent].kind, owner, result.stdout);
      assert.deepEqual(
        [issue.startByte, issue.endByte],
        [before.length, before.length + invalidUtf8.length],
      );
    });
  }

  for (const [prefix, suffix, owner] of [
    ["s", ";p", "substitute_command"],
    ["y", ";p", "translate_command"],
    ["\\", "p", "regex_address"],
    ["1", ";p", "command"],
    ["!", ";p", "command"],
    ["s/a/x/", "p", "substitution_flags"],
  ]) {
    test(`${scope}: invalid UTF-8 at ${owner} syntax boundary`, () => {
      const before = Buffer.from(prefix);
      const source = Buffer.concat([before, invalidUtf8, Buffer.from(suffix)]);
      const result = parseSuccessfully(scope, source);
      assert.deepEqual(
        issueSignatures(result.nodes).map(({ reason }) => reason),
        ["invalid_encoding"],
        result.stdout,
      );
      const issue = result.nodes.find((node) => node.kind === "syntax_issue");
      assert.equal(result.nodes[issue.parent].kind, owner, result.stdout);
      assert.deepEqual(
        [issue.startByte, issue.endByte],
        [before.length, before.length + invalidUtf8.length],
      );
      for (let offset = 0; offset < invalidUtf8.length; offset++) {
        const byte = before.length + offset;
        const edits = [{ byte, deleteBytes: 1, insert: "" }];
        assertIncrementalContract(
          parse(scope, applyEdits(source, edits)),
          parse(scope, source, edits),
          `${prefix} invalid byte ${byte}`,
        );
      }
    });
  }

  for (const { prefix, bytes } of [
    { prefix: "", bytes: [0xff, 0xce, 0xa3] },
    { prefix: ":", bytes: [0xff, 0xce, 0xa3] },
    { prefix: "s/", bytes: [0xff, 0xce, 0xa3] },
    { prefix: "s/a/", bytes: [0xff, 0xce, 0xa3] },
    { prefix: "s/[\\", bytes: [0xce, 0xa3] },
  ]) {
    test(`${scope}: editing UTF-8 after ${JSON.stringify(prefix)} preserves incremental CST`, () => {
      const source = Buffer.concat([
        Buffer.from(prefix),
        Buffer.from(bytes),
        Buffer.from("\n"),
      ]);
      const byte = Buffer.byteLength(prefix) + bytes.length - 1;
      for (const [deleteBytes, insert] of [
        [1, ""],
        [0, "x"],
      ]) {
        const edits = [{ byte, deleteBytes, insert }];
        assertIncrementalContract(
          parse(scope, applyEdits(source, edits)),
          parse(scope, source, edits),
          `${JSON.stringify(edits)}`,
        );
      }
    });
  }
}

for (const scope of ["source.gsed", "source.gsed.ere"]) {
  const open = scope.endsWith("ere") ? "(" : "\\(";
  const close = scope.endsWith("ere") ? ")" : "\\)";
  for (const [name, source] of [
    [`${largeCommandCount} commands`, "p;".repeat(largeCommandCount)],
    ["one MiB text operand", `a ${"x".repeat(largeTextCharacters)}`],
    [
      `${largeRegexCharacters} regex characters`,
      `s/${"a".repeat(largeRegexCharacters)}/b/`,
    ],
    [
      `${nestedBlockDepth} nested blocks`,
      `${"{".repeat(nestedBlockDepth)}p${"}".repeat(nestedBlockDepth)}`,
    ],
    [
      `${nestedGroupDepth} nested groups`,
      `s/${open.repeat(nestedGroupDepth)}a${close.repeat(nestedGroupDepth)}/b/`,
    ],
    [
      `${nestedGroupDepth} unclosed groups`,
      `s/${open.repeat(nestedGroupDepth)}a`,
    ],
  ]) {
    test(`${scope}: resources ${name}`, () => {
      const result = parseSummary(scope, source, resourceTimeoutMicros);
      assert.equal(result.successful, true, JSON.stringify(result));
    });
  }
}
