import { Timer } from "lucide-react";
import { useEffect, useState } from "react";
import { translate, type Language } from "./i18n";

function formatDuration(milliseconds: number, running: boolean) {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds / 60) % 60;
  const tail = `${String(seconds % 60).padStart(2, "0")}s`;
  if (hours > 0) {
    const time = `${hours}h ${String(minutes).padStart(2, "0")}m`;
    return running ? `${time} ${tail}` : time;
  }
  return minutes > 0 ? `${minutes}m ${tail}` : `${seconds}s`;
}

export function SessionWork({
  completedMs,
  running,
  startedAt,
  language,
}: {
  completedMs: number | null;
  running: boolean;
  startedAt?: number;
  language: Language;
}) {
  const [clock, setClock] = useState<{ observedAt: number; now: number }>();
  useEffect(() => {
    if (!running) {
      setClock(undefined);
      return;
    }
    const observedAt = Date.now();
    const observedTick = performance.now();
    const now = () => observedAt + (performance.now() - observedTick);
    setClock({ observedAt, now: observedAt });
    const interval = window.setInterval(
      () => setClock({ observedAt, now: now() }),
      1000,
    );
    return () => window.clearInterval(interval);
  }, [running]);

  const elapsed = clock
    ? Math.max(0, clock.now - (startedAt ?? clock.observedAt))
    : 0;
  const total = running ? (completedMs ?? 0) + elapsed : completedMs;
  const time = total === null ? "—" : formatDuration(total, running);
  const tooltip = running
    ? startedAt === undefined
      ? "sessionWorkObserved"
      : "sessionWorkLive"
    : "sessionWorkTotal";
  return (
    <span
      className="composer-metric"
      title={translate(language, tooltip)}
      aria-label={`${translate(language, "sessionWork")}: ${time}`}
    >
      <Timer size={12} aria-hidden="true" />
      <strong>{time}</strong>
    </span>
  );
}
