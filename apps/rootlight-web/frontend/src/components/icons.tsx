// Draws the application's small navigation and action glyphs from owned SVG primitives.
// Geometry is authored here so the browser does not ship an external icon runtime.

import type { ReactNode, SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number | string };

function icon(shape: ReactNode) {
  return function Icon({ size = 24, strokeWidth = 2, ...props }: IconProps) {
    return (
      <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={strokeWidth}
        strokeLinecap="round"
        strokeLinejoin="round"
        {...props}
      >
        {shape}
      </svg>
    );
  };
}

/** Shared first-party glyphs for the browser's compact controls and status labels. */
export const Activity = icon(<polyline points="2 12 6 12 9 5 13 19 16 12 22 12" />);
export const Archive = icon(
  <>
    <rect x="3" y="4" width="18" height="4" rx="1" />
    <path d="M5 8v12h14V8M10 13h4" />
  </>,
);
export const ArrowLeft = icon(<path d="m11 5-7 7 7 7M4 12h16" />);
export const ArrowRight = icon(<path d="m13 5 7 7-7 7M20 12H4" />);
export const Boxes = icon(
  <>
    <rect x="3" y="3" width="8" height="8" rx="1" />
    <rect x="13" y="3" width="8" height="8" rx="1" />
    <rect x="8" y="13" width="8" height="8" rx="1" />
  </>,
);
export const Braces = icon(
  <path d="M9 3H7a2 2 0 0 0-2 2v4a3 3 0 0 1-2 3 3 3 0 0 1 2 3v4a2 2 0 0 0 2 2h2m6-18h2a2 2 0 0 1 2 2v4a3 3 0 0 0 2 3 3 3 0 0 0-2 3v4a2 2 0 0 1-2 2h-2" />,
);
export const Check = icon(<path d="m4 12 5 5L20 6" />);
export const ChevronRight = icon(<path d="m9 5 7 7-7 7" />);
export const CircleCheck = icon(
  <>
    <circle cx="12" cy="12" r="9" />
    <path d="m8 12 3 3 5-6" />
  </>,
);
export const CircleX = icon(
  <>
    <circle cx="12" cy="12" r="9" />
    <path d="m9 9 6 6m0-6-6 6" />
  </>,
);
export const Clock3 = icon(
  <>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 6v6h5" />
  </>,
);
export const Command = icon(
  <>
    <path d="M9 9h6v6H9z" />
    <path d="M9 9V6a2 2 0 1 0-2 2h2m6 1V6a2 2 0 1 1 2 2h-2m-6 7v3a2 2 0 1 1-2-2h2m6-1v3a2 2 0 1 0 2-2h-2" />
  </>,
);
export const Copy = icon(
  <>
    <rect x="8" y="8" width="12" height="12" rx="2" />
    <path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" />
  </>,
);
export const Database = icon(
  <>
    <ellipse cx="12" cy="5" rx="8" ry="3" />
    <path d="M4 5v14c0 2 3.6 3 8 3s8-1 8-3V5M4 12c0 2 3.6 3 8 3s8-1 8-3" />
  </>,
);
export const Download = icon(<path d="M12 3v13m-5-5 5 5 5-5M4 18v3h16v-3" />);
export const ExternalLink = icon(<path d="M13 4h7v7m0-7-9 9M19 14v6H4V5h6" />);
export const EyeOff = icon(
  <>
    <path d="M3 3 21 21M10 6.2a11 11 0 0 1 2-.2c4 0 7.5 2.1 10 6a16 16 0 0 1-3.1 3.5M6 8C4.4 9 3 10.4 2 12c2.5 3.9 6 6 10 6 1 0 2-.1 3-.4" />
    <path d="M10 10a3 3 0 0 0 4 4" />
  </>,
);
export const FileCode2 = icon(
  <>
    <path d="M13 3H5v18h14V9l-6-6Zm0 0v6h6M9 13l-2 2 2 2m6-4 2 2-2 2" />
  </>,
);
export const Folder = icon(<path d="M3 6h7l2 2h9v12H3z" />);
export const FolderOpen = icon(<path d="M3 10V6h7l2 2h9v3M2 20l3-9h17l-3 9z" />);
export const FolderPlus = icon(
  <>
    <path d="M3 6h7l2 2h9v12H3z" />
    <path d="M12 11v6m-3-3h6" />
  </>,
);
export const FolderGit2 = icon(
  <>
    <path d="M3 6h7l2 2h9v12H3z" />
    <circle cx="9" cy="12" r="1" />
    <circle cx="9" cy="17" r="1" />
    <circle cx="16" cy="15" r="1" />
    <path d="M9 13v3m1-1h5" />
  </>,
);
export const Gauge = icon(
  <>
    <path d="M4 18a9 9 0 1 1 16 0H4Zm8-4 4-5" />
    <circle cx="12" cy="14" r="1" />
  </>,
);
export const GitBranch = icon(
  <>
    <circle cx="6" cy="5" r="2" />
    <circle cx="6" cy="19" r="2" />
    <circle cx="18" cy="8" r="2" />
    <path d="M6 7v10m0-4c0-3 3-5 10-5" />
  </>,
);
export const HardDrive = icon(
  <>
    <path d="M5 5h14l3 13H2L5 5Zm-3 13v2h20v-2" />
    <circle cx="17" cy="17" r="1" />
  </>,
);
export const Info = icon(
  <>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v5m0-8h.01" />
  </>,
);
export const LoaderCircle = icon(<path d="M20 12a8 8 0 1 1-8-8" />);
export const LogOut = icon(<path d="M10 4H4v16h6m2-8h10m-4-4 4 4-4 4M16 4h4v4m0 8v4h-4" />);
export const Network = icon(
  <>
    <circle cx="12" cy="5" r="2" />
    <circle cx="5" cy="18" r="2" />
    <circle cx="19" cy="18" r="2" />
    <path d="M11 7 6 16m7-9 5 9M7 18h10" />
  </>,
);
export const Pencil = icon(<path d="m4 20 4-.8L20 7a2 2 0 0 0-3-3L5 16l-1 4Zm2-4 2 3M15 6l3 3" />);
export const Play = icon(<path d="m8 5 11 7-11 7V5Z" />);
export const Radio = icon(
  <>
    <circle cx="12" cy="12" r="2" />
    <path d="M7 7a7 7 0 0 0 0 10m10-10a7 7 0 0 1 0 10M4 4a11 11 0 0 0 0 16M20 4a11 11 0 0 1 0 16" />
  </>,
);
export const RefreshCw = icon(
  <path d="M20 8a8 8 0 0 0-14-2L3 9m0-5v5h5m-4 7a8 8 0 0 0 14 2l3-3m0 5v-5h-5" />,
);
export const RotateCcw = icon(<path d="M4 10a8 8 0 1 1 0 5M4 4v6h6" />);
export const Search = icon(
  <>
    <circle cx="10.5" cy="10.5" r="6.5" />
    <path d="m15.5 15.5 5 5" />
  </>,
);
export const ShieldCheck = icon(
  <path d="M12 2 3 6v6c0 5 3.4 8.4 9 10 5.6-1.6 9-5 9-10V6l-9-4Zm-4 10 3 3 5-5" />,
);
export const Square = icon(<rect x="4" y="4" width="16" height="16" rx="2" />);
export const Trash2 = icon(<path d="M4 7h16m-2 0-1 14H7L6 7m3 0V4h6v3m-5 4v6m4-6v6" />);
export const TriangleAlert = icon(
  <>
    <path d="M12 3 2 21h20L12 3Zm0 6v5m0 3h.01" />
  </>,
);
export const X = icon(<path d="M5 5 19 19M19 5 5 19" />);
