import assert from "node:assert/strict";

function parseCorpus(text) {
  const sections = text.split("==================\n");
  assert.equal(sections.shift(), "", "corpus must begin with a header");
  assert.equal(sections.length % 2, 0, "corpus headers must be paired");
  const cases = [];
  for (let index = 0; index < sections.length; index += 2) {
    const [name, ...attributes] = sections[index].trimEnd().split("\n");
    assert.ok(name, "corpus case must have a name");
    const languages = [];
    for (const attribute of attributes) {
      if (attribute === ":cst") continue;
      const match = /^:language\(([a-z_]+)\)$/.exec(attribute);
      assert.ok(match, `unsupported corpus attribute: ${attribute}`);
      assert.ok(!languages.includes(match[1]), `${name}: duplicate language`);
      languages.push(match[1]);
    }
    assert.ok(languages.length > 0, `${name}: missing language`);
    const parts = sections[index + 1].split("\n------------------\n");
    assert.equal(parts.length, 2, `${name}: expected one output separator`);
    const source = parts[0].endsWith("\r") ? parts[0].slice(0, -1) : parts[0];
    cases.push({ name, languages, source });
  }
  return cases;
}

export { parseCorpus };
