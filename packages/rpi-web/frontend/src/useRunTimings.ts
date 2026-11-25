import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type RunTiming, type RunTimingsPage } from "./api";
import type { Row } from "./transcript";

type TimingResult = {
  binding: string;
  values: RunTiming[];
};

type TimingState = {
  binding: string;
  cursor?: string;
  values: Map<string, RunTiming>;
};

function isUnsupportedTimingError(error: unknown) {
  return error instanceof ApiError && [404, 405, 501].includes(error.status);
}

/**
 * Shared bounded, cancellable reader for supplemental native timing records.
 * Transcript cursors are never touched.
 */
function useTimingQuery(
  sessionId: string,
  revision: string,
  ready: boolean,
  settled: number,
  key: string,
  body: { entryIds?: string[]; all?: boolean },
  accept: (timing: RunTiming) => boolean,
  enabled = true,
) {
  const binding = `${sessionId}:${revision}:${key}`;
  // `unsupported` is a property of the backend, not of one entry-ID selection:
  // a host that answered 404/405/501 for the previous page would answer the
  // same for the next one. Keying it by binding made every row change probe the
  // endpoint again, so pagination replayed a lookup the host already refused.
  const support = useRef({
    binding: `${sessionId}:${revision}`,
    unsupported: false,
  });
  if (support.current.binding !== `${sessionId}:${revision}`)
    support.current = {
      binding: `${sessionId}:${revision}`,
      unsupported: false,
    };
  const cache = useRef<TimingState>({
    binding,
    values: new Map(),
  });
  const [result, setResult] = useState<TimingResult>({
    binding,
    values: [],
  });
  const nextBody = useRef(body);
  nextBody.current = body;
  const acceptTiming = useRef(accept);
  acceptTiming.current = accept;

  const run = useCallback(
    async (state: TimingState, controller: AbortController) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const delay = (ms: number) =>
        new Promise<void>((resolve) => {
          const done = () => {
            clearTimeout(timer);
            controller.signal.removeEventListener("abort", done);
            resolve();
          };
          timer = setTimeout(done, ms);
          controller.signal.addEventListener("abort", done, { once: true });
        });
      const publish = () => {
        if (!controller.signal.aborted && cache.current === state)
          setResult({ binding, values: [...state.values.values()] });
      };
      let resets = 0;
      const ids = nextBody.current.entryIds ?? [];
      const all = nextBody.current.all === true;
      // `all` is a single session-wide query. The row selector walks batches of
      // at most 400 entry IDs because the server rejects larger lists. Each
      // batch is its own request body: sending the union would exceed that
      // bound and would also ask the server for rows this batch does not own.
      const requests: Array<{ entryIds?: string[]; all?: boolean }> = all
        ? [{ all: true }]
        : Array.from({ length: Math.ceil(ids.length / 400) }, (_, index) => ({
            entryIds: ids.slice(index * 400, index * 400 + 400),
          }));
      // A conflict restarts at the first batch; every other path advances by one.
      for (
        let start = 0;
        start < requests.length && !controller.signal.aborted;
      ) {
        const batch = requests[start];
        let previousBytes = -1;
        let slices = 0;
        let failures = 0;
        let restart = false;
        while (!controller.signal.aborted) {
          try {
            const page: RunTimingsPage = await api.runTimings(
              sessionId,
              batch,
              state.cursor,
              controller.signal,
            );
            if (controller.signal.aborted || cache.current !== state) return;
            if (
              !page ||
              !Array.isArray(page.timings) ||
              typeof page.complete !== "boolean"
            )
              return;
            state.cursor = page.cursor ?? undefined;
            for (const timing of page.timings) {
              if (timing && acceptTiming.current(timing) && validTiming(timing))
                state.values.set(timing.runId, timing);
            }
            publish();
            if (page.complete) break;
            // At an unfinished native trailing line, wait rather than busy-loop.
            const stalled = page.scannedBytes === previousBytes;
            previousBytes = page.scannedBytes;
            if (stalled && ++failures > 3) return;
            await delay(stalled ? 1000 : ++slices % 8 === 0 ? 150 : 20);
          } catch (error) {
            if (controller.signal.aborted || cache.current !== state) return;
            if (error instanceof ApiError && error.status === 409) {
              state.values.clear();
              state.cursor = undefined;
              publish();
              // Every conflict invalidates displayed data, even once the
              // bounded restart budget is exhausted.
              if (resets++ >= 2) return;
              start = 0;
              restart = true;
              break;
            }
            if (isUnsupportedTimingError(error))
              support.current.unsupported = true;
            // No invented local timer fallback. Transient unavailability may be
            // retried at the next settlement/navigation/older-page boundary.
            return;
          } finally {
            if (timer) clearTimeout(timer);
          }
        }
        // A conflict reset needs every earlier batch queried again too.
        if (restart) continue;
        start += 1;
      }
    },
    [binding, key, sessionId],
  );

  useEffect(() => {
    if (cache.current.binding !== binding)
      cache.current = { binding, values: new Map() };
    const state = cache.current;
    if (!sessionId || !ready || !enabled || support.current.unsupported) return;
    if (!nextBody.current.all && !nextBody.current.entryIds?.length) return;
    const controller = new AbortController();
    void run(state, controller);
    return () => {
      controller.abort();
    };
  }, [binding, key, sessionId, ready, enabled, settled, run]);
  return result.binding === binding ? result.values : [];
}

/** Supplemental native timing lookups never change transcript/SSE cursors. */
export function useRunTimings(
  sessionId: string,
  rows: Row[],
  revision: string,
  ready: boolean,
  settled: number,
) {
  const entryKey = JSON.stringify([
    ...new Set(rows.flatMap((row) => (row.entryId ? [row.entryId] : []))),
  ]);
  const entries: string[] = JSON.parse(entryKey);
  const key = `rows:${entryKey}`;
  const body = { entryIds: entries };
  const accept = useCallback(
    (timing: RunTiming) => entries.includes(timing.finalEntryId),
    // `entries` is derived from the stable serialized `entryKey`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [entryKey],
  );
  return useTimingQuery(
    sessionId,
    revision,
    ready,
    settled,
    key,
    body,
    accept,
    entries.length > 0,
  );
}

/**
 * Every finished native run in a session, including rounds outside the
 * current transcript window. `all` is a metadata scan, never a browser clock.
 */
export function useSessionRunTimings(
  sessionId: string,
  revision: string,
  ready: boolean,
  settled: number,
) {
  const key = "all";
  const body = { all: true };
  const accept = useCallback(() => true, []);
  return useTimingQuery(sessionId, revision, ready, settled, key, body, accept);
}

function validTiming(t: RunTiming) {
  return (
    typeof t.runId === "string" &&
    !!t.runId &&
    typeof t.finalEntryId === "string" &&
    Number.isSafeInteger(t.elapsedMs) &&
    t.elapsedMs >= 0 &&
    Number.isSafeInteger(t.startedAt) &&
    Number.isSafeInteger(t.finishedAt) &&
    t.finishedAt - t.startedAt === t.elapsedMs &&
    ["completed", "aborted", "failed", "declined"].includes(t.outcome)
  );
}

/** One footer on the last projected row for the native final message entry. */
export function timingByRow(rows: Row[], timings: RunTiming[]) {
  const lastRow = new Map<string, string>();
  for (const row of rows) if (row.entryId) lastRow.set(row.entryId, row.id);
  const result = new Map<string, RunTiming[]>();
  for (const timing of timings) {
    const id = lastRow.get(timing.finalEntryId);
    if (id) result.set(id, [...(result.get(id) ?? []), timing]);
  }
  return result;
}
