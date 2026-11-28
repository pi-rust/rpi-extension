import { expect, test, type Page } from "@playwright/test";

const sessionId = "session-ask";
const dialogRequest = {
  type: "ui_dialog_request",
  requestId: "ask-1",
  ui: {
    purpose: "question",
    question: "Which option should the assistant use?",
    context: "Reference context stays visible while deciding.",
    options: [
      { title: "Option A", value: "a" },
      { title: "Option B", value: "b" },
    ],
  },
};

async function mockAskHost(page: Page) {
  const commands: Array<Record<string, unknown>> = [];
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (data: unknown) => route.fulfill({ json: data });
    const response = (data: unknown, command = "mock") =>
      json({ type: "response", command, success: true, data });

    if (path === "/api/v1/auth")
      return json({ required: false, authenticated: true });
    if (path === "/api/v1/capabilities")
      return json({
        protocol: "rpi",
        version: "9.9.9",
        piCompatibilityTarget: "1.2.0",
        supportedCommands: ["get_state", "get_messages", "ui_dialog_response"],
        supportedActions: [],
        parityComplete: false,
      });
    if (path === "/api/v1/projects")
      return json({
        projects: [
          { id: "project-a", name: "pi-rust", path: "/workspace/pi-rust" },
        ],
        defaultProjectId: "project-a",
        projectsRoot: "/workspace/projects",
      });
    if (path === "/api/v1/sessions" && request.method() === "GET")
      return json({
        sessions: [
          {
            sessionId,
            projectId: "project-a",
            title: "Ask session",
            tags: [],
            status: "awaiting_input",
            createdAt: 1_700_000_000_000,
            updatedAt: 1_700_000_000_000,
            lastSeq: 5,
            error: null,
            relation: null,
            parentSessionId: null,
            forkEntryId: null,
            latestSettlement: null,
          },
        ],
      });
    if (/^\/api\/v1\/sessions\/[^/]+\/snapshot$/.test(path))
      return json({
        sessionId,
        messages: [],
        total: 0,
        truncated: false,
        events: [],
        lastSeq: 5,
        pendingDialog: dialogRequest,
        status: "awaiting_input",
      });
    if (/^\/api\/v1\/sessions\/[^/]+\/events$/.test(path))
      return route.fulfill({
        contentType: "text/event-stream",
        body: 'event: ready\ndata: {"type":"ready"}\n\n',
      });
    if (/^\/api\/v1\/sessions\/[^/]+\/commands$/.test(path)) {
      const body = request.postDataJSON() as Record<string, unknown>;
      commands.push(body);
      if (body.type === "ui_dialog_response")
        return response({}, "ui_dialog_response");
      if (body.type === "get_state")
        return response(
          {
            sessionId,
            leafId: "leaf",
            queue: { steeringCount: 0, followUpCount: 0 },
            stats: {
              messageCount: 0,
              cachedTokens: 0,
              uncancelledTokens: 0,
              totalTokens: 0,
              costTotal: 0,
            },
          },
          body.type,
        );
      return response({}, String(body.type ?? "unknown"));
    }
    return json({});
  });
  return commands;
}

test("ask dialog is non-modal and supports manual and skipped answers", async ({
  page,
}) => {
  const commands = await mockAskHost(page);
  await page.goto(`/?session=${sessionId}`);

  const backdrop = page.locator(".dialog-backdrop");
  const card = page.getByRole("dialog", {
    name: "Which option should the assistant use?",
  });
  await expect(card).toBeVisible();
  await expect(card).toHaveAttribute("aria-modal", "false");
  await expect(backdrop).toHaveCSS("pointer-events", "none");
  await expect(backdrop).not.toHaveCSS("backdrop-filter", /blur/);
  // The page underneath remains reachable: a settings dialog can be opened
  // through the settings button while the answer card is visible.
  await page.getByRole("button", { name: "Settings Ctrl ," }).click();
  await expect(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(card).toBeVisible();

  await page.getByRole("button", { name: "Answer manually" }).click();
  const answer = page.getByRole("textbox", { name: "Answer" });
  await expect(answer).toBeVisible();
  await answer.fill("Use the second option because of this context.");
  const submit = page.getByRole("button", { name: "Submit" });
  await expect(submit).toBeEnabled();
  await submit.click();
  await expect
    .poll(() => commands)
    .toContainEqual({
      type: "ui_dialog_response",
      requestId: "ask-1",
      answer: "Use the second option because of this context.",
    });
});

test("ask dialog can be dismissed without answering", async ({ page }) => {
  const commands = await mockAskHost(page);
  await page.goto(`/?session=${sessionId}`);
  await expect(
    page.getByRole("dialog", {
      name: "Which option should the assistant use?",
    }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Don't answer" }).click();
  await expect
    .poll(() => commands)
    .toContainEqual({
      type: "ui_dialog_response",
      requestId: "ask-1",
      cancelled: true,
    });
});
