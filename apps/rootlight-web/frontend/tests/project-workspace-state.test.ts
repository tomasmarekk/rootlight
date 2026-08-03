// Verifies safe URL continuity for immutable project graph exploration.

import { describe, expect, it } from "vitest";

import {
  parseProjectWorkspaceState,
  serializeProjectWorkspaceState,
  workspaceStateEquals,
} from "../src/routing/project-workspace-state";

const generationId = `gen1_${"b".repeat(39)}`;
const symbolId = `sym1_${"c".repeat(39)}`;

describe("project workspace URL state", () => {
  it("round trips exact generation, filters, selection, and display settings", () => {
    const state = parseProjectWorkspaceState(
      new URLSearchParams([
        ["generation", generationId],
        ["view", "neighborhood"],
        ["selected", symbolId],
        ["node", "symbol"],
        ["relation", "calls"],
        ["relation", "references"],
        ["language", "rust"],
        ["min_confidence", "750"],
        ["include_inferred", "false"],
        ["include_generated", "true"],
        ["labels", "false"],
        ["budget", "expanded"],
      ]),
    );

    expect(state).toMatchObject({
      generation: generationId,
      view: "neighborhood",
      selected: symbolId,
      nodeKinds: ["symbol"],
      relations: ["calls", "references"],
      language: "rust",
      minConfidence: 750,
      labels: false,
      budgetProfile: "expanded",
    });
    expect(
      workspaceStateEquals(
        parseProjectWorkspaceState(serializeProjectWorkspaceState(state)),
        state,
      ),
    ).toBe(true);
  });

  it("normalizes unsafe, unsupported, and context-free values", () => {
    const state = parseProjectWorkspaceState(
      new URLSearchParams({
        generation: "active/../../secret",
        view: "symbols",
        selected: "C:\\private\\source.rs",
        language: "<script>",
        min_confidence: "999",
        include_inferred: "maybe",
        labels: "sometimes",
        budget: "unbounded",
      }),
    );

    expect(state).toMatchObject({
      generation: "active",
      view: "architecture",
      selected: undefined,
      language: undefined,
      minConfidence: 0,
      includeInferred: false,
      labels: true,
      budgetProfile: "balanced",
    });
    expect(serializeProjectWorkspaceState(state).toString()).not.toContain("private");
  });

  it("deduplicates closed filters and drops unknown values", () => {
    const state = parseProjectWorkspaceState(
      new URLSearchParams([
        ["node", "file"],
        ["node", "file"],
        ["node", "future"],
        ["relation", "imports"],
        ["relation", "imports"],
        ["relation", "future"],
      ]),
    );

    expect(state.nodeKinds).toEqual(["file"]);
    expect(state.relations).toEqual(["imports"]);
  });
});
