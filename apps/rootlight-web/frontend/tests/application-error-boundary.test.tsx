// Verifies unexpected render failures stay inside a source-free recovery surface.

import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ApplicationErrorBoundary } from "../src/components/application-error-boundary";

describe("ApplicationErrorBoundary", () => {
  it("replaces exception details with a bounded recovery action", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);

    render(
      <ApplicationErrorBoundary>
        <BrokenView />
      </ApplicationErrorBoundary>,
    );

    expect(
      screen.getByRole("heading", { name: "Rootlight could not render this view" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Reload interface" })).toBeVisible();
    expect(screen.queryByText("private repository content")).not.toBeInTheDocument();
  });
});

function BrokenView(): never {
  throw new Error("private repository content");
}
