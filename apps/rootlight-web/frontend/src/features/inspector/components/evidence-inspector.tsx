// Presents bounded evidence while keeping source bodies in short-lived component memory only.

import { Button } from "@heroui/react/button";
import { useInfiniteQuery, useMutation, useQuery } from "@tanstack/react-query";
import {
  Braces,
  EyeOff,
  FileCode2,
  GitBranch,
  Network,
  RefreshCw,
  TriangleAlert,
  X,
} from "lucide-react";
import { Component, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import {
  fetchNodeDetail,
  fetchRelationships,
  readSource,
  runChangeImpact,
} from "../../../api/client";
import type { BrowserGraphNode, GraphRelationKind } from "../../graph/model/graph-contracts";
import type {
  ChangeImpact,
  EvidenceCompleteness,
  RelationshipGroup,
  SourceRead,
} from "../model/evidence-contracts";

export type EvidenceInspectorProps = {
  repositoryId: string;
  generationId: string;
  selectedNode: BrowserGraphNode;
  relations: readonly Exclude<GraphRelationKind, "unknown">[];
  minimumConfidence: number;
  onClose: () => void;
  onOpenNode: (stableId: string) => void;
  onImpactChange: (symbolIds: readonly string[]) => void;
};

type BoundaryState = {
  failed: boolean;
};

/** Isolates unexpected inspector rendering failures from the retained graph. */
export class EvidenceInspectorBoundary extends Component<
  { children: ReactNode; onClose: () => void },
  BoundaryState
> {
  public override state: BoundaryState = { failed: false };

  public static getDerivedStateFromError(): BoundaryState {
    return { failed: true };
  }

  public override componentDidCatch() {
    // Repository-derived evidence may be present in exception values, so it is never logged here.
  }

  public override render() {
    if (!this.state.failed) {
      return this.props.children;
    }
    return (
      <aside className="evidence-inspector evidence-inspector--failed" aria-label="Node inspector">
        <TriangleAlert size={24} aria-hidden="true" />
        <h2>Inspector is unavailable</h2>
        <p>The graph remains available. Close this panel and select the node again to retry.</p>
        <Button size="sm" variant="primary" onPress={this.props.onClose}>
          Close inspector
        </Button>
      </aside>
    );
  }
}

/** Renders exact-generation node evidence, relationships, source, and change impact. */
export function EvidenceInspector({
  generationId,
  minimumConfidence,
  onClose,
  onImpactChange,
  onOpenNode,
  relations,
  repositoryId,
  selectedNode,
}: EvidenceInspectorProps) {
  const heading = useRef<HTMLHeadingElement>(null);
  const sourceAbort = useRef<AbortController | null>(null);
  const [source, setSource] = useState<SourceRead | null>(null);
  const [sourceEncoding, setSourceEncoding] = useState<"utf8" | "bytes_base64">("utf8");
  const [sourceLoading, setSourceLoading] = useState(false);
  const [sourceFailed, setSourceFailed] = useState(false);
  const [maximumDepth, setMaximumDepth] = useState(3);
  const [includeTests, setIncludeTests] = useState(true);
  const symbolSelected =
    selectedNode.idKind === "symbol" && selectedNode.stableId.startsWith("sym1_");
  const relationFingerprint = relations.join("\u001f");
  const detail = useQuery({
    queryKey: ["node-detail", repositoryId, generationId, selectedNode.stableId],
    queryFn: ({ signal }) =>
      fetchNodeDetail(repositoryId, generationId, selectedNode.stableId, signal),
    enabled: symbolSelected,
    retry: 1,
    refetchOnWindowFocus: false,
  });
  const relationships = useInfiniteQuery({
    queryKey: [
      "relationships",
      repositoryId,
      generationId,
      selectedNode.stableId,
      relationFingerprint,
      minimumConfidence,
    ],
    queryFn: ({ pageParam, signal }) =>
      fetchRelationships(
        {
          repositoryId,
          generationId,
          seedIds: [selectedNode.stableId],
          relations: [...relations],
          direction: "both",
          minimumConfidence,
          pageOffset: pageParam,
        },
        signal,
      ),
    initialPageParam: "0",
    getNextPageParam: (page) => page.nextPageOffset ?? undefined,
    enabled: symbolSelected && relations.length > 0,
    retry: 1,
    refetchOnWindowFocus: false,
  });
  const impact = useMutation({
    mutationFn: () =>
      runChangeImpact({
        repositoryId,
        generationId,
        changedSymbolIds: [selectedNode.stableId],
        maximumDepth,
        minimumConfidence,
        includeTests,
      }),
  });
  const relationshipGroups = useMemo(
    () => relationships.data?.pages.flatMap((page) => page.groups) ?? [],
    [relationships.data],
  );
  const relationshipCompleteness = relationships.data?.pages.at(-1)?.completeness;

  useEffect(() => {
    heading.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", closeOnEscape, true);
    return () => {
      window.removeEventListener("keydown", closeOnEscape, true);
      sourceAbort.current?.abort();
      onImpactChange([]);
    };
  }, [onClose, onImpactChange]);

  async function loadSource() {
    sourceAbort.current?.abort();
    const abortController = new AbortController();
    sourceAbort.current = abortController;
    setSource(null);
    setSourceFailed(false);
    setSourceLoading(true);
    try {
      // Refreshing after the explicit action avoids attempting an expired one-use capability.
      const refreshed = await detail.refetch({ cancelRefetch: true });
      const capability = refreshed.data?.sourceReferences[0]?.capability;
      if (capability === undefined) {
        throw new Error("Definition source is unavailable");
      }
      const result = await readSource(
        {
          repositoryId,
          generationId,
          capability,
          encoding: sourceEncoding,
        },
        abortController.signal,
      );
      setSource(result);
    } catch (error) {
      if (!(error instanceof DOMException && error.name === "AbortError")) {
        setSourceFailed(true);
      }
    } finally {
      if (sourceAbort.current === abortController) {
        setSourceLoading(false);
      }
    }
  }

  async function calculateImpact() {
    try {
      const result = await impact.mutateAsync();
      onImpactChange(impactSymbolIds(result));
    } catch {
      onImpactChange([]);
    }
  }

  return (
    <aside
      className="evidence-inspector"
      aria-labelledby="evidence-inspector-title"
      data-selected-kind={selectedNode.idKind}
    >
      <header className="evidence-inspector__header">
        <div>
          <p className="eyebrow">Node inspector</p>
          <h2 id="evidence-inspector-title" ref={heading} tabIndex={-1}>
            {detail.data?.displayName ?? selectedNode.label}
          </h2>
          <code title={selectedNode.stableId}>{shortId(selectedNode.stableId)}</code>
        </div>
        <Button
          aria-label="Close node inspector"
          isIconOnly
          size="sm"
          variant="ghost"
          onPress={onClose}
        >
          <X size={16} aria-hidden="true" />
        </Button>
      </header>

      <div className="evidence-inspector__generation">
        <span>Exact generation</span>
        <code title={generationId}>{shortId(generationId)}</code>
      </div>

      <section className="inspector-section" aria-labelledby="node-overview-title">
        <div className="inspector-section__heading">
          <Braces size={15} aria-hidden="true" />
          <h3 id="node-overview-title">Overview</h3>
        </div>
        <dl className="inspector-facts">
          <Fact label="Graph kind" value={selectedNode.kind} />
          <Fact label="Graph evidence" value={selectedNode.evidence} />
          <Fact label="Graph confidence" value={formatConfidence(selectedNode.confidence)} />
          {detail.data === undefined ? null : (
            <>
              <Fact label="Language" value={detail.data.language} />
              <Fact label="Analysis tier" value={detail.data.tier} />
              <Fact label="Provider" value={detail.data.provider} />
              <Fact label="Evidence" value={detail.data.evidence} />
              <Fact label="Confidence" value={formatConfidence(detail.data.confidence)} />
              <Fact label="References" value={detail.data.referenceCount} />
            </>
          )}
        </dl>
        {detail.data?.signature === null || detail.data?.signature === undefined ? null : (
          <pre className="inspector-signature">
            <code>{detail.data.signature}</code>
          </pre>
        )}
        {!symbolSelected ? (
          <p className="inspector-note">
            Detailed evidence is available for symbol nodes. This file node remains selectable in
            the graph and companion list.
          </p>
        ) : detail.isPending ? (
          <SectionStatus label="Loading node evidence" />
        ) : detail.isError ? (
          <SectionFailure
            label="Node evidence could not be validated."
            onRetry={() => void detail.refetch()}
          />
        ) : (
          <CompletenessSummary completeness={detail.data.completeness} />
        )}
      </section>

      {symbolSelected ? (
        <>
          <section className="inspector-section" aria-labelledby="relationships-title">
            <div className="inspector-section__heading">
              <Network size={15} aria-hidden="true" />
              <h3 id="relationships-title">Relationships</h3>
            </div>
            {relationships.isPending ? (
              <SectionStatus label="Loading relationship groups" />
            ) : relationships.isError ? (
              <SectionFailure
                label="Relationships could not be validated."
                onRetry={() => void relationships.refetch()}
              />
            ) : relationshipGroups.length === 0 ? (
              <p className="inspector-note">
                No relationships matched the current bounded filters.
              </p>
            ) : (
              <RelationshipGroups groups={relationshipGroups} onOpenNode={onOpenNode} />
            )}
            {relationships.hasNextPage ? (
              <Button
                isDisabled={relationships.isFetchingNextPage}
                size="sm"
                variant="ghost"
                onPress={() => void relationships.fetchNextPage()}
              >
                {relationships.isFetchingNextPage ? (
                  <RefreshCw className="spin" size={13} aria-hidden="true" />
                ) : null}
                Load next relationship page
              </Button>
            ) : null}
            {relationshipCompleteness === undefined ? null : (
              <CompletenessSummary completeness={relationshipCompleteness} />
            )}
          </section>

          <section className="inspector-section" aria-labelledby="source-title">
            <div className="inspector-section__heading">
              <FileCode2 size={15} aria-hidden="true" />
              <h3 id="source-title">Definition source</h3>
            </div>
            <p className="inspector-note">
              Source is fetched only after this explicit action and is cleared when the inspector,
              node, generation, or session changes.
            </p>
            {source === null ? (
              <div className="source-actions">
                <label>
                  Presentation
                  <select
                    value={sourceEncoding}
                    onChange={(event) => {
                      setSourceEncoding(event.currentTarget.value as typeof sourceEncoding);
                    }}
                  >
                    <option value="utf8">UTF-8 text</option>
                    <option value="bytes_base64">Base64 bytes</option>
                  </select>
                </label>
                <Button
                  isDisabled={
                    sourceLoading ||
                    detail.data === undefined ||
                    detail.data.sourceReferences.length === 0
                  }
                  size="sm"
                  variant="primary"
                  onPress={() => void loadSource()}
                >
                  {sourceLoading ? (
                    <RefreshCw className="spin" size={13} aria-hidden="true" />
                  ) : (
                    <FileCode2 size={13} aria-hidden="true" />
                  )}
                  Show source
                </Button>
              </div>
            ) : (
              <SourceBlock source={source} onHide={() => setSource(null)} />
            )}
            {sourceFailed ? (
              <p className="inspector-error" role="alert">
                Source could not be read with the selected presentation. Request a fresh read to
                retry.
              </p>
            ) : null}
            {detail.data?.sourceReferences.length === 0 ? (
              <p className="inspector-note">This node has no definition source reference.</p>
            ) : null}
          </section>

          <section className="inspector-section" aria-labelledby="impact-title">
            <div className="inspector-section__heading">
              <GitBranch size={15} aria-hidden="true" />
              <h3 id="impact-title">Change impact</h3>
            </div>
            <div className="impact-controls">
              <label>
                Maximum depth
                <select
                  value={maximumDepth}
                  onChange={(event) => setMaximumDepth(Number(event.currentTarget.value))}
                >
                  {[1, 2, 3, 4, 5, 6, 7, 8].map((depth) => (
                    <option key={depth} value={depth}>
                      {depth}
                    </option>
                  ))}
                </select>
              </label>
              <label className="impact-checkbox">
                <input
                  type="checkbox"
                  checked={includeTests}
                  onChange={(event) => setIncludeTests(event.currentTarget.checked)}
                />
                Include tests
              </label>
              <Button
                isDisabled={impact.isPending}
                size="sm"
                variant="primary"
                onPress={() => void calculateImpact()}
              >
                {impact.isPending ? (
                  <RefreshCw className="spin" size={13} aria-hidden="true" />
                ) : (
                  <GitBranch size={13} aria-hidden="true" />
                )}
                Calculate impact
              </Button>
            </div>
            {impact.isError ? (
              <p className="inspector-error" role="alert">
                Change impact could not be calculated for this immutable generation.
              </p>
            ) : impact.data === undefined ? (
              <p className="inspector-note">
                Run the typed Rootlight intent to highlight returned dependents in the graph.
              </p>
            ) : (
              <ImpactResult impact={impact.data} onOpenNode={onOpenNode} />
            )}
          </section>
        </>
      ) : null}
    </aside>
  );
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd>{humanize(value)}</dd>
    </div>
  );
}

