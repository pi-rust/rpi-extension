import { test as base, expect, type Page } from "@playwright/test";
import { chooseMenuOption, clickRowAction, openRuntimePanel } from "./controls";

type Json = Record<string, unknown>;
const models = [
  { provider: "native", id: "default", name: "Native default" },
  { provider: "chosen", id: "special", name: "Chosen model" },
];
const levels = ["off", "low", "high"];
const envelope = (command: string, data: unknown) => ({
  type: "response",
  command,
  success: true,
  data,
});
function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
class Host {
  calls: { session: string; type: string; body: Json }[] = [];
  unexpected: string[] = [];
  states: Record<string, Json> = {
    old: { provider: "native", modelId: "default", thinkingLevel: "off" },
  };
  projects: Record<string, string> = { old: "p1" };
  streams: ReturnType<typeof gate>[] = [];
  constructor(readonly page: Page) {}
  summary(sessionId: string) {
    return {
      sessionId,
      projectId: this.projects[sessionId],
      title: `Session ${sessionId}`,
      status: "ready",
      createdAt: 1,
      updatedAt: 2,
      lastSeq: 0,
      pinned: false,
      archived: false,
    };
  }
  async install() {
    await this.page.route("**/api/**", async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname.replace("/api/v1", "");
      const json = (data: unknown) => route.fulfill({ json: data });
      if (req.method() === "GET" && path === "/auth")
        return json({ required: false, authenticated: true });
      if (req.method() === "GET" && path === "/capabilities")
        return json({
          protocol: "rpi",
          supportedCommands: [
            "get_state",
            "get_messages",
            "get_available_models",
            "get_available_thinking_levels",
            "get_auth_status",
            "set_model",
            "set_thinking_level",
            "prompt",
          ],
          supportedActions: ["projects", "provider_auth"],
          parityComplete: false,
        });
      if (req.method() === "GET" && path === "/projects")
        return json({
          projects: [
            { id: "p1", name: "Project 1", path: "/mock/1", archived: false },
          ],
          defaultProjectId: "p1",
          projectsRoot: "/mock",
        });
      if (path.match(/^\/projects\/[^/]+\/resources$/))
        return json({
          version: 1,
          success: true,
          operation: "list",
          scope: new URL(req.url()).searchParams.get("scope"),
          kind: null,
          name: null,
          data: { outcome: "observed", resources: [], diagnostics: [] },
        });
      if (path === "/sessions") {
        if (req.method() === "GET")
          return json({
            sessions: Object.keys(this.states).map((id) => this.summary(id)),
          });
        if (req.method() === "POST") {
          const id = `new${Object.keys(this.states).length}`;
          this.projects[id] = req.postDataJSON().projectId;
          this.states[id] = {
            provider: "native",
            modelId: "default",
            thinkingLevel: "off",
          };
          this.calls.push({
            session: id,
            type: "create",
            body: req.postDataJSON(),
          });
          return json({ sessionId: id, status: "ready" });
        }
      }
      const match = path.match(
        /^\/sessions\/([^/]+)\/(open|snapshot|events|commands|run-timings\/query)$/,
      );
      if (match && this.states[match[1]]) {
        const [, session, endpoint] = match;
        if (endpoint === "open") return json(this.summary(session));
        if (endpoint === "snapshot")
          return json({
            sessionId: session,
            status: "ready",
            lastSeq: 0,
            messages: [],
            events: [],
            total: 0,
            hasMore: false,
            truncated: false,
            leafId: `${session}-leaf`,
            pendingResume: false,
          });
        if (endpoint === "events") {
          const stream = gate();
          this.streams.push(stream);
          await stream.promise;
          return route.fulfill({
            contentType: "text/event-stream",
            body: "retry: 60000\n\n",
          });
        }
        if (endpoint === "run-timings/query")
          return json({ timings: [], complete: true, cursor: null });
        const body = req.postDataJSON() as Json;
        const type = String(body.type);
        this.calls.push({ session, type, body });
        if (type === "set_model") {
          this.states[session] = {
            ...this.states[session],
            provider: body.provider,
            modelId: body.modelId,
          };
          return json(envelope(type, {}));
        }
        if (type === "set_thinking_level") {
          this.states[session] = {
            ...this.states[session],
            thinkingLevel: body.level,
          };
          return json(envelope(type, {}));
        }
        if (type === "prompt") return json(envelope(type, {}));
        if (type === "get_state")
          return json(
            envelope(type, { sessionId: session, ...this.states[session] }),
          );
        if (type === "get_available_models")
          return json(envelope(type, { models }));
        if (type === "get_available_thinking_levels")
          return json(envelope(type, { levels }));
        if (type === "get_messages")
          return json(
            envelope(type, { messages: [], total: 0, hasMore: false }),
          );
        if (type === "get_auth_status")
          return json(envelope(type, { providers: [] }));
      }
      this.unexpected.push(`${req.method()} ${path}`);
      return route.fulfill({ status: 501, json: { error: "Unmocked API" } });
    });
  }
  async dispose() {
    for (const stream of this.streams) stream.release();
    await this.page.unrouteAll({ behavior: "wait" });
  }
}
const test = base.extend<{ host: Host }>({
  host: [
    async ({ page }, use) => {
      const host = new Host(page);
      await host.install();
      await page.addInitScript(() =>
        localStorage.setItem("rpi-web-language", "en"),
      );
      try {
        await use(host);
      } finally {
        await host.dispose();
        expect(host.unexpected).toEqual([]);
      }
    },
    { auto: true },
  ],
});
async function stored(page: Page) {
  return page.evaluate(() =>
    Object.entries(localStorage)
      .filter(([key]) => key.startsWith("rpi-web-model-preference:"))
      .map(([, value]) => JSON.parse(value)),
  );
}
test("runtime settings saves a default model and thinking level as one pair", async ({
  page,
  host,
}) => {
  await page.goto("/?session=old");
  await expect(page.locator(".toolbar .breadcrumb h1")).toHaveText(
    "Session old",
  );
  await openRuntimePanel(page);
  await chooseMenuOption(page, "Default model", {
    label: "Chosen model (chosen)",
  });
  await chooseMenuOption(page, "Default thinking level", { label: "high" });
  await page
    .getByRole("button", {
      name: "Save default model and thinking level",
      exact: true,
    })
    .click();
  expect(await stored(page)).toEqual([
    { provider: "chosen", modelId: "special", thinkingLevel: "high" },
  ]);
  await clickRowAction(page, "New session in: Project 1");
  await expect(page.locator(".toolbar .breadcrumb h1")).toHaveText(
    "Session new1",
  );
  const commands = host.calls
    .filter((call) => call.session === "new1")
    .map((call) => call.type);
  expect(commands.indexOf("set_model")).toBeLessThan(
    commands.indexOf("set_thinking_level"),
  );
  expect(
    host.calls.find(
      (call) => call.session === "new1" && call.type === "set_model",
    )?.body,
  ).toMatchObject({ provider: "chosen", modelId: "special" });
  expect(
    host.calls.find(
      (call) => call.session === "new1" && call.type === "set_thinking_level",
    )?.body,
  ).toMatchObject({ level: "high" });
});
