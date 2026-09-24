// Owns short-lived browser query state without a third-party cache runtime.
// Session clearing, aborts, and immutable query keys keep source capabilities scoped.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

type QueryKey = readonly unknown[];
type Retry = number | false;
type QueryFn<T> = (context: { signal: AbortSignal }) => Promise<T>;
type QueryOptions<T> = {
  queryKey: QueryKey;
  queryFn: QueryFn<T>;
  enabled?: boolean;
  gcTime?: number;
  staleTime?: number;
  retry?: Retry;
  retryDelay?: (attempt: number) => number;
  refetchInterval?: number | false;
  refetchOnWindowFocus?: false;
};
type QueryDefaults = Pick<
  QueryOptions<unknown>,
  "gcTime" | "staleTime" | "retry" | "refetchOnWindowFocus"
>;
type QueryResultBase<T> = {
  isFetching: boolean;
  refetch: (options?: { cancelRefetch?: boolean }) => Promise<{ data?: T; error?: unknown }>;
};

/** A browser query result that distinguishes missing data from retained stale data. */
export type UseQueryResult<T> = QueryResultBase<T> &
  (
    | { data: undefined; error: undefined; isPending: true; isError: false }
    | { data: undefined; error: unknown; isPending: false; isError: true }
    | { data: T; error: unknown; isPending: false; isError: boolean }
  );

type QueryState = {
  data: unknown;
  error: unknown;
  isFetching: boolean;
  fetchedAt: number;
  invalidated: boolean;
};
type QueryEntry = {
  key: QueryKey;
  hash: string;
  state: QueryState;
  listeners: Set<() => void>;
  fetchers: Set<() => Promise<unknown>>;
  controller?: AbortController;
  pending?: Promise<unknown>;
  sequence: number;
  gcTimer?: ReturnType<typeof setTimeout>;
};

const EMPTY_STATE: QueryState = {
  data: undefined,
  error: undefined,
  isFetching: false,
  fetchedAt: 0,
  invalidated: false,
};

function stableKey(value: unknown): string {
  if (value === undefined) return "undefined";
  if (value === null) return "null";
  if (Array.isArray(value)) return `[${value.map(stableKey).join(",")}]`;
  if (typeof value === "object") {
    return `{${Object.entries(value)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, child]) => `${JSON.stringify(key)}:${stableKey(child)}`)
      .join(",")}}`;
  }
  return `${typeof value}:${JSON.stringify(value)}`;
}

function retryCount(value: Retry | undefined): number {
  return value === false ? 0 : Math.max(0, value ?? 1);
}

function abortableDelay(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Query request was aborted", "AbortError"));
      return;
    }
    const timer = window.setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, milliseconds);
    function abort() {
      window.clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      reject(new DOMException("Query request was aborted", "AbortError"));
    }
    signal.addEventListener("abort", abort, { once: true });
  });
}

/** A bounded, session-owned cache for the Rootlight browser application. */
export class QueryClient {
  private readonly entries = new Map<string, QueryEntry>();
  private readonly clearListeners = new Set<() => void>();
  readonly defaults: QueryDefaults;

  /** Creates a cache with the application's stale and retention policy. */
  constructor(
    options: { defaultOptions?: { queries?: QueryDefaults; mutations?: { retry?: Retry } } } = {},
  ) {
    this.defaults = options.defaultOptions?.queries ?? {};
  }

  /** Returns one stable entry for a structural query key. */
  entry(key: QueryKey): QueryEntry {
    const hash = stableKey(key);
    let entry = this.entries.get(hash);
    if (entry === undefined) {
      entry = {
        key,
        hash,
        state: EMPTY_STATE,
        listeners: new Set(),
        fetchers: new Set(),
        sequence: 0,
      };
      this.entries.set(hash, entry);
    }
    return entry;
  }

