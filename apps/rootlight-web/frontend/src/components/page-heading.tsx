// Provides a compact, reusable heading for data-dense product routes.

import type { ReactNode } from "react";

export function PageHeading({
  actions,
  eyebrow,
  subtitle,
  title,
}: {
  actions?: ReactNode;
  eyebrow: string;
  subtitle: string;
  title: string;
}) {
  return (
    <div className="page-heading">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
        <p className="page-subtitle">{subtitle}</p>
      </div>
      {actions === undefined ? null : <div className="page-actions">{actions}</div>}
    </div>
  );
}
