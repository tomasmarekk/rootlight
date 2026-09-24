// Implements Rootlight's small set of native interface primitives.
// These components keep interaction semantics in HTML and styling in local CSS.

import type { ComponentProps } from "react";

type ButtonProps = Omit<ComponentProps<"button">, "onClick"> & {
  onPress?: () => void;
  variant?: "primary" | "ghost";
  size?: "sm";
  isDisabled?: boolean;
  isIconOnly?: boolean;
};

/** Renders a native button with Rootlight's bounded appearance variants. */
export function Button({
  children,
  className,
  disabled,
  isDisabled,
  isIconOnly,
  onPress,
  size,
  type = "button",
  variant = "primary",
  ...attributes
}: ButtonProps) {
  const classes = [
    "rl-button",
    `rl-button--${variant}`,
    size === "sm" ? "rl-button--sm" : undefined,
    isIconOnly ? "rl-button--icon" : undefined,
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      {...attributes}
      className={classes}
      disabled={disabled === true || isDisabled === true}
      onClick={onPress}
      type={type}
    >
      {children}
    </button>
  );
}

type ChipProps = ComponentProps<"span"> & {
  color?: "success" | "warning";
  size?: "sm";
  variant?: "soft";
};

/** Displays short, non-interactive status text without introducing a button role. */
export function Chip({
  children,
  className,
  color = "success",
  size,
  variant = "soft",
  ...attributes
}: ChipProps) {
  const classes = [
    "rl-chip",
    `rl-chip--${variant}`,
    `rl-chip--${color}`,
    size === "sm" ? "rl-chip--sm" : undefined,
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <span {...attributes} className={classes}>
      {children}
    </span>
  );
}

type CardProps = ComponentProps<"section"> & { variant?: "secondary" };

/** Groups related empty-state content under one semantic region. */
export function Card({ children, className, variant = "secondary", ...attributes }: CardProps) {
  return (
    <section
      {...attributes}
      className={["rl-card", `rl-card--${variant}`, className].filter(Boolean).join(" ")}
    >
      {children}
    </section>
  );
}

/** Preserves the styling slot for card contents. */
export function CardContent({ children, ...attributes }: ComponentProps<"div">) {
  return (
    <div {...attributes} data-slot="content">
      {children}
    </div>
  );
}

/** Renders the card heading at the level below the page title. */
export function CardTitle({ children, ...attributes }: ComponentProps<"h2">) {
  return (
    <h2 {...attributes} data-slot="title">
      {children}
    </h2>
  );
}

/** Renders supporting text for a card heading. */
export function CardDescription({ children, ...attributes }: ComponentProps<"p">) {
  return (
    <p {...attributes} data-slot="description">
      {children}
    </p>
  );
}
