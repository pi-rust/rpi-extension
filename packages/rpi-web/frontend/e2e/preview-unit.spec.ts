import { expect, test } from "@playwright/test";
import type { Project } from "../src/api";
import { inlineFileLink, projectFileLink } from "../src/messageLinks";
import { projectImageSource } from "../src/projectImages";
import { previewParams, readPreviewLink } from "../src/previewLink";

// Pure helpers behind the preview dialog's address bar and relative links.

const project = {
  id: "project-1",
  name: "links",
  path: "/workspace/links",
  worktrees: ["/workspace/links-wt"],
} as Project;

test("preview parameters round-trip through the address bar", () => {
  const search = previewParams({
    path: "docs/notes.md",
    root: "/workspace/links-wt",
    line: 3,
    endLine: 5,
  }).toString();
  expect(readPreviewLink(`?${search}`)).toEqual({
    path: "docs/notes.md",
    root: "/workspace/links-wt",
    line: 3,
    endLine: 5,
  });
  // A single line needs no separate end, and junk numbers are ignored.
  expect(readPreviewLink("?file=a%2Fb.md&line=4&endLine=x")).toEqual({
    path: "a/b.md",
    line: 4,
    endLine: 4,
  });
  expect(readPreviewLink("?file=&line=2")).toBeNull();
  expect(readPreviewLink("?session=abc")).toBeNull();
});

test("a relative link inside a file resolves against its directory", () => {
  const base = { dir: "docs" };
  expect(projectFileLink("sub/next.md", project, base)).toEqual({
    path: "docs/sub/next.md",
  });
  expect(projectFileLink("./next.md", project, base)).toEqual({
    path: "docs/next.md",
  });
  expect(projectFileLink("../README.md", project, base)).toEqual({
    path: "README.md",
  });
  // Climbing above the checkout is refused, exactly like the server boundary.
  expect(projectFileLink("../../etc/passwd", project, base)).toBeNull();
  // The worktree the file came from travels with the link.
  expect(
    projectFileLink("sub/next.md", project, {
      dir: "docs",
      root: "/workspace/links-wt",
    }),
  ).toEqual({ path: "docs/sub/next.md", root: "/workspace/links-wt" });
  // Absolute links ignore the base and keep their own root resolution.
  expect(
    projectFileLink("/workspace/links-wt/src/a.ts", project, base),
  ).toEqual({ path: "src/a.ts", root: "/workspace/links-wt" });
  // Hidden directories stay unlinkable after resolution.
  expect(projectFileLink("../../.rpi/sessions.json", project, base)).toBeNull();
});

test("inline code and images honour the same base directory", () => {
  expect(inlineFileLink("sub/next.md", project, { dir: "docs" })).toEqual({
    path: "docs/sub/next.md",
  });
  expect(inlineFileLink("next.md", project, { dir: "docs" })).toBeNull();
  expect(projectImageSource("./chart.png", project, { dir: "docs" })).toBe(
    "/api/v1/projects/project-1/image?path=docs%2Fchart.png",
  );
  expect(
    projectImageSource("./chart.png", project, {
      dir: "docs",
      root: "/workspace/links-wt",
    }),
  ).toBe(
    "/api/v1/projects/project-1/image?path=docs%2Fchart.png&root=%2Fworkspace%2Flinks-wt",
  );
  expect(projectImageSource("../../escape.png", project, { dir: "docs" })).toBe(
    "",
  );
});

test("a chat message without a base keeps project-root resolution", () => {
  expect(projectFileLink("src/main.ts", project)).toEqual({
    path: "src/main.ts",
  });
  expect(projectFileLink("../src/main.ts", project)).toBeNull();
});