function SectionStatus({ label }: { label: string }) {
  return (
    <p className="inspector-status" role="status">
      <RefreshCw className="spin" size={13} aria-hidden="true" />
      {label}
    </p>
  );
}

function SectionFailure({ label, onRetry }: { label: string; onRetry: () => void }) {
  return (
    <div className="inspector-section-failure" role="alert">
      <TriangleAlert size={14} aria-hidden="true" />
      <span>{label}</span>
      <Button size="sm" variant="ghost" onPress={onRetry}>
        Retry
      </Button>
    </div>
  );
}

function RelationshipGroups({
  groups,
  onOpenNode,
}: {
  groups: readonly RelationshipGroup[];
  onOpenNode: (stableId: string) => void;
}) {
  return (
    <div className="relationship-groups">
      {groups.map((group, index) => (
        <details key={`${group.seedId}:${group.relation}:${group.direction}:${String(index)}`}>
          <summary>
            <span>{humanize(group.relation)}</span>
            <small>
              {humanize(group.direction)} · {group.targets.length} of {group.totalCount}
            </small>
          </summary>
          <ul>
            {group.targets.map((target) => (
              <li key={target.symbolId}>
                <button type="button" onClick={() => onOpenNode(target.symbolId)}>
                  <code>{shortId(target.symbolId)}</code>
                  <span>{formatConfidence(target.confidence)}</span>
                  <small>
                    {target.sourceReferences.length === 0
                      ? "No source evidence"
                      : `${String(target.sourceReferences.length)} source evidence`}
                  </small>
                </button>
              </li>
            ))}
          </ul>
        </details>
      ))}
    </div>
  );
}

