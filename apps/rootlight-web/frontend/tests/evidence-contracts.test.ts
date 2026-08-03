// Exercises fail-closed parsing for evidence, explicit source, and typed impact DTOs.

import { describe, expect, it } from "vitest";

import {
  parseChangeImpact,
  parseNodeDetail,
  parseRelationships,
  parseSourceRead,
} from "../src/features/inspector/model/evidence-contracts";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const symbolId = `sym1_${"c".repeat(39)}`;
const targetId = `sym1_${"d".repeat(39)}`;
const fileId = `file1_${"e".repeat(39)}`;
const capability = "f".repeat(43);

describe("evidence contracts", () => {
  it("correlates source-free node detail and preserves untrusted evidence as text", () => {
    const value = nodeDetailFixture();
    expect(parseNodeDetail(value, repositoryId, generationId, symbolId)).toMatchObject({
      nodeId: symbolId,
      provider: "<provider>",
      evidence: "**repository evidence**",
      sourceReferences: [{ capability, expiresInSeconds: 60 }],
    });

    expect(() =>
      parseNodeDetail(
        {
          ...value,
          context: evidenceContext("1"),
        },
        repositoryId,
        generationId,
        symbolId,
      ),
    ).toThrow(/source-free/u);
    expect(() => parseNodeDetail(value, repositoryId, `gen1_${"z".repeat(39)}`, symbolId)).toThrow(
      /immutable generation/u,
    );
  });

  it("preserves relationship groups and rejects inconsistent continuation", () => {
    const value = {
      schema: "rootlight.web-relationships/1",
      context: evidenceContext(),
      groups: [
        {
          seedId: symbolId,
          relation: "calls",
          direction: "outbound",
          totalCount: "1",
          targets: [
            {
              symbolId: targetId,
              confidence: 875,
              sourceReferences: [{ capability, expiresInSeconds: 60 }],
            },
          ],
        },
      ],
      returnedEdges: "1",
      totalEdges: "1",
      exact: true,
      truncated: false,
      nextPageOffset: null,
      completeness: complete(),
    };

    expect(parseRelationships(value, repositoryId, generationId).groups[0]).toMatchObject({
      relation: "calls",
      direction: "outbound",
      targets: [{ symbolId: targetId }],
    });
    expect(() =>
      parseRelationships({ ...value, nextPageOffset: "1" }, repositoryId, generationId),
    ).toThrow(/continuation/u);
  });

  it("validates explicit UTF-8 source ranges and byte accounting", () => {
    const content = "<script>alert('repository data')</script>\nfn main() {}";
    const bytes = new TextEncoder().encode(content).byteLength;
    const value = {
      schema: "rootlight.web-source/1",
      repositoryId,
      generationId,
      chunks: [
        {
          fileId,
          path: "src/untrusted.rs",
          requestedStartByte: "0",
          requestedEndByte: String(bytes),
          includedStartByte: "0",
          includedEndByte: String(bytes),
          includedStartLine: "1",
          includedEndLine: "2",
          content,
          encoding: "utf8",
          contentHash: `b3_${"f".repeat(58)}`,
          language: "rust",
          tier: "tier_a",
          generated: false,
        },
      ],
      totalSourceBytes: String(bytes),
      truncated: false,
      context: evidenceContext(String(bytes)),
      completeness: complete(),
    };

    expect(parseSourceRead(value, repositoryId, generationId).chunks[0]?.content).toBe(content);
    expect(() =>
      parseSourceRead(
        { ...value, totalSourceBytes: String(bytes + 1) },
        repositoryId,
        generationId,
      ),
    ).toThrow(/accounting/u);
  });

  it("maps bounded change impact without admitting paths or generic queries", () => {
    const value = {
      schema: "rootlight.web-change-impact/1",
      context: evidenceContext(),
      resolvedChanges: [{ symbolId, fileId: null, classification: "resolved", kind: "function" }],
      impacted: [
        {
          sourceIndex: 0,
          dependents: [
            {
              symbolId: targetId,
              kind: "function",
              distance: 2,
              confidence: 900,
              via: ["calls"],
              isPublic: true,
            },
          ],
        },
      ],
      tests: [
        {
          testId: "test_rootlight",
          relevance: 800,
          why: ["covers_changed_symbol"],
          estimatedCostMs: 25,
        },
      ],
      riskSummary: {
        level: "medium",
        reasons: ["public_fanout"],
        coverage: "bounded",
        breakingSurface: true,
        fanout: 1,
        dynamicBlindSpots: false,
      },
      completeness: complete(),
    };

    expect(parseChangeImpact(value, repositoryId, generationId)).toMatchObject({
      impacted: [{ dependents: [{ symbolId: targetId, distance: 2 }] }],
      riskSummary: { level: "medium", fanout: 1 },
    });
    expect(() =>
      parseChangeImpact(
        {
          ...value,
          impacted: [
            {
              sourceIndex: 0,
              dependents: [
                {
                  symbolId: targetId,
                  kind: "function",
                  distance: 9,
                  confidence: 900,
                  via: ["calls"],
                  isPublic: true,
                },
              ],
            },
          ],
        },
        repositoryId,
        generationId,
      ),
    ).toThrow(/integer/u);
  });
});

function nodeDetailFixture() {
  return {
    schema: "rootlight.web-node-detail/1",
    repositoryId,
    generationId,
    nodeId: symbolId,
    idKind: "symbol",
    kind: "function",
    displayName: "run",
    qualifiedName: null,
    signature: "fn run()",
    language: "rust",
    tier: "tier_a",
    confidence: 950,
    provider: "<provider>",
    evidence: "**repository evidence**",
    outboundExact: "1",
    outboundCandidates: "0",
    inboundExact: "2",
    inboundCandidates: "0",
    referenceCount: "3",
    generated: null,
    sourceReferences: [{ capability, expiresInSeconds: 60 }],
    context: evidenceContext(),
    completeness: complete(),
  };
}

function evidenceContext(sourceBytes = "0") {
  return {
    repositoryId,
    generationId,
    parentGenerationId: null,
    activeGeneration: true,
    structuralFreshness: "current",
    semanticFreshness: "current",
    tier: "tier_a",
    coverageStatus: "complete",
    skippedInputs: "0",
    usage: {
      rows: "1",
      edges: "1",
      results: "1",
      sourceBytes,
      jsonBytes: "128",
      estimatedTokens: "32",
      tokenAccountingProfile: null,
      memoryBytes: "256",
      elapsedMicros: "10",
    },
  };
}

function complete() {
  return {
    state: "complete",
    limitingResources: [],
    continuation: "not_applicable",
    guidance: [],
  };
}
