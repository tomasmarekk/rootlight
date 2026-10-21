import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { createTreeSitter } from "../scripts/tree-sitter.js";

const firstThemeColor = 17;

function decodeEntities(text) {
  return text
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&amp;", "&");
}

function renderedCaptures(html, source) {
  const start = html.indexOf("<pre><code>");
  const end = html.indexOf("</code></pre>");
  assert.ok(start >= 0 && end >= start, html);
  const content = html.slice(start + "<pre><code>".length, end);
  const stack = [];
  const captures = [];
  let text = "";
  for (const part of content.matchAll(
    /<span class='([^']*)'>|<[/]span>|([^<]+)/g,
  )) {
    if (part[1] !== undefined) stack.push(part[1].replaceAll(" ", "."));
    else if (part[0] === "</span>") assert.notEqual(stack.pop(), undefined);
    else {
      const decoded = decodeEntities(part[2]);
      text += decoded;
      captures.push(
        ...Array(Buffer.byteLength(decoded)).fill(stack.at(-1) ?? ""),
      );
    }
  }
  assert.equal(stack.length, 0, "unclosed highlight span");
  assert.equal(
    text.replace(/\n$/, ""),
    source.replace(/\n$/, ""),
    "rendered source differs from the input",
  );
  return captures;
}

function assertCaptures(source, actual, ranges) {
  const bytes = Buffer.from(source);
  const expected = Array(bytes.length).fill("");
  let previousEnd = 0;
  for (const [start, end, capture] of ranges) {
    assert.ok(
      Number.isSafeInteger(start) && start >= previousEnd,
      "expected ranges must be ordered and disjoint",
    );
    assert.ok(
      Number.isSafeInteger(end) && end > start && end <= bytes.length,
      "expected range exceeds source bytes",
    );
    expected.fill(capture, start, end);
    previousEnd = end;
  }
  for (const [index, byte] of bytes.entries()) {
    if (byte !== 0x0a)
      assert.equal(
        actual[index],
        expected[index],
        `byte ${index} in ${JSON.stringify(source)}`,
      );
  }
}

let runner;
const captures = [
  "character.special",
  "comment",
  "constant.builtin",
  "keyword",
  "keyword.directive",
  "keyword.modifier",
  "label",
  "number",
  "operator",
  "punctuation.bracket",
  "punctuation.delimiter",
  "punctuation.special",
  "string",
  "string.escape",
  "string.regexp",
  "string.special.path",
  "string.special.symbol",
];
before(() => {
  runner = createTreeSitter();
  const config = JSON.parse(readFileSync(runner.configPath, "utf8"));
  config.theme = Object.fromEntries(
    captures.map((name, index) => [name, index + firstThemeColor]),
  );
  writeFileSync(runner.configPath, JSON.stringify(config));
  writeFileSync(
    join(runner.directory, "captures.txt"),
    `${captures.join("\n")}\n`,
  );
});
after(() => runner?.close());

