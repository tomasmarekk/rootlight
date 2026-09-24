// Ensures every first-party glyph remains a usable, theme-aware inline SVG.

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import * as icons from "../src/components/icons";

describe("browser glyphs", () => {
  it("renders each glyph with the shared sizing and accessibility attributes", () => {
    for (const [name, Icon] of Object.entries(icons)) {
      const markup = renderToStaticMarkup(
        <Icon size={18} className="navigation-icon" aria-hidden="true" />,
      );
      expect(markup, name).toContain('width="18"');
      expect(markup, name).toContain('height="18"');
      expect(markup, name).toContain('viewBox="0 0 24 24"');
      expect(markup, name).toContain('stroke="currentColor"');
      expect(markup, name).toContain('class="navigation-icon"');
      expect(markup, name).toContain('aria-hidden="true"');
      expect(markup, name).toMatch(/<(path|rect|circle|ellipse|polyline)\b/u);
    }
  });
});
