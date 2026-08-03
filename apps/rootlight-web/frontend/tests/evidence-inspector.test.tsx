// Verifies explicit source disclosure, isolated failures, and typed impact interactions.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchNodeDetail,
  fetchRelationships,
  readSource,
  runChangeImpact,
} from "../src/api/client";
import {
  EvidenceInspector,
  EvidenceInspectorBoundary,
} from "../src/features/inspector/components/evidence-inspector";
import type {
  ChangeImpact,
  NodeDetail,
  Relationships,
  SourceRead,
} from "../src/features/inspector/model/evidence-contracts";

const repositoryId = `repo1_${"a".repeat(32)}`;
const generationId = `gen1_${"b".repeat(39)}`;
const symbolId = `sym1_${"c".repeat(39)}`;
const targetId = `sym1_${"d".repeat(39)}`;
const capability = "e".repeat(43);

vi.mock("../src/api/client", () => ({
  fetchNodeDetail: vi.fn(),
  fetchRelationships: vi.fn(),
  readSource: vi.fn(),
  runChangeImpact: vi.fn(),
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(fetchNodeDetail).mockResolvedValue(nodeDetail());
  vi.mocked(fetchRelationships).mockResolvedValue(relationships());
  vi.mocked(readSource).mockResolvedValue(sourceRead());
  vi.mocked(runChangeImpact).mockResolvedValue(changeImpact());
});

describe("EvidenceInspector", () => {
  it("loads source only after a click, renders it as text, and clears it explicitly", async () => {
    const view = renderInspector();

    expect(await screen.findByRole("heading", { name: "run" })).toBeVisible();
    expect(readSource).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Explicitly loaded source")).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Show source" }));
    const source = await screen.findByLabelText("Explicitly loaded source");
    expect(readSource).toHaveBeenCalledWith(
      {
        repositoryId,
        generationId,
        capability,
        encoding: "utf8",
      },
      expect.any(AbortSignal),
    );
    expect(source).toHaveTextContent("<img src=x onerror=repositoryAttack()>");
    expect(view.container.querySelector("img")).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Hide source" }));
    expect(screen.queryByLabelText("Explicitly loaded source")).not.toBeInTheDocument();
  });

  it("opens relationship targets and publishes a source-free impact overlay", async () => {
    const onOpenNode = vi.fn();
    const onImpactChange = vi.fn();
    renderInspector({ onImpactChange, onOpenNode });

    expect(await screen.findByText("calls")).toBeVisible();
    await userEvent.click(screen.getByText("calls"));
    await userEvent.click(screen.getByText(shortId(targetId)));
    expect(onOpenNode).toHaveBeenCalledWith(targetId);

    await userEvent.click(screen.getByRole("button", { name: "Calculate impact" }));
    expect(await screen.findByText("medium risk")).toBeVisible();
    expect(onImpactChange).toHaveBeenCalledWith([targetId]);
    expect(runChangeImpact).toHaveBeenCalledWith({
      repositoryId,
      generationId,
      changedSymbolIds: [symbolId],
      maximumDepth: 3,
      minimumConfidence: 500,
      includeTests: true,
    });
  });

  it("keeps the overview available when relationships fail and closes first on Escape", async () => {
    vi.mocked(fetchRelationships).mockRejectedValue(new Error("untrusted repository failure"));
    const onClose = vi.fn();
    renderInspector({ onClose });

    expect(await screen.findByText("provider")).toBeVisible();
    expect(await screen.findByText("Relationships could not be validated.")).toBeVisible();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(onClose).toHaveBeenCalledOnce();
    expect(screen.queryByText("untrusted repository failure")).not.toBeInTheDocument();
  });
});

