import { createContext } from "react";

/**
 * The file a rendered view came from.
 *
 * A Markdown preview renders links and images from the previewed file, not from
 * the message that opened it, so relative references must resolve against that
 * file's directory — and against the worktree it lives in, not the project root.
 */
export interface ProjectBase {
  /** `/`-separated directory of the file, empty for a root-level file. */
  dir: string;
  /** Linked worktree the file belongs to; omitted for the project root. */
  root?: string;
}

export const ProjectBaseContext = createContext<ProjectBase | undefined>(
  undefined,
);

/** Directory part of a `/`-separated project path. */
export function projectDirname(path: string): string {
  const index = path.lastIndexOf("/");
  return index < 0 ? "" : path.slice(0, index);
}
