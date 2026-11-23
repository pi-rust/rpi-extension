import { test, expect, devices, type Page } from "@playwright/test";

const sessionId = "session-1";
const capabilities = {
  protocol: "rpi",
  version: "9.9.9",
  piCompatibilityTarget: "1.2.0",
  supportedCommands: [
    "get_state",
    "get_messages",
    "get_history",
    "get_queue",
    "prompt",
    "abort",
  ],
  supportedActions: [],
  parityComplete: false,
};
const session = {
  sessionId,
  projectId: "project-a",
  title: "Mock session",
  status: "ready",
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_000_100,
};
const response = (data: unknown, command = "mock") => ({
  type: "response",
  command,
  success: true,
  data,
});

async function mockHost(page: Page) {
  await page.route("**/api/v1/**", (route) => {
    const { pathname } = new URL(route.request().url());
    if (pathname === "/api/v1/auth")
      return route.fulfill({ json: { required: false, authenticated: true } });
    if (pathname === "/api/v1/capabilities")
      return route.fulfill({ json: capabilities });
    if (pathname === "/api/v1/sessions")
      return route.fulfill({ json: { sessions: [session] } });
    if (pathname === `/api/v1/sessions/${sessionId}/open`)
      return route.fulfill({ json: session });
    if (pathname === `/api/v1/sessions/${sessionId}/snapshot`)
      return route.fulfill({
        json: {
          sessionId,
          messages: [
            {
              entryId: "message-1",
              role: "user",
              content: [{ type: "text", text: "A long first question" }],
              timestamp: 1,
            },
            {
              entryId: "message-2",
              role: "assistant",
              content: [
                {
                  type: "text",
                  text: "A long answer that should stay behind the slash palette. ".repeat(
                    24,
                  ),
                },
              ],
              timestamp: 2,
            },
          ],
          history: {
            records: [],
            total: 0,
            limit: 100,
            hasMore: false,
            firstSeq: 0,
            lastSeq: 0,
          },
          events: [],
          lastSeq: 0,
          leafId: "message-2",
          pendingResume: false,
        },
      });
    if (pathname === `/api/v1/sessions/${sessionId}/events`)
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: 'event: ready\ndata: {"type":"ready"}\n\n',
      });
    if (pathname === `/api/v1/sessions/${sessionId}/commands`) {
      const body = route.request().postDataJSON() as { type?: string };
      if (body.type === "get_state")
        return route.fulfill({
          json: response(
            { modelId: "m", provider: "p", thinkingLevel: "off" },
            body.type,
          ),
        });
      if (body.type === "get_history")
        return route.fulfill({
          json: response(
            { records: [], total: 0, hasMore: false, openOperations: [] },
            body.type,
          ),
        });
      if (body.type === "get_queue")
        return route.fulfill({
          json: response({ items: [], count: 0 }, body.type),
        });
      if (body.type === "get_auth_status")
        return route.fulfill({ json: response({ providers: [] }, body.type) });
      return route.fulfill({ json: response({}, body.type) });
    }
    return route.fulfill({ json: response({}) });
  });
}

test.use({ ...devices["Pixel 5"] });
test("slash menu floats opaquely above mobile conversation text", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto(`/?session=${sessionId}`);
  const input = page.locator(".composer textarea");
  await expect(input).toBeVisible();
  await expect(page.locator(".transcript-row")).not.toHaveCount(0);
  await input.fill("/");
  const menu = page.locator(".slash-menu");
  await expect(menu).toBeVisible();

  const geometry = await page.evaluate(() => {
    const menuEl = document.querySelector(".slash-menu")!;
    const inputEl = document.querySelector(".composer textarea")!;
    const composerEl = document.querySelector(".composer")!;
    const menuRect = menuEl.getBoundingClientRect();
    const inputRect = inputEl.getBoundingClientRect();
    const composerRect = composerEl.getBoundingClientRect();
    const style = getComputedStyle(menuEl);
    const probes = [
      [(menuRect.left + menuRect.right) / 2, menuRect.top + 4],
      [
        (menuRect.left + menuRect.right) / 2,
        (menuRect.top + menuRect.bottom) / 2,
      ],
      [(menuRect.left + menuRect.right) / 2, menuRect.bottom - 4],
    ];
    return {
      aboveComposer: menuRect.bottom <= inputRect.top + 1,
      // The palette is the composer's own box, not a narrower content box.
      sameLeft: Math.abs(menuRect.left - composerRect.left) <= 1,
      sameRight: Math.abs(menuRect.right - composerRect.right) <= 1,
      position: style.position,
      opaque: style.backgroundColor === "rgb(255, 255, 255)",
      zIndex: Number(style.zIndex),
      probes: probes.map(([x, y]) => {
        const hit = document.elementFromPoint(x, y);
        return hit === menuEl || menuEl.contains(hit);
      }),
    };
  });

  expect(geometry.aboveComposer).toBe(true);
  expect(geometry.position).toBe("absolute");
  expect(geometry.opaque).toBe(true);
  expect(geometry.zIndex).toBeGreaterThan(0);
  expect(geometry.probes.every(Boolean)).toBe(true);
  expect(geometry.sameLeft).toBe(true);
  expect(geometry.sameRight).toBe(true);
});

// The form's side padding changes per breakpoint and per stylesheet, so pin the
// palette/composer alignment where each of those values is in force.
for (const [width, height] of [
  [1280, 900],
  [1100, 900],
  [900, 900],
  [600, 900],
] as const) {
  test(`slash menu keeps the composer box at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await mockHost(page);
    await page.goto(`/?session=${sessionId}`);
    const input = page.locator(".composer textarea");
    await expect(input).toBeVisible();
    await input.fill("/");
    await expect(page.locator(".slash-menu")).toBeVisible();
    const box = await page.evaluate(() => {
      const menu = document
        .querySelector(".slash-menu")!
        .getBoundingClientRect();
      const composer = document
        .querySelector(".composer")!
        .getBoundingClientRect();
      return {
        left: Math.abs(menu.left - composer.left),
        right: Math.abs(menu.right - composer.right),
        width: composer.width,
      };
    });
    expect(box.width).toBeGreaterThan(0);
    expect(box.left).toBeLessThanOrEqual(1);
    expect(box.right).toBeLessThanOrEqual(1);
  });
}