function SourceBlock({ onHide, source }: { onHide: () => void; source: SourceRead }) {
  const chunk = source.chunks[0];
  if (chunk === undefined) {
    return (
      <div className="source-empty">
        <p>No source bytes were returned.</p>
        <Button size="sm" variant="ghost" onPress={onHide}>
          Hide source
        </Button>
      </div>
    );
  }
  return (
    <div className="source-block">
      <div className="source-block__header">
        <div>
          <strong>{chunk.path}</strong>
          <span>
            Lines {chunk.includedStartLine ?? "?"}–{chunk.includedEndLine ?? "?"} ·{" "}
            {source.totalSourceBytes} bytes · {chunk.encoding}
          </span>
        </div>
        <Button aria-label="Hide source" isIconOnly size="sm" variant="ghost" onPress={onHide}>
          <EyeOff size={14} aria-hidden="true" />
        </Button>
      </div>
      <pre tabIndex={0} aria-label="Explicitly loaded source">
        <code>{chunk.content}</code>
      </pre>
      <CompletenessSummary completeness={source.completeness} />
    </div>
  );
}

function ImpactResult({
  impact,
  onOpenNode,
}: {
  impact: ChangeImpact;
  onOpenNode: (stableId: string) => void;
}) {
  const entries = impact.impacted.flatMap((group) => group.dependents);
  return (
    <div className="impact-result">
      <div className="impact-summary" role="status">
        <strong>{humanize(impact.riskSummary.level)} risk</strong>
        <span>
          {entries.length} returned dependents · fanout {impact.riskSummary.fanout}
        </span>
        <small>
          {humanize(impact.riskSummary.coverage)}
          {impact.riskSummary.breakingSurface ? " · public breaking surface" : ""}
          {impact.riskSummary.dynamicBlindSpots ? " · dynamic blind spots" : ""}
        </small>
      </div>
      {impact.riskSummary.reasons.length === 0 ? null : (
        <ul className="impact-reasons">
          {impact.riskSummary.reasons.map((reason) => (
            <li key={reason}>{humanize(reason)}</li>
          ))}
        </ul>
      )}
      <ul className="impact-entries">
        {entries.map((entry) => (
          <li key={`${entry.symbolId}:${String(entry.distance)}`}>
            <button type="button" onClick={() => onOpenNode(entry.symbolId)}>
              <code>{shortId(entry.symbolId)}</code>
              <span>
                distance {entry.distance} · {formatConfidence(entry.confidence)}
              </span>
              <small>
                {entry.via.map(humanize).join(" → ") || humanize(entry.kind)}
                {entry.isPublic ? " · public" : ""}
              </small>
            </button>
          </li>
        ))}
      </ul>
      {impact.tests.length === 0 ? null : (
        <details className="impact-tests">
          <summary>Relevant tests · {impact.tests.length}</summary>
          <ul>
            {impact.tests.map((test) => (
              <li key={test.testId}>
                <strong>{test.testId}</strong>
                <span>{formatConfidence(test.relevance)} relevance</span>
                <small>{test.why.map(humanize).join(" · ")}</small>
              </li>
            ))}
          </ul>
        </details>
      )}
      <CompletenessSummary completeness={impact.completeness} />
    </div>
  );
}