  /** Subscribes to immutable snapshots and releases inactive entries after their GC window. */
  subscribe(entry: QueryEntry, listener: () => void, gcTime: number): () => void {
    if (entry.gcTimer !== undefined) window.clearTimeout(entry.gcTimer);
    entry.gcTimer = undefined;
    entry.listeners.add(listener);
    return () => {
      entry.listeners.delete(listener);
      if (entry.listeners.size !== 0) return;
      this.abort(entry);
      entry.gcTimer = window.setTimeout(() => {
        if (entry.listeners.size === 0 && this.entries.get(entry.hash) === entry) {
          this.entries.delete(entry.hash);
        }
      }, gcTime);
    };
  }

  /** Registers an active fetcher so invalidation refreshes mounted queries. */
  observe(entry: QueryEntry, fetcher: () => Promise<unknown>): () => void {
    entry.fetchers.add(fetcher);
    return () => entry.fetchers.delete(fetcher);
  }

  /** Registers cleanup for work such as cursor fetches outside the shared query request. */
  onClear(listener: () => void): () => void {
    this.clearListeners.add(listener);
    return () => this.clearListeners.delete(listener);
  }

  private publish(entry: QueryEntry, state: QueryState): void {
    entry.state = state;
    for (const listener of entry.listeners) listener();
  }

  private abort(entry: QueryEntry): void {
    entry.sequence += 1;
    entry.controller?.abort();
    entry.controller = undefined;
    entry.pending = undefined;
    if (entry.state.isFetching) this.publish(entry, { ...entry.state, isFetching: false });
  }

  /** Fetches once per key, preserving stale data if refresh fails. */
  fetch<T>(
    entry: QueryEntry,
    queryFn: QueryFn<T>,
    options: QueryOptions<T>,
    force = false,
  ): Promise<T> {
    if (entry.pending !== undefined && !force) return entry.pending as Promise<T>;
    if (force && entry.pending !== undefined) this.abort(entry);
    const controller = new AbortController();
    const sequence = ++entry.sequence;
    entry.controller = controller;
    this.publish(entry, { ...entry.state, isFetching: true });
    const task = (async () => {
      for (let attempt = 0; ; attempt += 1) {
        try {
          const data = await queryFn({ signal: controller.signal });
          if (controller.signal.aborted || entry.sequence !== sequence) {
            throw new DOMException("Query request was aborted", "AbortError");
          }
          if (entry.sequence === sequence) {
            this.publish(entry, {
              data,
              error: undefined,
              isFetching: false,
              fetchedAt: Date.now(),
              invalidated: false,
            });
          }
          return data;
        } catch (error) {
          if (controller.signal.aborted || entry.sequence !== sequence) throw error;
          if (attempt >= retryCount(options.retry ?? this.defaults.retry)) {
            this.publish(entry, { ...entry.state, error, isFetching: false });
            throw error;
          }
          const delay = options.retryDelay?.(attempt) ?? Math.min(1_000 * 2 ** attempt, 30_000);
          await abortableDelay(delay, controller.signal);
        }
      }
    })();
    entry.pending = task;
    void task.then(
      () => {
        if (entry.sequence === sequence) entry.pending = undefined;
      },
      () => {
        if (entry.sequence === sequence) entry.pending = undefined;
      },
    );
    return task;
  }

  /** Invalidates matching entries and awaits refreshes for mounted observers. */
  async invalidateQueries(options: {
    queryKey?: QueryKey;
    predicate?: (query: { queryKey: QueryKey }) => boolean;
  }): Promise<void> {
    const refreshes: Promise<unknown>[] = [];
    for (const entry of this.entries.values()) {
      if (
        options.queryKey !== undefined &&
        !options.queryKey.every((part, index) => stableKey(part) === stableKey(entry.key[index]))
      ) {
        continue;
      }
      if (options.predicate !== undefined && !options.predicate({ queryKey: entry.key })) continue;
      this.publish(entry, { ...entry.state, invalidated: true });
      const fetcher = entry.fetchers.values().next().value;
      if (fetcher !== undefined && entry.listeners.size > 0) refreshes.push(fetcher());
    }
    await Promise.allSettled(refreshes);
  }

