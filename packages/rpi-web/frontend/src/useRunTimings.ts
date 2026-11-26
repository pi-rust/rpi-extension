import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  ApiError,
  type ActiveRunTiming,
  type RunTiming,
  type RunTimingsPage,
} from "./api";
import type { Row } from "./transcript";

type TimingState = {
  binding: string;
  cursor?: string;
  values: Map<string, RunTiming>;
  /** Keyed by lane: a lane runs at most one operation at a time. */
  active: Map<string, ActiveRunTiming>;
  now: number | null;
  anchoredAt: number | null;
};

type TimingSnapshot = {
  values: RunTiming[];
  activeRuns: ActiveRunTiming[];
  now: number | null;
  anchoredAt: number | null;
};

const EMPTY: TimingSnapshot = {
  values: [],
  activeRuns: [],
  now: null,
  anchoredAt: null,
};

/**
 * While a run is in flight, re-read the timing page on this cadence. The
 * displayed elapsed time advances locally from the last server anchor, so this
 * interval only bounds how late a start/finish is noticed, not tick smoothness.
 */
const LIVE_POLL_MS = 1500;
/**
 * Stop after this many consecutive polls that report nothing in flight. The
 * start record is written before the model call but after the UI sees
 * `agent_start`, so a few misses are expected; an unbounded loop is not.
 */
const LIVE_IDLE_POLLS = 5;
/** Hard bound on one live session so a lost settlement cannot poll forever. */
const LIVE_POLL_BUDGET = 2400;

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
  /** The UI believes a run is in flight, so keep polling for its anchor. */
  live = false,
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
    active: new Map(),
    now: null,
    anchoredAt: null,
  });
  const [result, setResult] = useState<{
    binding: string;
    snapshot: TimingSnapshot;
  }>({ binding, snapshot: EMPTY });
  const nextBody = useRef(body);
  nextBody.current = body;
  const acceptTiming = useRef(accept);
  acceptTiming.current = accept;
  const liveRef = useRef(live);
  liveRef.current = live;

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
          setResult({
            binding,
            snapshot: {
              values: [...state.values.values()],
              activeRuns: [...state.active.values()],
              now: state.now,
              anchoredAt: state.anchoredAt,
            },
          });
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
      let polls = 0;
      let idlePolls = 0;
      for (;;) {
        let restarted = false;
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
                if (
                  timing &&
                  acceptTiming.current(timing) &&
                  validTiming(timing)
                )
                  state.values.set(timing.runId, timing);
              }
              // An older backend omits both fields; keep the previous anchor
              // rather than inventing one from the browser clock.
              if (Array.isArray(page.activeRuns) && validClock(page.now)) {
                state.active = new Map(
                  page.activeRuns
                    .filter(validActiveRun)
                    .map((run) => [run.lane, run]),
                );
                state.now = page.now;
                state.anchoredAt = performance.now();
                idlePolls = state.active.size ? 0 : idlePolls + 1;
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
                state.active.clear();
                state.now = null;
                state.anchoredAt = null;
                state.cursor = undefined;
                publish();
                // Every conflict invalidates displayed data, even once the
                // bounded restart budget is exhausted.
                if (resets++ >= 2) return;
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
          // A conflict reset re-queries every earlier batch from the first one.
          if (restart) {
            restarted = true;
            break;
          }
          start += 1;
        }
        if (controller.signal.aborted) return;
        // The cursor was dropped, so the batch walk starts over without
        // consulting the live-phase gate below.
        if (restarted) continue;
        if (resets > 2) return;
        // Only a session-wide (`all`) reader has a live phase; a row reader ends
        // once its batches are done.
        if (!all) return;
        // A live run has no settlement to trigger the next read, so poll while
        // either the transcript reports a run or the server still reports one in
        // flight. `delay` resolves on abort as well, which makes the cleanup
        // below the only way out.
        if (!liveRef.current && state.active.size === 0) return;
        // The run's own settlement is what ends the normal loop. These bounds
        // only stop a session whose settlement event was lost.
        if (idlePolls >= LIVE_IDLE_POLLS || ++polls > LIVE_POLL_BUDGET) return;
        await delay(LIVE_POLL_MS);
        if (controller.signal.aborted) return;
      }
    },
    [binding, key, sessionId],
  );

  useEffect(() => {
    if (cache.current.binding !== binding)
      cache.current = {
        binding,
        values: new Map(),
        active: new Map(),
        now: null,
        anchoredAt: null,
      };
    const state = cache.current;
    if (!sessionId || !ready || !enabled || support.current.unsupported) return;
    if (!nextBody.current.all && !nextBody.current.entryIds?.length) return;
    const controller = new AbortController();
    void run(state, controller);
    return () => {
      controller.abort();
    };
  }, [binding, key, sessionId, ready, enabled, settled, live, run]);
  return result.binding === binding ? result.snapshot : EMPTY;
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
  ).values;
}

/**
 * Every finished native run in a session, including rounds outside the
 * current transcript window, plus any run still in flight. `all` is a metadata
 * scan, never a browser stopwatch: the live part is anchored to a server clock.
 */
export function useSessionRunTimings(
  sessionId: string,
  revision: string,
  ready: boolean,
  settled: number,
  /** Poll for an in-flight run; a finished run needs no extra request. */
  live = false,
): TimingSnapshot {
  const key = "all";
  const body = { all: true };
  const accept = useCallback(() => true, []);
  return useTimingQuery(
    sessionId,
    revision,
    ready,
    settled,
    key,
    body,
    accept,
    true,
    live,
  );
}

function validClock(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function validActiveRun(run: ActiveRunTiming) {
  return (
    !!run &&
    typeof run.lane === "string" &&
    !!run.lane &&
    typeof run.runId === "string" &&
    !!run.runId &&
    Number.isSafeInteger(run.startedAt) &&
    run.startedAt >= 0
  );
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

/** Sum of every finished native run, or null when none is known yet. */
export function finishedWorkMs(timings: RunTiming[]): number | null {
  return timings.length
    ? timings.reduce(
        (total, timing) =>
          total + (Number.isFinite(timing.elapsedMs) ? timing.elapsedMs : 0),
        0,
      )
    : null;
}

/**
 * In-flight work at one instant. The server anchor supplies both the native
 * start and the clock those timestamps belong to, so the local part is only the
 * monotonic time elapsed since the response arrived.
 */
export function activeWorkMs(
  activeRuns: ActiveRunTiming[],
  now: number | null,
  anchoredAt: number | null,
  clock: number,
) {
  if (!activeRuns.length || now == null || anchoredAt == null) return 0;
  const current = now + Math.max(0, clock - anchoredAt);
  return activeRuns.reduce(
    (total, run) => total + Math.max(0, current - run.startedAt),
    0,
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
