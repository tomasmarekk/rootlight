// Verifies controlled native-dialog dismissal without third-party overlay side effects.

import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { NativeDialog } from "../src/components/native-dialog";

describe("NativeDialog", () => {
  it("uses the modal top layer and removes closed content from the document", () => {
    const onDismiss = vi.fn();
    const { rerender } = render(
      <NativeDialog
        ariaLabelledBy="native-dialog-heading"
        className="test-dialog"
        isOpen
        onDismiss={onDismiss}
      >
        <h2 id="native-dialog-heading">Native confirmation</h2>
        <button type="button">Continue</button>
      </NativeDialog>,
    );

    const dialog = screen.getByRole("dialog", { name: "Native confirmation" });
    expect(dialog).toHaveAttribute("open");
    expect(dialog.querySelector("[style]")).toBeNull();

    const cancel = new Event("cancel", { bubbles: false, cancelable: true });
    fireEvent(dialog, cancel);
    expect(cancel.defaultPrevented).toBe(true);
    expect(onDismiss).toHaveBeenCalledOnce();

    rerender(
      <NativeDialog
        ariaLabelledBy="native-dialog-heading"
        className="test-dialog"
        isOpen={false}
        onDismiss={onDismiss}
      >
        <h2 id="native-dialog-heading">Native confirmation</h2>
      </NativeDialog>,
    );
    expect(screen.queryByText("Native confirmation")).not.toBeInTheDocument();
  });

  it("dismisses only direct backdrop interaction when dismissal is allowed", () => {
    const onDismiss = vi.fn();
    const { rerender } = render(
      <NativeDialog
        ariaLabelledBy="native-dialog-heading"
        className="test-dialog"
        isDismissable={false}
        isOpen
        onDismiss={onDismiss}
      >
        <h2 id="native-dialog-heading">Native confirmation</h2>
        <button type="button">Continue</button>
      </NativeDialog>,
    );
    const dialog = screen.getByRole("dialog", { name: "Native confirmation" });

    fireEvent.mouseDown(screen.getByRole("button", { name: "Continue" }));
    fireEvent.mouseDown(dialog);
    fireEvent(dialog, new Event("cancel", { cancelable: true }));
    expect(onDismiss).not.toHaveBeenCalled();

    rerender(
      <NativeDialog
        ariaLabelledBy="native-dialog-heading"
        className="test-dialog"
        isOpen
        onDismiss={onDismiss}
      >
        <h2 id="native-dialog-heading">Native confirmation</h2>
      </NativeDialog>,
    );
    fireEvent.mouseDown(screen.getByRole("dialog", { name: "Native confirmation" }));
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("restores focus to the opener after native cancellation", async () => {
    const user = userEvent.setup();

    function DialogFixture() {
      const [isOpen, setIsOpen] = useState(false);
      return (
        <>
          <button type="button" onClick={() => setIsOpen(true)}>
            Open confirmation
          </button>
          <NativeDialog
            ariaLabelledBy="focus-dialog-heading"
            className="test-dialog"
            isOpen={isOpen}
            onDismiss={() => setIsOpen(false)}
          >
            <h2 id="focus-dialog-heading">Focus confirmation</h2>
            <button type="button">Continue</button>
          </NativeDialog>
        </>
      );
    }

    render(<DialogFixture />);
    const opener = screen.getByRole("button", { name: "Open confirmation" });
    await user.click(opener);
    const dialog = screen.getByRole("dialog", { name: "Focus confirmation" });
    fireEvent(dialog, new Event("cancel", { cancelable: true }));

    expect(screen.queryByRole("dialog", { name: "Focus confirmation" })).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
  });
});
