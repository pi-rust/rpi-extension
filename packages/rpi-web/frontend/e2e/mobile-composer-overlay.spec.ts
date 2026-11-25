/* Regression coverage is a focused CSS assertion; no host fixture is needed. */
import { devices, expect, test, type Page } from "@playwright/test";

const capabilities = {
  protocol: "rpi",
  version: "9.9.9",
  piCompatibilityTarget: "1.2.0",
  supportedCommands: ["get_state", "get_messages", "prompt"],
  supportedActions: ["projects", "files", "session_organization"],
  parityComplete: false,
};

async function mockEmptySession(page: Page) {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    const data = (value: unknown) => ({
      type: "response",
      command: "mock",
      success: true,
      data: value,
    });

    if (path === "/api/v1/auth")
      return route.fulfill({
        json: { required: false, authenticated: true },
      });
    if (path === "/api/v1/capabilities")
      return route.fulfill({ json: capabilities });
    if (path === "/api/v1/projects")
      return route.fulfill({
        json: {
          projects: [
            {
              id: "project-a",
              name: "task",
              path: "/workspace/projects/task",
              archived: false,
            },
          ],
          defaultProjectId: "project-a",
          projectsRoot: "/workspace/projects",
        },
      });
    if (path === "/api/v1/sessions")
      return route.fulfill({
        json: {
          sessions: [
            {
              sessionId: "session-1",
              projectId: "project-a",
              title: "New session",
              tags: [],
              status: "ready",
              createdAt: 1_700_000_000_000,
              updatedAt: 1_700_000_000_000,
              lastSeq: 0,
              error: null,
              relation: null,
              parentSessionId: null,
              forkEntryId: null,
              latestSettlement: null,
            },
          ],
        },
      });
    if (/\/sessions\/session-1$/.test(path))
      return route.fulfill({
        json: data({
          sessionId: "session-1",
          projectId: "project-a",
          title: "New session",
          tags: [],
          status: "ready",
          createdAt: 1_700_000_000_000,
          updatedAt: 1_700_000_000_000,
          lastSeq: 0,
          error: null,
          relation: null,
          parentSessionId: null,
          forkEntryId: null,
          latestSettlement: null,
        }),
      });
    if (path === "/api/v1/sessions/session-1/snapshot")
      return route.fulfill({
        json: {
          sessionId: "session-1",
          supportedCommands: [],
          messages: [],
          history: {
            records: [],
            total: 0,
            limit: 100,
            hasMore: false,
            firstSeq: 0,
            lastSeq: 0,
            openOperations: [],
          },
          events: [],
          lastSeq: 0,
          leafId: null,
          pendingResume: false,
        },
      });
    return route.fulfill({ json: data({}) });
  });
}

test("mobile composer hides the empty-state project name behind attachments", async ({
  browser,
}) => {
  const context = await browser.newContext({
    ...devices["iPhone 13"],
    baseURL: process.env.BASE_URL ?? "http://127.0.0.1:4191",
  });
  const page = await context.newPage();
  try {
    await mockEmptySession(page);
    await page.goto("/?session=session-1");

    const composerArea = page.locator(".composer-area");
    await expect(composerArea).toBeVisible();
    await expect(page.locator(".welcome-symbol")).toBeHidden();
    await expect(page.locator(".welcome h2")).toBeHidden();
    await expect(page.locator(".welcome > p")).toBeHidden();
    await expect(page.locator(".welcome .suggestions")).toBeHidden();

    const composerBackground = await composerArea.evaluate(
      (element) => getComputedStyle(element).backgroundColor,
    );
    expect(composerBackground).not.toMatch(/rgba\([^)]*,\s*0\)/);

    // Two attachments approximate the tall composer that made the project
    // name bleed through on a phone keyboard viewport.
    await page.locator(".composer textarea").fill("draft");
    await page.evaluate(() => {
      const cards = document.createElement("div");
      cards.className = "composer-attachments";
      cards.innerHTML = Array.from(
        { length: 2 },
        () =>
          '<div class="attachment-card composer-attachment"><span class="attachment-info"><b class="attachment-name">IMG_14169.jpg</b><span class="attachment-meta">JPG · 89.3 KB</span></span></div>',
      ).join("");
      document.querySelector(".composer")?.prepend(cards);
    });

    await expect(page.locator(".composer-attachment")).toHaveCount(2);
    await expect(page.locator(".welcome-symbol")).toBeHidden();
    const composerBox = await page.locator(".composer").boundingBox();
    expect(composerBox?.height).toBeGreaterThan(100);
  } finally {
    await context.close();
  }
});
