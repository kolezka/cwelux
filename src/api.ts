import {
  decodeDetails,
  decodeRemoteHosts,
  decodeSession,
  decodeSessions,
  decodeText,
  decodeVoid,
  type RemoteHostStatus,
  type Session,
  type SessionDetails,
} from "./types";
import { decodeBoolean, decodeChosenDirectory, decodePreferences, type Preferences } from "./preferences";

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
  getPreferences(): Promise<Preferences>;
  openSettings(): Promise<void>;
  listRemoteHosts(): Promise<RemoteHostStatus[]>;
}

/** The settings window's bridge. It has no tmux session commands. */
export interface SettingsBridge {
  getPreferences(): Promise<Preferences>;
  savePreferences(preferences: Preferences): Promise<Preferences>;
  chooseDirectory(): Promise<string | null>;
  getLaunchAtLogin(): Promise<boolean>;
  setLaunchAtLogin(enabled: boolean): Promise<boolean>;
}

const getPreferences = (invoke: InvokeFn) => async () => decodePreferences(await invoke("get_preferences"));

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
    getPreferences: getPreferences(invoke),
    openSettings: async () => decodeVoid(await invoke("open_settings"), "open_settings"),
    listRemoteHosts: async () => decodeRemoteHosts(await invoke("list_remote_hosts")),
  };
}

export function createSettingsBridge(invoke: InvokeFn): SettingsBridge {
  return {
    getPreferences: getPreferences(invoke),
    savePreferences: async (preferences) =>
      decodePreferences(await invoke("save_preferences", { preferences }), "saved preferences"),
    chooseDirectory: async () => decodeChosenDirectory(await invoke("choose_directory")),
    getLaunchAtLogin: async () => decodeBoolean(await invoke("get_launch_at_login"), "launch at login state"),
    setLaunchAtLogin: async (enabled) =>
      decodeBoolean(await invoke("set_launch_at_login", { enabled }), "launch at login state"),
  };
}