  /** Seeds or replaces exact-key data, including in mounted consumers. */
  setQueryData<T>(key: QueryKey, update: T | ((current: T | undefined) => T | undefined)): void {
    const entry = this.entry(key);
    const data =
      typeof update === "function"
        ? (update as (current: T | undefined) => T | undefined)(entry.state.data as T | undefined)
        : update;
    this.publish(entry, {
      data,
      error: undefined,
      isFetching: false,
      fetchedAt: Date.now(),
      invalidated: false,
    });
  }

  /** Retains prior data while reporting a page refresh failure. */
  recordError(key: QueryKey, error: unknown): void {
    const entry = this.entry(key);
    this.publish(entry, { ...entry.state, error, isFetching: false });
  }

  /** Aborts outstanding requests and removes all session-scoped cached data. */
  clear(): void {
    for (const listener of this.clearListeners) listener();
    for (const entry of this.entries.values()) {
      this.abort(entry);
      if (entry.gcTimer !== undefined) window.clearTimeout(entry.gcTimer);
      this.publish(entry, EMPTY_STATE);
    }
    this.entries.clear();
  }
}

/** Shared context for the browser's session-owned query client. */
export const QueryContext = createContext<QueryClient | null>(null);

/** Returns the cache bound to the current browser session. */
export function useQueryClient(): QueryClient {
  const client = useContext(QueryContext);
  if (client === null) throw new Error("QueryClientProvider is unavailable");
  return client;
}

