import type { ProjectFileLink } from "./messageLinks";

/**
 * Deep-link parameters for the preview dialog.
 *
 * The dialog used to be invisible to the address bar, so "dialog stacked on
 * dialog" left a URL that named only the session. These parameters make the
 * file being read (and the worktree it came from) copyable and reload-safe.
 */
const FILE = "file";
const ROOT = "root";
const LINE = "line";
const END_LINE = "endLine";

function positiveInt(value: string | null) {
  if (!value) return undefined;
  const number = Number(value);
  return Number.isSafeInteger(number) && number > 0 ? number : undefined;
}

/** Parse `?file=…&root=…&line=…` from a search string (`location.search`). */
export function readPreviewLink(search: string): ProjectFileLink | null {
  const params = new URLSearchParams(search);
  const path = params.get(FILE)?.trim();
  if (!path) return null;
  const root = params.get(ROOT)?.trim() || undefined;
  const line = positiveInt(params.get(LINE));
  const end = positiveInt(params.get(END_LINE));
  const endLine = line === undefined ? undefined : Math.max(line, end ?? line);
  return root ? { path, root, line, endLine } : { path, line, endLine };
}

/** The search parameters for a link, without touching the URL (testable). */
export function previewParams(link: ProjectFileLink | null): URLSearchParams {
  const params = new URLSearchParams();
  if (!link?.path) return params;
  params.set(FILE, link.path);
  if (link.root) params.set(ROOT, link.root);
  if (link.line) {
    params.set(LINE, String(link.line));
    if (link.endLine && link.endLine !== link.line)
      params.set(END_LINE, String(link.endLine));
  }
  return params;
}

/**
 * Replace the URL's preview parameters. `null` removes them, which is what a
 * closed dialog leaves behind; every other query parameter is preserved.
 */
export function writePreviewLink(link: ProjectFileLink | null) {
  const url = new URL(window.location.href);
  url.searchParams.delete(FILE);
  url.searchParams.delete(ROOT);
  url.searchParams.delete(LINE);
  url.searchParams.delete(END_LINE);
  for (const [key, value] of previewParams(link))
    url.searchParams.set(key, value);
  history.replaceState(null, "", url);
}
