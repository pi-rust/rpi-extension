import { expect, test } from "@playwright/test";
import {
  MAX_SESSION_AUTO_ARCHIVE_DAYS,
  MIN_SESSION_AUTO_ARCHIVE_DAYS,
  normalizeSessionAutoArchiveDays,
  staleSessionIds,
  type StaleSessionCandidate,
} from "../src/sessionAutoArchive";

const DAY = 86_400_000;
const now = 1_800_000_000_000;

const session = (
  sessionId: string,
  lastTurnEndedAt: number | null,
  extra: Partial<StaleSessionCandidate> = {},
): StaleSessionCandidate => ({
  sessionId,
  lastTurnEndedAt,
  status: "ready",
  ...extra,
});

test("auto archive days clamp to the supported 1-90 range", () => {
  expect(MIN_SESSION_AUTO_ARCHIVE_DAYS).toBe(1);
  expect(MAX_SESSION_AUTO_ARCHIVE_DAYS).toBe(90);
  expect(normalizeSessionAutoArchiveDays(0)).toBe(1);
  expect(normalizeSessionAutoArchiveDays(-5)).toBe(1);
  expect(normalizeSessionAutoArchiveDays(91)).toBe(90);
  expect(normalizeSessionAutoArchiveDays("30")).toBe(30);
  expect(normalizeSessionAutoArchiveDays("nope")).toBe(15);
  expect(normalizeSessionAutoArchiveDays(7.6)).toBe(8);
});

test("only completed turns older than the configured threshold are archived", () => {
  const sessions = [
    session("fresh", now - 14 * DAY - 1),
    session("stale", now - 15 * DAY),
    session("very-stale", now - 90 * DAY),
    session("no-turn", null),
    session("running", now - 30 * DAY, { status: "running" }),
    session("archived", now - 30 * DAY, { archived: true }),
    session("deleting", now - 30 * DAY, { deleting: true }),
    session("protected", now - 30 * DAY),
  ];
  expect(
    staleSessionIds(sessions, {
      days: 15,
      now,
      protectedIds: ["protected"],
    }),
  ).toEqual(["stale", "very-stale"]);
  expect(
    staleSessionIds(sessions, { days: 1, now, protectedIds: ["protected"] }),
  ).toEqual(["fresh", "stale", "very-stale"]);
});
