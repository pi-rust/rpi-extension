import { useCallback, useEffect, useMemo, useState } from "react";

/** Chromium's install prompt event; not part of the DOM lib typings. */
export interface InstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed"; platform: string }>;
}

const DISMISSED_KEY = "rpi-web-install-dismissed";

/** Already launched from the installed app, not a browser tab. */
export function isStandalone() {
  if (typeof window === "undefined") return false;
  return (
    window.matchMedia?.("(display-mode: standalone)").matches === true ||
    (window.navigator as { standalone?: boolean }).standalone === true
  );
}

/**
 * iOS and iPadOS never fire `beforeinstallprompt`: installation is the Share
 * sheet's "Add to Home Screen", so the entry falls back to instructions there.
 */
export function isIosDevice() {
  if (typeof navigator === "undefined") return false;
  return (
    /iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1)
  );
}

function dismissed() {
  try {
    return localStorage.getItem(DISMISSED_KEY) === "1";
  } catch {
    return false;
  }
}

function persistDismissed() {
  try {
    localStorage.setItem(DISMISSED_KEY, "1");
  } catch {
    /* optional browser persistence */
  }
}

/**
 * Track whether this browser can install the workspace as a standalone app.
 *
 * The prompt is only offered while installation is real: a captured
 * `beforeinstallprompt`, or the iOS Share-sheet route. Installed launches and
 * dismissed prompts keep the sidebar clean.
 */
export function useInstallApp() {
  const [deferred, setDeferred] = useState<InstallPromptEvent | null>(null);
  const [installed, setInstalled] = useState(isStandalone);
  const [hidden, setHidden] = useState(dismissed);
  useEffect(() => {
    const onPrompt = (event: Event) => {
      // Keep the event so the sidebar button can open the browser's own prompt.
      event.preventDefault();
      setDeferred(event as InstallPromptEvent);
    };
    const onInstalled = () => {
      setInstalled(true);
      setDeferred(null);
    };
    const standalone = window.matchMedia?.("(display-mode: standalone)");
    const onChange = () => setInstalled(isStandalone());
    window.addEventListener("beforeinstallprompt", onPrompt);
    window.addEventListener("appinstalled", onInstalled);
    standalone?.addEventListener?.("change", onChange);
    return () => {
      window.removeEventListener("beforeinstallprompt", onPrompt);
      window.removeEventListener("appinstalled", onInstalled);
      standalone?.removeEventListener?.("change", onChange);
    };
  }, []);
  const ios = useMemo(isIosDevice, []);
  const promptInstall = useCallback(async () => {
    if (!deferred) return false;
    await deferred.prompt();
    const choice = await deferred.userChoice.catch(() => null);
    if (choice?.outcome !== "accepted") return false;
    // The event is single-use; a second call would be a no-op.
    setDeferred(null);
    return true;
  }, [deferred]);
  const dismiss = useCallback(() => {
    persistDismissed();
    setHidden(true);
  }, []);
  return {
    /** Show the sidebar entry only while installation is still possible. */
    available: !installed && !hidden && (deferred !== null || ios),
    /** A real browser prompt exists; otherwise the entry shows instructions. */
    canPrompt: deferred !== null,
    ios,
    promptInstall,
    dismiss,
  };
}
