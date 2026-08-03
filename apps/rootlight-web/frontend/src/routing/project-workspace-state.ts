// Keeps project exploration state reproducible without admitting secrets or local paths to the URL.

import type {
  GraphBudgetProfile,
  GraphNodeKind,
  GraphRelationKind,
  GraphView,
} from "../features/graph/model/graph-contracts";

export type ProjectWorkspaceState = {
  generation: string;
  view: GraphView;
  nodeKinds: Exclude<GraphNodeKind, "unknown">[];
  relations: Exclude<GraphRelationKind, "unknown">[];
  language?: string;
  minConfidence: 0 | 250 | 500 | 750;
  includeInferred: boolean;
  includeGenerated: boolean;
  selected?: string;
  labels: boolean;
  budgetProfile: GraphBudgetProfile;
};

const generationPattern = /^gen1_[a-z2-7]{39}$/u;
const selectedPattern = /^(file1_|sym1_)[a-z2-7]{39}$/u;
const languagePattern = /^[A-Za-z0-9_+.#-]{1,32}$/u;
const graphViews = new Set<GraphView>(["architecture", "files", "symbols", "neighborhood"]);
const nodeKinds = new Set<Exclude<GraphNodeKind, "unknown">>(["file", "symbol"]);
export const projectGraphRelationKinds = [
  "calls",
  "called_by",
  "references",
  "types",
  "implements",
  "imports",
  "tests",
  "ownership",
  "service_call",
  "calls_route",
  "messaging",
  "reads_table",
  "writes_table",
  "build_dependency",
  "data_flow",
  "history",
] as const satisfies readonly Exclude<GraphRelationKind, "unknown">[];
const relationKindSet = new Set<Exclude<GraphRelationKind, "unknown">>(projectGraphRelationKinds);
const budgetProfiles = new Set<GraphBudgetProfile>(["compact", "balanced", "expanded"]);

export const defaultProjectWorkspaceState: ProjectWorkspaceState = {
  generation: "active",
  view: "architecture",
  nodeKinds: ["file", "symbol"],
  relations: [...projectGraphRelationKinds],
  minConfidence: 0,
  includeInferred: false,
  includeGenerated: true,
  labels: true,
  budgetProfile: "balanced",
};

export function parseProjectWorkspaceState(parameters: URLSearchParams): ProjectWorkspaceState {
  const generation = parseGeneration(parameters.get("generation"));
  const selected = parseSelected(parameters.get("selected"));
  const requestedView = parseEnum(parameters.get("view"), graphViews) ?? "architecture";
  const view =
    (requestedView === "symbols" || requestedView === "neighborhood") &&
    !selected?.startsWith("sym1_")
      ? "architecture"
      : requestedView;
  const parsedNodeKinds = uniqueClosed(parameters.getAll("node"), nodeKinds);
  const parsedRelations = uniqueClosed(parameters.getAll("relation"), relationKindSet);
  return {
    generation,
    view,
    nodeKinds: parsedNodeKinds.length === 0 ? ["file", "symbol"] : parsedNodeKinds,
    relations: parsedRelations.length === 0 ? [...projectGraphRelationKinds] : parsedRelations,
    language: parseLanguage(parameters.get("language")),
    minConfidence: parseMinimumConfidence(parameters.get("min_confidence")),
    includeInferred: parseBoolean(parameters.get("include_inferred"), false),
    includeGenerated: parseBoolean(parameters.get("include_generated"), true),
    selected,
    labels: parseBoolean(parameters.get("labels"), true),
    budgetProfile: parseEnum(parameters.get("budget"), budgetProfiles) ?? "balanced",
  };
}

export function serializeProjectWorkspaceState(state: ProjectWorkspaceState): URLSearchParams {
  const parameters = new URLSearchParams();
  parameters.set("generation", parseGeneration(state.generation));
  parameters.set("view", state.view);
  for (const kind of uniqueClosed(state.nodeKinds, nodeKinds)) {
    parameters.append("node", kind);
  }
  for (const relation of uniqueClosed(state.relations, relationKindSet)) {
    parameters.append("relation", relation);
  }
  if (state.language !== undefined && languagePattern.test(state.language)) {
    parameters.set("language", state.language);
  }
  parameters.set("min_confidence", String(state.minConfidence));
  parameters.set("include_inferred", String(state.includeInferred));
  parameters.set("include_generated", String(state.includeGenerated));
  if (state.selected !== undefined && selectedPattern.test(state.selected)) {
    parameters.set("selected", state.selected);
  }
  parameters.set("labels", String(state.labels));
  parameters.set("budget", state.budgetProfile);
  return parameters;
}

export function workspaceStateEquals(
  left: ProjectWorkspaceState,
  right: ProjectWorkspaceState,
): boolean {
  return (
    left.generation === right.generation &&
    left.view === right.view &&
    left.language === right.language &&
    left.minConfidence === right.minConfidence &&
    left.includeInferred === right.includeInferred &&
    left.includeGenerated === right.includeGenerated &&
    left.selected === right.selected &&
    left.labels === right.labels &&
    left.budgetProfile === right.budgetProfile &&
    left.nodeKinds.join("\0") === right.nodeKinds.join("\0") &&
    left.relations.join("\0") === right.relations.join("\0")
  );
}

function parseGeneration(value: string | null): string {
  return value === "active" || (value !== null && generationPattern.test(value)) ? value : "active";
}

function parseSelected(value: string | null): string | undefined {
  return value !== null && selectedPattern.test(value) ? value : undefined;
}

function parseLanguage(value: string | null): string | undefined {
  return value !== null && languagePattern.test(value) ? value : undefined;
}

function parseMinimumConfidence(value: string | null): 0 | 250 | 500 | 750 {
  switch (value) {
    case "250":
      return 250;
    case "500":
      return 500;
    case "750":
      return 750;
    default:
      return 0;
  }
}

function parseBoolean(value: string | null, fallback: boolean): boolean {
  if (value === "true") {
    return true;
  }
  if (value === "false") {
    return false;
  }
  return fallback;
}

function parseEnum<Value extends string>(
  value: string | null,
  accepted: ReadonlySet<Value>,
): Value | undefined {
  return value !== null && accepted.has(value as Value) ? (value as Value) : undefined;
}

function uniqueClosed<Value extends string>(
  values: readonly string[],
  accepted: ReadonlySet<Value>,
): Value[] {
  const unique = new Set<Value>();
  for (const value of values) {
    if (accepted.has(value as Value)) {
      unique.add(value as Value);
    }
  }
  return [...unique];
}
