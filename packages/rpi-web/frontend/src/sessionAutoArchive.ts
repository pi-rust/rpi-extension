// Browser-local preference for automatic session archival. Native session
// metadata stays the source of truth; this module only stores the UI choice.
export interface SessionAutoArchivePreference {
  enabled: boolean;
  days: number;
}

const STORAGE_KEY = "rpi-session-auto-archive";
const DEFAULT_PREFERENCE: SessionAutoArchivePreference = {
  enabled: true,
  days: 15,
};
export const MIN_SESSION_AUTO_ARCHIVE_DAYS = 1;
export const MAX_SESSION_AUTO_ARCHIVE_DAYS = 90;
export const SESSION_AUTO_ARCHIVE_STORAGE_KEY = STORAGE_KEY;

let fallback: SessionAutoArchivePreference | null = null;
let unpersisted = false;

export function normalizeSessionAutoArchiveDays(value: unknown): number {
  const days = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(days)) return DEFAULT_PREFERENCE.days;
  return Math.min(
    MAX_SESSION_AUTO_ARCHIVE_DAYS,
    Math.max(MIN_SESSION_AUTO_ARCHIVE_DAYS, Math.round(days)),
  );
}

export function readSessionAutoArchivePreference(): SessionAutoArchivePreference {
  if (unpersisted) return fallback ?? DEFAULT_PREFERENCE;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_PREFERENCE;
    const value = JSON.parse(raw) as Record<string, unknown> | null;
    if (!value || typeof value !== "object") return DEFAULT_PREFERENCE;
    return {
      enabled:
        typeof value.enabled === "boolean"
          ? value.enabled
          : DEFAULT_PREFERENCE.enabled,
      days: normalizeSessionAutoArchiveDays(value.days),
    };
  } catch {
    return fallback ?? DEFAULT_PREFERENCE;
  }
}

export function writeSessionAutoArchivePreference(
  value: SessionAutoArchivePreference,
) {
  const next = {
    enabled: value.enabled,
    days: normalizeSessionAutoArchiveDays(value.days),
  };
  fallback = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    unpersisted = false;
  } catch {
    unpersisted = true;
  }
}

export interface StaleSessionCandidate {
  sessionId: string;
  archived?: boolean;
  deleting?: boolean;
  status?: string;
  lastTurnEndedAt?: number | null;
}

export function staleSessionIds(
  sessions: StaleSessionCandidate[],
  options: {
    days: number;
    now: number;
    protectedIds?: Iterable<string>;
  },
): string[] {
  const protectedSet = new Set(options.protectedIds ?? []);
  const ageMs = normalizeSessionAutoArchiveDays(options.days) * 86_400_000;
  return sessions
    .filter((session) => {
      if (
        session.archived ||
        session.deleting ||
        protectedSet.has(session.sessionId)
      )
        return false;
      if (
        ["running", "compacting", "starting", "awaiting_input"].includes(
          session.status ?? "",
        )
      )
        return false;
      const ended = session.lastTurnEndedAt;
      return (
        typeof ended === "number" &&
        Number.isFinite(ended) &&
        ended <= options.now - ageMs
      );
    })
    .map((session) => session.sessionId);
}
