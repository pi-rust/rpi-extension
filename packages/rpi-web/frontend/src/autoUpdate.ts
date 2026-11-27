// Browser-local preference for automatic update checks. The installer and
// release registry are authoritative; this only stores whether the web UI
// should query them when the page opens.
export interface AutoUpdatePreference {
  enabled: boolean;
}

const STORAGE_KEY = "rpi-auto-update";
const DEFAULT_PREFERENCE: AutoUpdatePreference = { enabled: true };

let fallback: AutoUpdatePreference | null = null;
let unpersisted = false;

export function readAutoUpdatePreference(): AutoUpdatePreference {
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
    };
  } catch {
    return fallback ?? DEFAULT_PREFERENCE;
  }
}

export function writeAutoUpdatePreference(value: AutoUpdatePreference) {
  const next = { enabled: value.enabled };
  fallback = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    unpersisted = false;
  } catch {
    unpersisted = true;
  }
}
