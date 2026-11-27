import { test, expect } from "@playwright/test";
import { openSettings } from "./controls";

const updateInfo = {
  currentVersion: "0.29.0",
  checkedAt: 1_700_000_000_000,
  host: {
    name: "rpi",
    current: "0.29.0",
    latest: "0.30.0",
    updatable: true,
  },
  extensions: [
    {
      name: "rpi-web",
      current: "0.1.86",
      latest: "0.1.95-preview",
      updatable: true,
    },
  ],
  proxy: "http://127.0.0.1:7890",
  proxyConfigured: true,
};

const summary = {
  sessionId: "auto-update",
  projectId: "workspace",
  title: "Auto update fixture",
  status: "ready",
  tags: [],
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_000_000,
  lastSeq: 0,
  error: null,
  latestSettlement: null,
};

test("automatic update settings are available from the sidebar", async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "en"),
  );
  let checks = 0;
  let savedProxy: string | null = null;
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/updates/check") {
      checks += 1;
      return route.fulfill({ json: updateInfo });
    }
    if (path === "/api/v1/updates/proxy") {
      savedProxy = route.request().postDataJSON().proxy ?? null;
      return route.fulfill({
        json: {
          proxy: savedProxy,
          proxyConfigured: Boolean(savedProxy),
        },
      });
    }
    if (path === "/api/v1/updates/install") {
      return route.fulfill({ json: { taskId: "task-1" } });
    }
    if (path === "/api/v1/updates/status") {
      return route.fulfill({
        json: {
          state: "succeeded",
          target: "host",
          message: "Update succeeded",
          log: "done",
          restartRequired: true,
        },
      });
    }
    if (path.endsWith("/commands")) {
      const type = route.request().postDataJSON().type;
      return route.fulfill({
        json: {
          type: "response",
          command: type,
          success: true,
          data:
            type === "get_state"
              ? { stats: {}, activeOperation: null, queue: {} }
              : type === "get_queue"
                ? { items: [], count: 0 }
                : {},
        },
      });
    }
    if (path.endsWith("/events")) {
      return route.fulfill({
        contentType: "text/event-stream",
        body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n',
      });
    }
    const json =
      path === "/api/v1/auth"
        ? { required: false, authenticated: true }
        : path === "/api/v1/projects"
          ? {
              projects: [
                { id: "workspace", name: "Workspace", path: "/workspace" },
              ],
              projectsRoot: "/workspace",
              defaultProjectId: "workspace",
            }
          : path === "/api/v1/sessions"
            ? { sessions: [summary] }
            : path === `/api/v1/sessions/${summary.sessionId}`
              ? summary
              : path.endsWith("/snapshot")
                ? {
                    sessionId: summary.sessionId,
                    messages: [],
                    events: [],
                    lastSeq: 0,
                    hasMore: false,
                    pendingResume: false,
                  }
                : path.endsWith("/run-timings/query")
                  ? { timings: [], complete: true }
                  : path === "/api/v1/capabilities"
                    ? {
                        protocol: "rpi",
                        version: "0.1.86",
                        supportedCommands: [
                          "get_state",
                          "get_stats",
                          "get_history",
                          "get_queue",
                        ],
                        supportedActions: ["projects"],
                      }
                    : {};
    return route.fulfill({ json });
  });

  await page.goto("/");
  // Opt-in: opening the page must not spawn the native CLI by itself.
  await page.waitForTimeout(1_000);
  expect(checks).toBe(0);
  await expect(page.locator(".update-badge")).toHaveCount(0);

  const dialog = await openSettings(page);
  const section = dialog.locator(".auto-update-settings");
  await expect(
    section.getByText("Automatic updates", { exact: true }),
  ).toBeVisible();
  await expect(
    section.getByLabel("Check for new versions on open"),
  ).not.toBeChecked();

  // Turning it on is what triggers the check, through the same opt-in path.
  await section.getByLabel("Check for new versions on open").check();
  await expect.poll(() => checks).toBeGreaterThan(0);
  await expect(page.locator(".update-badge")).toBeVisible();
  await expect(
    section.getByRole("button", { name: "Update host" }),
  ).toBeEnabled();
  await expect(
    section.getByRole("button", { name: "Update extension" }),
  ).toBeEnabled();
  await expect(section.getByText("v0.30.0")).toBeVisible();
  await expect(section.getByLabel("Proxy URL (optional)")).toHaveValue(
    "http://127.0.0.1:7890",
  );

  await section
    .getByLabel("Proxy URL (optional)")
    .fill("http://127.0.0.1:10809");
  await section.getByRole("button", { name: "Save proxy" }).click();
  expect(savedProxy).toBe("http://127.0.0.1:10809");

  await section.getByRole("button", { name: "Update host" }).click();
  await expect(section.locator(".update-task span")).toHaveText(
    "Update succeeded",
  );
});
