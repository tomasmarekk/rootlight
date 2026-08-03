// Keeps modal focus, inertness, and dismissal in the browser top layer without runtime styles.

import { useLayoutEffect, useRef, type MouseEvent, type ReactNode } from "react";

export type NativeDialogProps = {
  ariaLabelledBy: string;
  children: ReactNode;
  className: string;
  isDismissable?: boolean;
  isOpen: boolean;
  onDismiss: () => void;
};

/** Controls a native modal dialog while preserving React-owned open state. */
export function NativeDialog({
  ariaLabelledBy,
  children,
  className,
  isDismissable = true,
  isOpen,
  onDismiss,
}: NativeDialogProps) {
  const dialogReference = useRef<HTMLDialogElement>(null);
  const restoreFocusReference = useRef<HTMLElement>(null);

  useLayoutEffect(() => {
    const dialog = dialogReference.current;
    if (dialog === null) {
      return;
    }
    const activeElement = document.activeElement;
    restoreFocusReference.current =
      activeElement instanceof HTMLElement && activeElement !== document.body
        ? activeElement
        : null;
    if (!dialog.open) {
      if (typeof dialog.showModal === "function") {
        dialog.showModal();
      } else {
        // jsdom has no top-layer implementation, but the open state remains unit-testable.
        dialog.setAttribute("open", "");
      }
    }
    return () => {
      if (!dialog.open) {
        return;
      }
      if (typeof dialog.close === "function") {
        dialog.close();
      } else {
        dialog.removeAttribute("open");
      }
      // Controlled unmounting can precede the browser's native focus restoration.
      const restoreTarget = restoreFocusReference.current;
      restoreFocusReference.current = null;
      if (restoreTarget?.isConnected) {
        restoreTarget.focus({ preventScroll: true });
      }
    };
  }, [isOpen]);

  function dismissFromBackdrop(event: MouseEvent<HTMLDialogElement>) {
    if (isDismissable && event.target === event.currentTarget) {
      onDismiss();
    }
  }

  return isOpen ? (
    <dialog
      ref={dialogReference}
      aria-labelledby={ariaLabelledBy}
      aria-modal="true"
      className={`native-dialog ${className}`}
      onCancel={(event) => {
        event.preventDefault();
        if (isDismissable) {
          onDismiss();
        }
      }}
      onMouseDown={dismissFromBackdrop}
    >
      <div data-slot="modal-dialog">{children}</div>
    </dialog>
  ) : null;
}
