import assert from "node:assert/strict";
import { test } from "node:test";
import { issueSignatures } from "./support/cst.js";
import {
  applyEdits,
  assertIncrementalContract,
  parse,
  parseSuccessfully,
} from "./support/parser.js";

for (const scope of ["source.gsed", "source.gsed.ere"]) {
  const group = scope.endsWith("ere") ? "(a|b)+" : "\\(a\\|b\\)\\+";

  for (const source of [
    `s/${group}/\\U&/g\np\n`,
    "{\n/ab/Ip;a hello\n}\n",
    "s/\\x5ba\\x5d/x/\n",
    "s/\\xce\\xa3*/x/\n",
    String.raw`s\b\X\p`,
    String.raw`y\abc\xyz\;p`,
    String.raw`\\b\p`,
    "a x\\t\\",
    "a x\\c\\.\\",
    "a x\\t\\\ny\\n\\",
    "e x\\t\\",
    "e\\\nx\\t",
    "e\\\\t\\",
    "a\\\\\\",
    "e\\\\\\",
    String.raw`e x\c\y`,
    String.raw`s5\x6\55X5p`,
    String.raw`s5\x6\65X5p`,
    String.raw`s5\x\5ba\x\5d5X5`,
    "p;   ,2p",
    "{   ,2p}",
    "/[!---0]/p",
    "/[a-b---c]/p",
    String.raw`/[a-\x2d-c]/p`,
    String.raw`/[a--\x2dc]/p`,
    scope.endsWith("ere")
      ? String.raw`s/a{1b2,\q34d5}/x/`
      : String.raw`s/a\{1b2,\q34d5\}/x/`,
  ]) {
    test(`${scope}: deletion and restoration in ${JSON.stringify(source)}`, () => {
      const expected = parseSuccessfully(scope, source);
      for (let byte = 0; byte < Buffer.byteLength(source); byte++) {
        const edits = [{ byte, deleteBytes: 1, insert: "" }];
        const edited = applyEdits(source, edits);
        const fresh = parse(scope, edited);
        assertIncrementalContract(
          fresh,
          parse(scope, source, edits),
          `delete byte ${byte}`,
        );
        const restored = [{ byte, deleteBytes: 0, insert: source[byte] }];
        assertIncrementalContract(
          expected,
          parse(scope, edited, restored),
          `restore byte ${byte}`,
        );
      }
    });
  }

  test(`${scope}: appending source through incomplete states`, () => {
    for (const source of [
      "s/[[:alpha:]]/word/g\n",
      "0~2p;0,/a/p\n",
      "3,0~4p\n",
      "/[[....]]/p\n",
      "/[[:alpha:]-]/p\n",
      "/[a-b-]/p\n",
      "s/a\\{1\\x2c\\}/x/\n",
      "/a{1\\x2c}/p\n",
      "/a\\{1\\x5c\\x7d/p\n",
      "/a\\b\\{1\\x5c}/p\n",
      "/\\o510/p\n",
      "/\\d420/p\n",
      "/[[:alpha\\x3a]:]]/p\n",
      "s3a\\{1\\x\\30\\}3x3\n",
      "s3a{1\\x\\30}3x3\n",
      "/[0-z-\\o135]/p\n",
      "/[!---0]/p\n",
      "/[a-b---c]/p\n",
      String.raw`/[a-b-\x2d-c]/p`,
      String.raw`/[a-b--\x2dc]/p`,
      "ax\\c\\.\\",
      "e x\\c\\y\\",
      "a\\\nx\\c\\.\\",
      "e x\\c\\\np\\",
    ]) {
      for (let end = 1; end <= source.length; end++) {
        const before = source.slice(0, end - 1);
        const edits = [
          { byte: end - 1, deleteBytes: 0, insert: source[end - 1] },
        ];
        const fresh = parse(scope, source.slice(0, end));
        assert.deepEqual(
          issueSignatures(fresh.nodes).filter(
            ({ outcome }) => outcome !== "incomplete_syntax",
          ),
          [],
          `${JSON.stringify(source.slice(0, end))} has determined issues`,
        );
        assertIncrementalContract(
          fresh,
          parse(scope, before, edits),
          `prefix ${end} of ${JSON.stringify(source)}`,
        );
      }
    }
  });

  test(`${scope}: address and opaque operand edits preserve incremental CST`, () => {
    for (const source of [
      "b;t;T;e\n",
      ":x p\nb x#hi\n",
      "v0alpha p\n",
      "\rp;\vp;\fp",
      "s[\\c[[x[",
      "s/[:alpha:]/x/",
      "s/[^\\x3aalpha:\\x5d]/x/",
      "\\Σp\n",
      "pΣ;p Σ\n",
      "\\\np\n",
      "0,/a/p\n",
      "0r file\n",
      "1,2q\n",
      ",2q\n",
      "1/a/p\n",
      "p\0p\n",
      "!#c\np\n",
      ":loop; b loop\n",
      "#n hi\na text\n",
      "\r#n\n:",
      "w file;p\n",
      "s/[[:\\x61lpha:]]/x/\n",
      "s/[\\x5b\\x3aalpha\\x3a\\x5d]/x/\n",
      "s$\\$$x$",
      "s/[a-\\x5d]/x/",
      "s@[a\\x5d@]@x@",
      "s/\\x5b/x/",
      "s/[a-b-c]/x/",
      "s/[a-[=z=]]/x/",
    ]) {
      for (let byte = 0; byte < Buffer.byteLength(source); byte++) {
        const edits = [{ byte, deleteBytes: 1, insert: "" }];
        assertIncrementalContract(
          parse(scope, applyEdits(source, edits)),
          parse(scope, source, edits),
          `${JSON.stringify(source)} delete ${byte}`,
        );
      }
    }
  });
}
