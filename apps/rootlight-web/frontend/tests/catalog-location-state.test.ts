// Protects route-carried catalog state from unbounded or malformed browser input.

import { describe, expect, it } from "vitest";

import {
  createCatalogLocationState,
  parseCatalogLocationState,
} from "../src/routing/catalog-location-state";

describe("catalog location state", () => {
  it("accepts a bounded state and returns an independent normalized value", () => {
    const value = createCatalogLocationState({
      searchInput: "root",
      query: "root",
      stateFilter: "ready",
      history: [{ snapshot: "snapshot", after: "cursor", sortVersion: 1 }],
    });

    expect(parseCatalogLocationState(value)).toEqual(value);
  });

  it.each([
    undefined,
    null,
    { schema: "rootlight.catalog-navigation/2", catalog: {} },
    {
      schema: "rootlight.catalog-navigation/1",
      catalog: { searchInput: "", query: "", stateFilter: "unknown", history: [] },
    },
    {
      schema: "rootlight.catalog-navigation/1",
      catalog: {
        searchInput: "x".repeat(257),
        query: "",
        stateFilter: "all",
        history: [],
      },
    },
    {
      schema: "rootlight.catalog-navigation/1",
      catalog: {
        searchInput: "",
        query: "",
        stateFilter: "all",
        history: [{ snapshot: "snapshot", after: "", sortVersion: 1 }],
      },
    },
    {
      schema: "rootlight.catalog-navigation/1",
      catalog: {
        searchInput: "",
        query: "",
        stateFilter: "all",
        history: Array.from({ length: 101 }, () => ({
          snapshot: "snapshot",
          after: "cursor",
          sortVersion: 1,
        })),
      },
    },
  ])("rejects malformed or unbounded browser state", (value) => {
    expect(parseCatalogLocationState(value)).toBeUndefined();
  });
});