function renderInspector(
  overrides: Partial<{
    onClose: () => void;
    onImpactChange: (symbolIds: readonly string[]) => void;
    onOpenNode: (stableId: string) => void;
  }> = {},
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const onClose = overrides.onClose ?? vi.fn();
  return render(
    <QueryClientProvider client={queryClient}>
      <EvidenceInspectorBoundary onClose={onClose}>
        <EvidenceInspector
          repositoryId={repositoryId}
          generationId={generationId}
          selectedNode={{
            ordinal: 0,
            stableId: symbolId,
            idKind: "symbol",
            label: "run",
            path: null,
            kind: "symbol",
            confidence: 900,
            generated: false,
            community: null,
            component: null,
            symbolCount: null,
            fanIn: null,
            fanOut: null,
            hotspotScore: null,
            evidence: "structural",
          }}
          relations={["calls"]}
          minimumConfidence={500}
          onClose={onClose}
          onOpenNode={overrides.onOpenNode ?? vi.fn()}
          onImpactChange={overrides.onImpactChange ?? vi.fn()}
        />
      </EvidenceInspectorBoundary>
    </QueryClientProvider>,
  );
}

function nodeDetail(): NodeDetail {
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
    provider: "provider",
    evidence: "definition",
    outboundExact: "1",
    outboundCandidates: "0",
    inboundExact: "0",
    inboundCandidates: "0",
    referenceCount: "1",
    generated: null,
    sourceReferences: [{ capability, expiresInSeconds: 60 }],
    context: context(),
    completeness: complete(),
  };
}

function relationships(): Relationships {
  return {
    schema: "rootlight.web-relationships/1",
    context: context(),
    groups: [
      {
        seedId: symbolId,
        relation: "calls",
        direction: "outbound",
        totalCount: "1",
        targets: [{ symbolId: targetId, confidence: 900, sourceReferences: [] }],
      },
    ],
    returnedEdges: "1",
    totalEdges: "1",
    exact: true,
    truncated: false,
    nextPageOffset: null,
    completeness: complete(),
  };
}

function sourceRead(): SourceRead {
  const content = "<img src=x onerror=repositoryAttack()>";
  const bytes = String(new TextEncoder().encode(content).byteLength);
  return {
    schema: "rootlight.web-source/1",
    repositoryId,
    generationId,
    chunks: [
      {
        fileId: `file1_${"f".repeat(39)}`,
        path: "src/untrusted.rs",
        requestedStartByte: "0",
        requestedEndByte: bytes,
        includedStartByte: "0",
        includedEndByte: bytes,
        includedStartLine: "1",
        includedEndLine: "1",
        content,
        encoding: "utf8",
        contentHash: `b3_${"a".repeat(58)}`,
        language: "rust",
        tier: "tier_a",
        generated: false,
      },
    ],
    totalSourceBytes: bytes,
    truncated: false,
    context: context(bytes),
    completeness: complete(),
  };
}

function changeImpact(): ChangeImpact {
  return {
    schema: "rootlight.web-change-impact/1",
    context: context(),
    resolvedChanges: [{ symbolId, fileId: null, classification: "resolved", kind: "function" }],
    impacted: [
      {
        sourceIndex: 0,
        dependents: [
          {
            symbolId: targetId,
            kind: "function",
            distance: 1,
            confidence: 900,
            via: ["calls"],
            isPublic: true,
          },
        ],
      },
    ],
    tests: [],
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
}

function context(sourceBytes = "0") {
  return {
    repositoryId,
    generationId,
    parentGenerationId: null,
    activeGeneration: true,
    structuralFreshness: "current" as const,
    semanticFreshness: "current" as const,
    tier: "tier_a" as const,
    coverageStatus: "complete" as const,
    skippedInputs: "0",
    usage: {
      rows: "1",
      edges: "1",
      results: "1",
      sourceBytes,
      jsonBytes: "128",
      estimatedTokens: "32",
      tokenAccountingProfile: null,
      memoryBytes: null,
      elapsedMicros: "10",
    },
  };
}

function complete() {
  return {
    state: "complete" as const,
    limitingResources: [],
    continuation: "not_applicable" as const,
    guidance: [],
  };
}

function shortId(identifier: string) {
  return `${identifier.slice(0, 13)}…${identifier.slice(-4)}`;
}
