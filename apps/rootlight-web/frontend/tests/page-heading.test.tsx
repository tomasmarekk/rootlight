// Verifies the optional action surface without coupling routes to heading markup.

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { PageHeading } from "../src/components/page-heading";

describe("PageHeading", () => {
  it("renders its required content without an empty action container", () => {
    const { container } = render(
      <PageHeading eyebrow="Workspace" subtitle="Inspect the active graph." title="Repository" />,
    );

    expect(screen.getByRole("heading", { name: "Repository" })).toBeVisible();
    expect(screen.getByText("Workspace")).toBeVisible();
    expect(screen.getByText("Inspect the active graph.")).toBeVisible();
    expect(container.querySelector(".page-actions")).not.toBeInTheDocument();
  });

  it("renders supplied actions", () => {
    render(
      <PageHeading
        actions={<button type="button">Refresh</button>}
        eyebrow="Workspace"
        subtitle="Inspect the active graph."
        title="Repository"
      />,
    );

    expect(screen.getByRole("button", { name: "Refresh" })).toBeVisible();
  });
});
