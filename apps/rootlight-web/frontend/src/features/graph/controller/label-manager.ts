// Selects a bounded HTML label overlay from screen-space graph candidates.
// Priority and overlap rejection remain independent from React node count.

/** A screen-space candidate considered by the Atlas label budget. */
export type GraphLabelCandidate = {
  ordinal: number;
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  priority: number;
  selected: boolean;
  hovered: boolean;
  directNeighbor: boolean;
};

/** A safe label accepted for the current overlay frame. */
export type VisibleGraphLabel = GraphLabelCandidate & {
  clippedText: string;
};

/** Options controlling deterministic overlap rejection. */
export type GraphLabelSelectionOptions = {
  budget: number;
  maximumTextLength?: number;
  padding?: number;
};

/**
 * Selects highest-value labels within a strict budget and rejects screen-space overlap.
 *
 * Selected, hovered, and direct-neighbor candidates sort before metric priority.
 */
export function selectVisibleGraphLabels(
  candidates: readonly GraphLabelCandidate[],
  options: GraphLabelSelectionOptions,
): readonly VisibleGraphLabel[] {
  if (!Number.isSafeInteger(options.budget) || options.budget < 0) {
    throw new Error("Graph label budget must be a non-negative safe integer");
  }
  const maximumTextLength = options.maximumTextLength ?? 48;
  const padding = options.padding ?? 4;
  const sorted = [...candidates].sort(compareCandidates);
  const accepted: VisibleGraphLabel[] = [];
  for (const candidate of sorted) {
    if (accepted.length >= options.budget) {
      break;
    }
    const rectangle = paddedRectangle(candidate, padding);
    const overlaps = accepted.some((label) =>
      rectanglesOverlap(rectangle, paddedRectangle(label, padding)),
    );
    if (overlaps && !candidate.selected && !candidate.hovered) {
      continue;
    }
    accepted.push({
      ...candidate,
      clippedText: middleTruncate(candidate.text, maximumTextLength),
    });
  }
  return accepted;
}

function compareCandidates(left: GraphLabelCandidate, right: GraphLabelCandidate) {
  return (
    Number(right.selected) - Number(left.selected) ||
    Number(right.hovered) - Number(left.hovered) ||
    Number(right.directNeighbor) - Number(left.directNeighbor) ||
    right.priority - left.priority ||
    left.ordinal - right.ordinal
  );
}

function paddedRectangle(candidate: GraphLabelCandidate, padding: number) {
  return {
    left: candidate.x - padding,
    top: candidate.y - padding,
    right: candidate.x + candidate.width + padding,
    bottom: candidate.y + candidate.height + padding,
  };
}

function rectanglesOverlap(
  left: ReturnType<typeof paddedRectangle>,
  right: ReturnType<typeof paddedRectangle>,
) {
  return !(
    left.right <= right.left ||
    right.right <= left.left ||
    left.bottom <= right.top ||
    right.bottom <= left.top
  );
}

function middleTruncate(value: string, maximumLength: number) {
  if (value.length <= maximumLength) {
    return value;
  }
  const available = Math.max(2, maximumLength - 1);
  const prefixLength = Math.ceil(available / 2);
  const suffixLength = Math.floor(available / 2);
  return `${value.slice(0, prefixLength)}…${value.slice(-suffixLength)}`;
}
