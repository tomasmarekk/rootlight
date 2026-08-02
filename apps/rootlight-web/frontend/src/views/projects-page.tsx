// Presents the local repository catalog entry point and its bounded empty state.

import { Button } from "@heroui/react/button";
import { Card } from "@heroui/react/card";
import { Archive, CircleCheck, FolderPlus, RefreshCw, Search, TriangleAlert } from "lucide-react";

import { PageHeading } from "../components/page-heading";
import { StatusCard } from "../components/status-card";

export function ProjectsPage() {
  return (
    <div className="content-container">
      <PageHeading
        eyebrow="Local repository catalog"
        title="Projects"
        subtitle="Structural and semantic indexes available to this Rootlight daemon."
        actions={
          <>
            <Button size="sm" variant="ghost">
              <RefreshCw size={15} aria-hidden="true" />
              Refresh
            </Button>
            <Button size="sm" variant="primary">
              <FolderPlus size={15} aria-hidden="true" />
              Add project
            </Button>
          </>
        }
      />

      <section className="metrics-grid" aria-label="Project catalog summary">
        <StatusCard
          icon={<Archive size={17} />}
          label="Catalog"
          value="Local"
          detail="Account-private index inventory"
        />
        <StatusCard
          icon={<CircleCheck size={17} />}
          label="Ready"
          value="—"
          detail="Awaiting catalog snapshot"
        />
        <StatusCard
          icon={<RefreshCw size={17} />}
          label="Indexing"
          value="—"
          detail="No active snapshot loaded"
        />
        <StatusCard
          icon={<TriangleAlert size={17} />}
          label="Attention"
          value="—"
          detail="No complete count available"
        />
      </section>

      <section className="catalog-panel" aria-labelledby="catalog-heading">
        <div className="catalog-toolbar">
          <div>
            <h2 id="catalog-heading">Repository indexes</h2>
            <p>Search and open an immutable published generation.</p>
          </div>
          <label className="search-control">
            <Search size={16} aria-hidden="true" />
            <span className="sr-only">Search projects</span>
            <input type="search" placeholder="Search projects or repository ID" />
            <kbd>/</kbd>
          </label>
        </div>

        <Card className="empty-state-card" variant="secondary">
          <Card.Content>
            <div className="empty-state-icon" aria-hidden="true">
              <FolderPlus size={22} />
            </div>
            <p className="eyebrow">Catalog ready</p>
            <Card.Title>No projects have been loaded yet</Card.Title>
            <Card.Description>
              Add a local repository to build a bounded structural index. Source stays on this
              machine and is accessed only through the Rootlight daemon.
            </Card.Description>
            <Button size="sm" variant="primary">
              <FolderPlus size={15} aria-hidden="true" />
              Add your first project
            </Button>
          </Card.Content>
        </Card>
      </section>
    </div>
  );
}