const cases = [
  {
    name: "shell operand sed escapes",
    source: "e x\\t\\x41\\\ny",
    ranges: [
      [0, 1, "keyword"],
      [2, 3, "string"],
      [3, 9, "string.escape"],
      [9, 11, "punctuation.special"],
      [11, 12, "string"],
    ],
  },
  {
    name: "EOF backslash leaves shell escapes literal",
    source: "e x\\t\\",
    ranges: [
      [0, 1, "keyword"],
      [2, 5, "string"],
      [5, 6, "string.escape"],
    ],
  },
  {
    name: "numeric escape contains the escaped delimiter digit",
    source: String.raw`s5\x\55X5`,
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 6, "string.escape"],
      [6, 7, "punctuation.delimiter"],
      [7, 8, "string"],
      [8, 9, "punctuation.delimiter"],
    ],
  },
  {
    name: "version word followed by a command",
    source: "v0alpha p",
    ranges: [
      [0, 1, "keyword"],
      [1, 7, "string"],
      [8, 9, "keyword"],
    ],
  },
  {
    name: "label word followed by a command and comment",
    source: ":x p#hi",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "label"],
      [3, 4, "keyword"],
      [4, 7, "comment"],
    ],
  },
  {
    name: "misplaced class retains payload and markers",
    source: "s/[:alpha:]/x/",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 4, "punctuation.bracket"],
      [4, 9, "character.special"],
      [9, 11, "punctuation.bracket"],
      [11, 12, "punctuation.delimiter"],
      [12, 13, "string"],
      [13, 14, "punctuation.delimiter"],
    ],
  },
  {
    name: "replacement escapes and flags",
    source: "s/a/\\U&/gg",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 3, "string.regexp"],
      [3, 4, "punctuation.delimiter"],
      [4, 6, "string.escape"],
      [6, 7, "string.special.symbol"],
      [7, 8, "punctuation.delimiter"],
      [8, 10, "keyword.modifier"],
    ],
  },
  {
    name: "numeric escapes keep escape captures",
    source: "s/\\x5ba\\x5d/b/",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 6, "string.escape"],
      [6, 7, "string.regexp"],
      [7, 11, "string.escape"],
      [11, 12, "punctuation.delimiter"],
      [12, 13, "string"],
      [13, 14, "punctuation.delimiter"],
    ],
  },
  {
    name: "filenames own command punctuation",
    source: "w file;p\n",
    ranges: [
      [0, 1, "keyword"],
      [2, 8, "string.special.path"],
    ],
  },
  {
    name: "directive and comment body",
    source: "#n hi\n",
    ranges: [
      [0, 2, "keyword.directive"],
      [2, 5, "comment"],
    ],
  },
  {
    name: "labels and separators",
    source: ": loop;b loop",
    ranges: [
      [0, 1, "keyword"],
      [2, 6, "label"],
      [6, 7, "punctuation.delimiter"],
      [7, 8, "keyword"],
      [9, 13, "label"],
    ],
  },
  {
    name: "unmatched closing brace keeps its bracket role",
    source: "p;}",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 3, "punctuation.bracket"],
    ],
  },
  {
    name: "range retains address and command roles",
    source: "1,2q",
    ranges: [
      [0, 1, "number"],
      [1, 2, "punctuation.delimiter"],
      [2, 3, "number"],
      [3, 4, "keyword"],
    ],
  },
  {
    name: "host delimiter escape retains its source role",
    source: "s.\\..x.",
    ranges: [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 4, "string.escape"],
      [4, 5, "punctuation.delimiter"],
      [5, 6, "string"],
      [6, 7, "punctuation.delimiter"],
    ],
  },
  {
    name: "brackets classes and repetition",
    source: "/[[:alpha:]a-z]*/p",
    ranges: [
      [0, 1, "punctuation.delimiter"],
      [1, 4, "punctuation.bracket"],
      [4, 9, "character.special"],
      [9, 11, "punctuation.bracket"],
      [11, 12, "string.regexp"],
      [12, 13, "operator"],
      [13, 14, "string.regexp"],
      [14, 15, "punctuation.bracket"],
      [15, 16, "operator"],
      [16, 17, "punctuation.delimiter"],
      [17, 18, "keyword"],
    ],
  },
];
function assertHighlights(scope, source, ranges) {
  const path = join(runner.directory, "highlight.sed");
  writeFileSync(path, source);
  const result = runner.run([
    "highlight",
    "--check",
    "--captures-path",
    join(runner.directory, "captures.txt"),
    "--config-path",
    runner.configPath,
    "--html",
    "--layout",
    "fragment",
    "--style",
    "classes",
    "--scope",
    scope,
    path,
  ]);
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.doesNotMatch(result.stderr, /Non-standard highlight captures/);
  assertCaptures(source, renderedCaptures(result.stdout, source), ranges);
}

for (const scope of ["source.gsed", "source.gsed.ere"]) {
  for (const { name, source, ranges } of cases) {
    test(`${scope}: highlights ${name}`, () => {
      assertHighlights(scope, source, ranges);
    });
  }
}

for (const [scope, source, ranges] of [
  [
    "source.gsed",
    String.raw`s/a\{1x\}/b/`,
    [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 3, "string.regexp"],
      [3, 5, "punctuation.bracket"],
      [5, 6, "number"],
      [6, 7, "string.regexp"],
      [7, 9, "punctuation.bracket"],
      [9, 10, "punctuation.delimiter"],
      [10, 11, "string"],
      [11, 12, "punctuation.delimiter"],
    ],
  ],
  [
    "source.gsed.ere",
    "s/a{1x}/b/",
    [
      [0, 1, "keyword"],
      [1, 2, "punctuation.delimiter"],
      [2, 3, "string.regexp"],
      [3, 4, "punctuation.bracket"],
      [4, 5, "number"],
      [5, 6, "string.regexp"],
      [6, 7, "punctuation.bracket"],
      [7, 8, "punctuation.delimiter"],
      [8, 9, "string"],
      [9, 10, "punctuation.delimiter"],
    ],
  ],
]) {
  test(`${scope}: highlights invalid interval characters as regex literals`, () => {
    assertHighlights(scope, source, ranges);
  });
}
