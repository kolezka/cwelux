import {
  decodeDetails,
  decodeSession,
  decodeSessions,
  decodeText,
  decodeVoid,
  type Session,
  type SessionDetails,
} from "./types";

export type InvokeFn = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

/** Everything the UI may ask of the backend. Mirrors the shared bridge contract. */
export interface TmuxBridge {
  listSessions(): Promise<Session[]>;
  sessionDetails(sessionId: string): Promise<SessionDetails>;
  capturePane(sessionId: string, paneId: string): Promise<string>;
  createSession(name: string, directory: string): Promise<Session>;
  renameSession(sessionId: string, name: string): Promise<void>;
  killSession(sessionId: string): Promise<void>;
  openSession(sessionId: string): Promise<void>;
  attachCommand(sessionId: string): Promise<string>;
  hidePopover(): Promise<void>;
}

/** Wraps Tauri invoke and checks every response against the contract. Backend errors pass through. */
export function createTauriBridge(invoke: InvokeFn): TmuxBridge {
  return {
    listSessions: async () => decodeSessions(await invoke("list_sessions")),
    sessionDetails: async (sessionId) => decodeDetails(await invoke("session_details", { sessionId })),
    capturePane: async (sessionId, paneId) =>
      decodeText(await invoke("capture_pane", { sessionId, paneId }), "pane output"),
    createSession: async (name, directory) =>
      decodeSession(await invoke("create_session", { name, directory }), "created session"),
    renameSession: async (sessionId, name) =>
      decodeVoid(await invoke("rename_session", { sessionId, name }), "rename_session"),
    killSession: async (sessionId) => decodeVoid(await invoke("kill_session", { sessionId }), "kill_session"),
    openSession: async (sessionId) => decodeVoid(await invoke("open_session", { sessionId }), "open_session"),
    attachCommand: async (sessionId) =>
      decodeText(await invoke("attach_command", { sessionId }), "attach command", false),
    hidePopover: async () => decodeVoid(await invoke("hide_popover"), "hide_popover"),
  };
}
