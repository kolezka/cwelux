import { describe, expect, test } from "bun:test";
import { createTauriBridge, type InvokeFn } from "./api";
import { BridgeDataError } from "./types";

type Call = { command: string; args: Record<string, unknown> | undefined };

function fakeInvoke(result: unknown) {
  const calls: Call[] = [];
  const invoke: InvokeFn = async (command, args) => {
    calls.push({ command, args });
    return result;
  };
  return { calls, invoke };
}

const validSession = { id: "$1", name: "web site's", windows: 2, attached: 1, created: 1_700_000_000 };
const validDetails = {
  session: validSession,
  windows: [
    {
      id: "@1",
      index: 0,
      name: "dev",
      active: true,
      panes: [{ id: "%1", index: 0, command: "zsh", path: "/Users/me", active: true, width: 120, height: 32 }],
    },
  ],
};

describe("createTauriBridge command contract", () => {
  test("uses the exact command names and camelCase argument keys", async () => {
    const cases: Array<[(b: ReturnType<typeof createTauriBridge>) => Promise<unknown>, unknown, Call]> = [
      [(b) => b.listSessions(), [], { command: "list_sessions", args: undefined }],
      [(b) => b.sessionDetails("$1"), validDetails, { command: "session_details", args: { sessionId: "$1" } }],
      [(b) => b.capturePane("$1", "%1"), "out", { command: "capture_pane", args: { sessionId: "$1", paneId: "%1" } }],
      [(b) => b.createSession("n", "~/p"), validSession, { command: "create_session", args: { name: "n", directory: "~/p" } }],
      [(b) => b.renameSession("$1", "n"), null, { command: "rename_session", args: { sessionId: "$1", name: "n" } }],
      [(b) => b.killSession("$1"), null, { command: "kill_session", args: { sessionId: "$1" } }],
      [(b) => b.openSession("$1"), null, { command: "open_session", args: { sessionId: "$1" } }],
      [(b) => b.attachCommand("$1"), "tmux attach -t '$1'", { command: "attach_command", args: { sessionId: "$1" } }],
      [(b) => b.hidePopover(), null, { command: "hide_popover", args: undefined }],
    ];
    for (const [call, result, expected] of cases) {
      const fake = fakeInvoke(result);
      await call(createTauriBridge(fake.invoke));
      expect(fake.calls).toEqual([expected]);
    }
  });

  test("returns decoded models for valid payloads", async () => {
    expect(await createTauriBridge(fakeInvoke([validSession]).invoke).listSessions()).toEqual([validSession]);
    expect(await createTauriBridge(fakeInvoke([]).invoke).listSessions()).toEqual([]);
    expect(await createTauriBridge(fakeInvoke(validDetails).invoke).sessionDetails("$1")).toEqual(validDetails);
  });

  test("passes backend error strings through unchanged", async () => {
    const bridge = createTauriBridge(async () => {
      throw "can't find session: $1";
    });
    await expect(bridge.listSessions()).rejects.toBe("can't find session: $1");
  });
});

describe("createTauriBridge rejects malformed data instead of showing an empty list", () => {
  const malformedLists: unknown[] = [
    null,
    undefined,
    "",
    {},
    [{ ...validSession, id: 1 }],
    [{ ...validSession, windows: "2" }],
    [{ ...validSession, attached: -1 }],
    [{ ...validSession, created: 1.5 }],
    [{ id: "$1", name: "x", windows: 1, attached: 0 }],
    [null],
  ];

  for (const payload of malformedLists) {
    test(`list_sessions payload ${JSON.stringify(payload) ?? "undefined"}`, async () => {
      const bridge = createTauriBridge(fakeInvoke(payload).invoke);
      await expect(bridge.listSessions()).rejects.toBeInstanceOf(BridgeDataError);
    });
  }

  test("session_details with a malformed pane", async () => {
    const broken = structuredClone(validDetails) as { windows: Array<{ panes: Array<Record<string, unknown>> }> };
    broken.windows[0]!.panes[0]!.active = "yes";
    await expect(createTauriBridge(fakeInvoke(broken).invoke).sessionDetails("$1")).rejects.toBeInstanceOf(BridgeDataError);
  });

  test("session_details without windows array", async () => {
    await expect(
      createTauriBridge(fakeInvoke({ session: validSession }).invoke).sessionDetails("$1"),
    ).rejects.toBeInstanceOf(BridgeDataError);
  });

  test("capture_pane and attach_command require strings", async () => {
    await expect(createTauriBridge(fakeInvoke(null).invoke).capturePane("$1", "%1")).rejects.toBeInstanceOf(BridgeDataError);
    await expect(createTauriBridge(fakeInvoke(42).invoke).attachCommand("$1")).rejects.toBeInstanceOf(BridgeDataError);
    await expect(createTauriBridge(fakeInvoke("").invoke).attachCommand("$1")).rejects.toBeInstanceOf(BridgeDataError);
  });

  test("create_session requires a session", async () => {
    await expect(createTauriBridge(fakeInvoke(null).invoke).createSession("a", "~")).rejects.toBeInstanceOf(BridgeDataError);
  });
});
