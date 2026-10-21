import assert from "node:assert/strict";

function sourceEndPoint(source) {
  const bytes = Buffer.from(source);
  let row = 0;
  for (const byte of bytes) if (byte === 0x0a) row += 1;
  return `${row}:${bytes.length - bytes.lastIndexOf(0x0a) - 1}`;
}

function readCst(tree, source) {
  const bytes = Buffer.from(source);
  const lineStarts = [0];
  for (let index = 0; index < bytes.length; index++) {
    if (bytes[index] === 0x0a) lineStarts.push(index + 1);
  }
  const nodes = [];
  const ancestors = [];
  for (const line of tree.split("\n")) {
    const row = /^([0-9]+):([0-9]+) +- +([0-9]+):([0-9]+) +/.exec(line);
    if (row === null) continue;
    const match =
      /^(([a-z_][a-z_0-9]*): )?([a-z_][a-z_0-9]*|ERROR|MISSING|"[^"]+")/.exec(
        line.slice(row[0].length).replace(/^•/, ""),
      );
    if (match === null) continue;
    const indentation = row[0].length;
    while (ancestors.length && ancestors.at(-1).indentation >= indentation) {
      ancestors.pop();
    }
    const [, , field, kind] = match;
    const start = [Number(row[1]), Number(row[2])];
    const end = [Number(row[3]), Number(row[4])];
    const index = nodes.length;
    nodes.push({
      parent: ancestors.at(-1)?.index ?? null,
      kind,
      field: field ?? null,
      start,
      end,
      startByte: lineStarts[start[0]] + start[1],
      endByte: lineStarts[end[0]] + end[1],
    });
    ancestors.push({ indentation, index });
  }
  return nodes;
}

function nodeRange(node) {
  return `[${node.start.join(", ")}] - [${node.end.join(", ")}]`;
}

function subtree(nodes, index) {
  let end = index + 1;
  while (
    end < nodes.length &&
    nodes[end].parent !== null &&
    nodes[end].parent >= index
  )
    end++;
  return nodes.slice(index, end);
}

function issueSignatures(nodes, root = 0) {
  return subtree(nodes, root).flatMap((issue, offset) => {
    if (issue.kind !== "syntax_issue") return [];
    assert.equal(issue.field, "issue", "issues must use the issue field");
    const index = root + offset;
    const outcome = nodes[index + 1];
    const reason = nodes[index + 2];
    assert.ok(outcome !== undefined, "missing issue outcome");
    assert.ok(reason !== undefined, "missing issue reason");
    assert.equal(outcome.parent, index, "issue must directly own its outcome");
    assert.equal(
      reason.parent,
      index + 1,
      "outcome must directly own its reason",
    );
    const range = nodeRange(issue);
    assert.equal(
      nodeRange(outcome),
      range,
      "outcome range must match its issue",
    );
    assert.equal(nodeRange(reason), range, "reason range must match its issue");
    return [{ outcome: outcome.kind, reason: reason.kind, range }];
  });
}

function publicNodes(nodes) {
  const visible = [];
  const parents = [];
  const missing = [];
  for (const [index, node] of nodes.entries()) {
    const parent = node.parent === null ? null : parents[node.parent];
    parents[index] = parent;
    missing[index] = node.kind === "MISSING" || missing[node.parent] === true;
    if (missing[index] || node.kind === "ERROR") continue;
    parents[index] = visible.length;
    visible.push({
      ...node,
      parent,
      field: nodes[node.parent]?.kind === "ERROR" ? null : node.field,
    });
  }
  return visible;
}

function parseCst(tree, source) {
  const nodes = readCst(tree, source);
  assert.equal(nodes[0]?.kind, "script", tree);
  assert.equal(
    nodes[0].end.join(":"),
    sourceEndPoint(source),
    "root must reach the edited source end",
  );
  return nodes;
}

export { issueSignatures, parseCst, publicNodes, sourceEndPoint };
