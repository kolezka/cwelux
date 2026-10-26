// Bridge models. Field names match the Rust serde camelCase output.
export interface Session {
  id: string;
  name: string;
  windows: number;
  attached: number;
  /** Unix time in seconds. */
  created: number;
}

export interface Pane {
  id: string;
  index: number;
  command: string;
  path: string;
  active: boolean;
  width: number;
  height: number;
}

export interface TmuxWindow {
  id: string;
  index: number;
  name: string;
  active: boolean;
  panes: Pane[];
}

export interface SessionDetails {
  session: Session;
  windows: TmuxWindow[];
}

/** The backend answered, but not with the shape the contract promises. */
export class BridgeDataError extends Error {
  constructor(what: string) {
    super(`Unexpected data from the tmux backend: ${what}`);
    this.name = "BridgeDataError";
  }
}

type Fields = Record<string, unknown>;

function record(value: unknown, what: string): Fields {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new BridgeDataError(`${what} is not an object`);
  }
  return value as Fields;
}

function array(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new BridgeDataError(`${what} is not a list`);
  return value;
}

function str(obj: Fields, key: string, what: string): string {
  const value = obj[key];
  if (typeof value !== "string") throw new BridgeDataError(`${what}.${key} is not text`);
  return value;
}

function count(obj: Fields, key: string, what: string): number {
  const value = obj[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new BridgeDataError(`${what}.${key} is not a whole number`);
  }
  return value;
}

function bool(obj: Fields, key: string, what: string): boolean {
  const value = obj[key];
  if (typeof value !== "boolean") throw new BridgeDataError(`${what}.${key} is not true or false`);
  return value;
}

export function decodeSession(value: unknown, what = "session"): Session {
  const o = record(value, what);
  return {
    id: str(o, "id", what),
    name: str(o, "name", what),
    windows: count(o, "windows", what),
    attached: count(o, "attached", what),
    created: count(o, "created", what),
  };
}

export function decodeSessions(value: unknown): Session[] {
  return array(value, "session list").map((item, i) => decodeSession(item, `session ${i}`));
}

function decodePane(value: unknown, what: string): Pane {
  const o = record(value, what);
  return {
    id: str(o, "id", what),
    index: count(o, "index", what),
    command: str(o, "command", what),
    path: str(o, "path", what),
    active: bool(o, "active", what),
    width: count(o, "width", what),
    height: count(o, "height", what),
  };
}

function decodeWindow(value: unknown, what: string): TmuxWindow {
  const o = record(value, what);
  return {
    id: str(o, "id", what),
    index: count(o, "index", what),
    name: str(o, "name", what),
    active: bool(o, "active", what),
    panes: array(o.panes, `${what}.panes`).map((p, i) => decodePane(p, `${what} pane ${i}`)),
  };
}

export function decodeDetails(value: unknown): SessionDetails {
  const o = record(value, "session details");
  return {
    session: decodeSession(o.session),
    windows: array(o.windows, "session details.windows").map((w, i) => decodeWindow(w, `window ${i}`)),
  };
}

export function decodeText(value: unknown, what: string, allowEmpty = true): string {
  if (typeof value !== "string") throw new BridgeDataError(`${what} is not text`);
  if (!allowEmpty && value.trim() === "") throw new BridgeDataError(`${what} is empty`);
  return value;
}

/** Commands typed as returning nothing serialize to null. */
export function decodeVoid(value: unknown, what: string): void {
  if (value !== null && value !== undefined) throw new BridgeDataError(`${what} returned an unexpected value`);
}

export interface RemoteHostStatus {
  name: string;
  url: string;
  state: "online" | "offline";
  error: string | null;
  reportedHost: string | null;
  /** Unix time in seconds, from the agent. */
  capturedAt: number | null;
  sessions: Session[];
}

function nullable<T>(obj: Fields, key: string, what: string, read: (o: Fields, k: string, w: string) => T): T | null {
  if (!(key in obj)) throw new BridgeDataError(`${what}.${key} is missing`);
  return obj[key] === null ? null : read(obj, key, what);
}

function decodeRemoteHost(value: unknown, what: string): RemoteHostStatus {
  const o = record(value, what);
  const state = o.state;
  if (state !== "online" && state !== "offline") throw new BridgeDataError(`${what}.state is not online or offline`);
  const host: RemoteHostStatus = {
    name: str(o, "name", what),
    url: str(o, "url", what),
    state,
    error: nullable(o, "error", what, str),
    reportedHost: nullable(o, "reportedHost", what, str),
    capturedAt: nullable(o, "capturedAt", what, count),
    sessions: array(o.sessions, `${what}.sessions`).map((s, i) => decodeSession(s, `${what} session ${i}`)),
  };
  if (host.name === "") throw new BridgeDataError(`${what}.name is empty`);
  if (state === "offline" && (host.error === null || host.sessions.length > 0)) {
    throw new BridgeDataError(`${what} is offline without a reason or with sessions`);
  }
  if (state === "online" && host.error !== null) throw new BridgeDataError(`${what} is online with an error`);
  return host;
}

export function decodeRemoteHosts(value: unknown): RemoteHostStatus[] {
  return array(value, "remote host list").map((item, i) => decodeRemoteHost(item, `remote host ${i}`));
}