function CompletenessSummary({ completeness }: { completeness: EvidenceCompleteness }) {
  return (
    <div className={`completeness-summary completeness-summary--${completeness.state}`}>
      <strong>{humanize(completeness.state)}</strong>
      {completeness.limitingResources.length === 0 ? (
        <span>No limiting resource reported.</span>
      ) : (
        <ul>
          {completeness.limitingResources.map((resource, index) => (
            <li key={`${resource.kind}:${String(index)}`}>
              {humanize(resource.kind)}
              {resource.limit === null ? "" : ` · limit ${resource.limit}`}
              {resource.observed === null ? "" : ` · observed ${resource.observed}`}
            </li>
          ))}
        </ul>
      )}
      {completeness.guidance.length === 0 ? null : (
        <small>{completeness.guidance.map(humanize).join(" · ")}</small>
      )}
    </div>
  );
}

function impactSymbolIds(impact: ChangeImpact): string[] {
  return [
    ...new Set(impact.impacted.flatMap((group) => group.dependents.map((entry) => entry.symbolId))),
  ];
}

function formatConfidence(confidence: number) {
  return `${String(Math.round(confidence / 10))}%`;
}

function shortId(identifier: string) {
  return identifier.length > 18 ? `${identifier.slice(0, 13)}…${identifier.slice(-4)}` : identifier;
}

function humanize(value: string) {
  return value.replaceAll("_", " ");
}