/** Reads and refreshes a keyed request with abortable, shared cache state. */
export function useQuery<T>(options: QueryOptions<T>): UseQueryResult<T> {
  const client = useQueryClient();
  const entry = client.entry(options.queryKey);
  const current = useRef(options);
  useEffect(() => {
    current.current = options;
  });
  const gcTime = options.gcTime ?? client.defaults.gcTime ?? 5 * 60_000;
  const subscribe = useCallback(
    (listener: () => void) => client.subscribe(entry, listener, gcTime),
    [client, entry, gcTime],
  );
  const state = useSyncExternalStore(subscribe, () => entry.state);
  const refetch = useCallback(
    async (request?: { cancelRefetch?: boolean }) => {
      try {
        const data = await client.fetch(
          entry,
          (context) => current.current.queryFn(context),
          current.current,
          request?.cancelRefetch !== false,
        );
        return { data };
      } catch (error) {
        return { error };
      }
    },
    [client, entry],
  );
  useEffect(() => {
    if (options.enabled === false) return;
    const fetcher = (force = false) =>
      client.fetch(entry, (context) => current.current.queryFn(context), current.current, force);
    const unobserve = client.observe(entry, () => fetcher(true));
    const staleTime = current.current.staleTime ?? client.defaults.staleTime ?? 0;
    if (
      entry.state.data === undefined ||
      entry.state.invalidated ||
      Date.now() - entry.state.fetchedAt >= staleTime
    ) {
      void fetcher().catch(() => undefined);
    }
    return unobserve;
  }, [client, entry, options.enabled]);
  useEffect(() => {
    const interval = options.refetchInterval;
    if (options.enabled === false || interval === false || interval === undefined) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = () => {
      void client
        .fetch(entry, (context) => current.current.queryFn(context), current.current)
        .catch(() => undefined)
        .finally(() => {
          if (!stopped) timer = window.setTimeout(poll, interval);
        });
    };
    timer = window.setTimeout(poll, interval);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [client, entry, options.enabled, options.refetchInterval]);
  const data = state.data as T | undefined;
  return {
    data,
    error: state.error,
    isPending: data === undefined && state.error === undefined,
    isError: state.error !== undefined,
    isFetching: state.isFetching,
    refetch,
  } as UseQueryResult<T>;
}

type MutationOptions<T, V> = {
  mutationFn: (variables: V) => Promise<T>;
  onSuccess?: (data: T, variables: V) => void | Promise<void>;
};
type MutationState<T> =
  | { data: undefined; error: undefined; isPending: false; isError: false }
  | { data: undefined; error: undefined; isPending: true; isError: false }
  | { data: undefined; error: Error; isPending: false; isError: true }
  | { data: T; error: undefined; isPending: false; isError: false };

/** Runs one explicit request while retaining its most recent outcome. */
export function useMutation<T, V = void>(options: MutationOptions<T, V>) {
  const [state, setState] = useState<MutationState<T>>({
    data: undefined,
    error: undefined,
    isPending: false,
    isError: false,
  });
  const current = useRef(options);
  useEffect(() => {
    current.current = options;
  });
  const sequence = useRef(0);
  const reset = useCallback(() => {
    sequence.current += 1;
    setState({ data: undefined, error: undefined, isPending: false, isError: false });
  }, []);
  const mutateAsync = useCallback(async (variables?: V): Promise<T> => {
    const request = ++sequence.current;
    setState({ data: undefined, error: undefined, isPending: true, isError: false });
    try {
      const data = await current.current.mutationFn(variables as V);
      await current.current.onSuccess?.(data, variables as V);
      if (sequence.current === request) {
        setState({ data, error: undefined, isPending: false, isError: false });
      }
      return data;
    } catch (error) {
      if (sequence.current === request) {
        setState({
          data: undefined,
          error: error instanceof Error ? error : new Error(String(error)),
          isPending: false,
          isError: true,
        });
      }
      throw error;
    }
  }, []);
  const mutate = useCallback(
    (variables: V, callbacks?: { onSuccess?: (data: T) => void }) => {
      void mutateAsync(variables).then(
        (data) => callbacks?.onSuccess?.(data),
        () => undefined,
      );
    },
    [mutateAsync],
  );
  return {
    ...state,
    mutate,
    mutateAsync,
    reset,
  };
}

/** Reads cursor pages while preserving earlier evidence if a later page fails. */
export function useInfiniteQuery<T, P>(
  options: Omit<QueryOptions<{ pages: T[]; pageParams: P[] }>, "queryFn"> & {
    queryFn: (context: { pageParam: P; signal: AbortSignal }) => Promise<T>;
    initialPageParam: P;
    getNextPageParam: (page: T) => P | undefined;
  },
) {
  const client = useQueryClient();
  const current = useRef(options);
  useEffect(() => {
    current.current = options;
  });
  const [isFetchingNextPage, setFetchingNextPage] = useState(false);
  const pending = useRef(false);
  const controller = useRef<AbortController | null>(null);
  const query = useQuery({
    ...options,
    queryFn: async ({ signal }) => ({
      pages: [
        await current.current.queryFn({ pageParam: current.current.initialPageParam, signal }),
      ],
      pageParams: [current.current.initialPageParam],
    }),
  });
  useEffect(() => {
    const unsubscribe = client.onClear(() => controller.current?.abort());
    return () => {
      controller.current?.abort();
      unsubscribe();
    };
  }, [client, query.refetch]);
  const next = query.data?.pages.at(-1);
  const hasNextPage = next !== undefined && options.getNextPageParam(next) !== undefined;
  const fetchNextPage = useCallback(async () => {
    if (pending.current || current.current.enabled === false) return;
    const key = current.current.queryKey;
    const hash = stableKey(key);
    const data = client.entry(key).state.data as { pages: T[]; pageParams: P[] } | undefined;
    const last = data?.pages.at(-1);
    const pageParam = last === undefined ? undefined : current.current.getNextPageParam(last);
    if (pageParam === undefined || data === undefined) return;
    pending.current = true;
    setFetchingNextPage(true);
    const active = new AbortController();
    controller.current = active;
    try {
      const page = await current.current.queryFn({ pageParam, signal: active.signal });
      if (
        !active.signal.aborted &&
        hash === stableKey(current.current.queryKey) &&
        client.entry(key).state.data === data
      ) {
        client.setQueryData(key, {
          pages: [...data.pages, page],
          pageParams: [...data.pageParams, pageParam],
        });
      }
    } catch (error) {
      if (
        !active.signal.aborted &&
        hash === stableKey(current.current.queryKey) &&
        client.entry(key).state.data === data
      ) {
        client.recordError(key, error);
      }
    } finally {
      if (controller.current === active) controller.current = null;
      pending.current = false;
      setFetchingNextPage(false);
    }
  }, [client]);
  return { ...query, hasNextPage, fetchNextPage, isFetchingNextPage };
}
