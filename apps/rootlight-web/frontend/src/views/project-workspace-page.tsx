// Reserves the graph workspace route while preserving repository identity boundaries.

import { Button } from "@heroui/react/button";
import { ArrowLeft, Network, RotateCcw } from "lucide-react";
import { Link, useParams } from "react-router";

export function ProjectWorkspacePage() {
  const { repositoryId = "unknown" } = useParams();
  return (
    <div className="workspace-frame">
      <header className="project-header">
        <div>
          <Link className="back-link" to="/projects">
            <ArrowLeft size={14} aria-hidden="true" />
            Projects
          </Link>
          <h1>Project workspace</h1>
          <code>{repositoryId}</code>
        </div>
        <Button size="sm" variant="ghost">
          <RotateCcw size={15} aria-hidden="true" />
          Reset view
        </Button>
      </header>
      <div className="workspace-grid">
        <aside className="workspace-rail">
          <p className="eyebrow">Generation overview</p>
          <h2>Projection controls</h2>
          <p>Filters and coverage controls become available after the generation is resolved.</p>
        </aside>
        <section className="graph-placeholder" aria-label="Graph visualization">
          <Network size={30} aria-hidden="true" />
          <h2>Graph projection not loaded</h2>
          <p>The canvas will open only after a bounded immutable projection is negotiated.</p>
        </section>
      </div>
    </div>
  );
}
