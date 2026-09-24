// Verifies native control semantics used across the local interface.

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

import { Button, Card, CardContent, CardDescription, CardTitle, Chip } from "../src/components/ui";

it("activates buttons through pointer and keyboard without submitting a form", async () => {
  const onPress = vi.fn();
  render(
    <form onSubmit={(event) => event.preventDefault()}>
      <Button onPress={onPress}>Open project</Button>
    </form>,
  );
  const button = screen.getByRole("button", { name: "Open project" });
  expect(button).toHaveAttribute("type", "button");
  await userEvent.click(button);
  button.focus();
  await userEvent.keyboard("{Enter}");
  expect(onPress).toHaveBeenCalledTimes(2);
});

it("does not activate disabled buttons and retains an accessible icon name", async () => {
  const onPress = vi.fn();
  render(
    <Button aria-label="End session" isDisabled isIconOnly size="sm" onPress={onPress}>
      ×
    </Button>,
  );
  const button = screen.getByRole("button", { name: "End session" });
  expect(button).toBeDisabled();
  expect(button).toHaveClass("rl-button--icon", "rl-button--sm");
  await userEvent.click(button);
  expect(onPress).not.toHaveBeenCalled();
});

it("keeps status and empty-state content semantic", () => {
  render(
    <>
      <Chip color="warning" size="sm" variant="soft">
        Reconnecting
      </Chip>
      <Card variant="secondary">
        <CardContent>
          <CardTitle>Catalog unavailable</CardTitle>
          <CardDescription>Try again after reconnecting.</CardDescription>
        </CardContent>
      </Card>
    </>,
  );
  expect(screen.getByText("Reconnecting").tagName).toBe("SPAN");
  expect(screen.getByText("Reconnecting")).toHaveClass("rl-chip--warning");
  expect(screen.getByRole("heading", { name: "Catalog unavailable" })).toHaveAttribute(
    "data-slot",
    "title",
  );
  expect(screen.getByText("Try again after reconnecting.")).toHaveAttribute(
    "data-slot",
    "description",
  );
});
