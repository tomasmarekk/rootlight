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

  useLayoutEffect(() => {
    const dialog = dialogReference.current;
    if (dialog === null) {
      return;
    }
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
