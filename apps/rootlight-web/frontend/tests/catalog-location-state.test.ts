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
    [],
    { schema: "rootlight.catalog-navigation/2", catalog: {} },
    { schema: "rootlight.catalog-navigation/1", catalog: null },
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

  it.each([
    null,
    "cursor",
    { snapshot: "", after: "cursor", sortVersion: 1 },
    { snapshot: "x".repeat(129), after: "cursor", sortVersion: 1 },
    { snapshot: "snapshot", after: "x".repeat(2_049), sortVersion: 1 },
    { snapshot: "snapshot", after: null, sortVersion: 1 },
    { snapshot: "snapshot", after: "cursor", sortVersion: "1" },
    { snapshot: "snapshot", after: "cursor", sortVersion: 1.5 },
    { snapshot: "snapshot", after: "cursor", sortVersion: 0 },
    { snapshot: "snapshot", after: "cursor", sortVersion: 1_001 },
  ])("rejects an invalid cursor without retaining partial history: %j", (cursor) => {
    const validCursor = { snapshot: "snapshot", after: "cursor", sortVersion: 1 };
    expect(
      parseCatalogLocationState({
        schema: "rootlight.catalog-navigation/1",
        catalog: {
          searchInput: "",
          query: "",
          stateFilter: "all",
          history: [validCursor, cursor],
        },
      }),
    ).toBeUndefined();
  });

  it("accepts exact size boundaries without sharing mutable history", () => {
    const value = createCatalogLocationState({
      searchInput: "x".repeat(256),
      query: "q".repeat(256),
      stateFilter: "all",
      history: Array.from({ length: 100 }, () => ({
        snapshot: "s".repeat(128),
        after: "c".repeat(2_048),
        sortVersion: 1_000,
      })),
    });
    const parsed = parseCatalogLocationState(value);

    expect(parsed).toEqual(value);
    expect(parsed?.catalog.history).not.toBe(value.catalog.history);
    expect(parsed?.catalog.history[0]).not.toBe(value.catalog.history[0]);
    value.catalog.history.splice(0);
    expect(parsed?.catalog.history).toHaveLength(100);
  });
});
