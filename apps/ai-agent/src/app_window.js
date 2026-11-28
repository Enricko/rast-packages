// AI Agent's window of a RAST web page app on the server (see
// `app_window.rs`): the page a headless Chrome shows at RAST's own address.
// It plays the RAST window's part for the app's page, as RAST's
// `frontend/src/core/web-window.ts`, `web-bridge.ts` and `web-storage.ts` do
// in the person's browser, and speaks the same messages with RAST's scripts
// in the app's page (`web_bridge.js`, `web_storage.js`):
// - the app's page runs in a sandboxed frame (an opaque origin), its
//   localStorage in the frame's name;
// - the window hands it documents (`open`), takes what it saves (`save`,
//   answered by `saved`) and asks it to work (`request`, answered by
//   `reply`);
// - what the page stores goes to the account's storage in RAST.
// - an app that takes no documents from RAST gets the file the window has
//   ready at its file picker, and what it saves as a download is stored
//   (`app_frame.js` in the app's frame, messages marked `rastAgentFrame`).
// AI Agent is reached through the `rastAgent` binding (a JSON message; an
// asking one carries `call` and is answered by `window.__rast.answer`).
(() => {
  "use strict";
  const WEB_SANDBOX = "allow-scripts allow-forms allow-modals allow-popups allow-downloads allow-pointer-lock";
  /** How long after the page's modules ran the app may still say what it offers. */
  const OFFER_GRACE_MS = 4000;
  const LOCAL_SAVE_MS = 400;
  /** How long a save may take once the app handed it over. */
  const SAVE_LIMIT_MS = 120000;

  // ------------------------------------------------------------ AI Agent

  const calls = new Map();
  let nextCall = 1;
  const tell = (message) => window.rastAgent(JSON.stringify(message));
  const ask = (message) =>
    new Promise((resolve, reject) => {
      const call = nextCall++;
      calls.set(call, { resolve, reject });
      tell({ ...message, call });
    });
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const failure = (error) => String((error && error.message) || error);

  // ------------------------------------------------------- keys and values

  function base64(bytes) {
    let text = "";
    for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return btoa(text);
  }

  function unbase64(text) {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }

  function copyBytes(view) {
    if (view instanceof ArrayBuffer) return new Uint8Array(view.slice(0));
    return new Uint8Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength));
  }

  class StorageError extends Error {
    constructor(message, storageName = "UnknownError") {
      super(message);
      this.storageName = storageName;
    }
  }

  function encodeKey(key) {
    if (typeof key === "number" && !Number.isNaN(key)) return `n${Object.is(key, -0) ? 0 : key}`;
    if (typeof key === "string") return `s${key}`;
    if (key instanceof Date && !Number.isNaN(key.getTime())) return `d${key.getTime()}`;
    if (key instanceof ArrayBuffer || ArrayBuffer.isView(key)) return `b${base64(copyBytes(key))}`;
    if (Array.isArray(key)) return `a${JSON.stringify(key.map(encodeKey))}`;
    throw new StorageError("That is not a key.");
  }

  function decodeKey(text) {
    const rest = text.slice(1);
    switch (text[0]) {
      case "n":
        return Number(rest);
      case "s":
        return rest;
      case "d":
        return new Date(Number(rest));
      case "b":
        return unbase64(rest).buffer;
      case "a":
        return JSON.parse(rest).map(decodeKey);
      default:
        throw new StorageError("A stored key is not readable.");
    }
  }

  const TAG = "$t";
  const VIEWS = {
    Int8Array,
    Uint8Array,
    Uint8ClampedArray,
    Int16Array,
    Uint16Array,
    Int32Array,
    Uint32Array,
    Float32Array,
    Float64Array,
    BigInt64Array,
    BigUint64Array,
    DataView,
  };

  function viewName(view) {
    for (const [name, type] of Object.entries(VIEWS)) if (view instanceof type) return name;
    return "Uint8Array";
  }

  async function encodeValue(value) {
    const parts = [];
    const seen = new Set();
    const part = (bytes) => {
      parts.push(bytes);
      return { i: parts.length - 1, l: bytes.length };
    };
    const walk = async (node) => {
      switch (typeof node) {
        case "string":
        case "boolean":
          return node;
        case "number":
          if (Number.isFinite(node) && !Object.is(node, -0)) return node;
          return { [TAG]: "n", v: Object.is(node, -0) ? "-0" : String(node) };
        case "undefined":
          return { [TAG]: "u" };
        case "bigint":
          return { [TAG]: "i", v: node.toString() };
        case "object":
          break;
        default:
          throw new StorageError(`A ${typeof node} cannot be stored.`);
      }
      if (node === null) return null;
      if (seen.has(node)) throw new StorageError("A value that holds itself cannot be stored.");
      seen.add(node);
      try {
        if (node instanceof Date) return { [TAG]: "d", v: String(node.getTime()) };
        if (node instanceof RegExp) return { [TAG]: "r", s: node.source, f: node.flags };
        if (node instanceof ArrayBuffer) return { [TAG]: "ab", ...part(copyBytes(node)) };
        if (ArrayBuffer.isView(node)) return { [TAG]: "v", c: viewName(node), ...part(copyBytes(node)) };
        if (node instanceof Blob) {
          const bytes = new Uint8Array(await node.arrayBuffer());
          const file = node instanceof File ? { name: node.name, m: node.lastModified } : {};
          return { [TAG]: "b", type: node.type, ...part(bytes), ...file };
        }
        if (node instanceof Map) {
          const entries = [];
          for (const [k, v] of node) entries.push([await walk(k), await walk(v)]);
          return { [TAG]: "m", v: entries };
        }
        if (node instanceof Set) {
          const entries = [];
          for (const v of node) entries.push(await walk(v));
          return { [TAG]: "s", v: entries };
        }
        if (node instanceof Error) return { [TAG]: "e", n: node.name, m: node.message };
        if (node instanceof Boolean || node instanceof Number || node instanceof String) {
          return { [TAG]: "w", v: await walk(node.valueOf()) };
        }
        if (Array.isArray(node)) {
          const out = [];
          for (let i = 0; i < node.length; i++) out.push(await walk(node[i]));
          return out;
        }
        const out = {};
        for (const key of Object.keys(node)) out[key] = await walk(node[key]);
        return TAG in out ? { [TAG]: "o", v: out } : out;
      } finally {
        seen.delete(node);
      }
    };
    const tree = new TextEncoder().encode(JSON.stringify(await walk(value)));
    const out = new Uint8Array(4 + tree.length + parts.reduce((sum, p) => sum + p.length, 0));
    new DataView(out.buffer).setUint32(0, tree.length);
    out.set(tree, 4);
    let at = 4 + tree.length;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }

  function decodeValue(bytes) {
    const unreadable = () => new StorageError("A stored value is not readable.");
    if (bytes.length < 4) throw unreadable();
    const length = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0);
    if (4 + length > bytes.length) throw unreadable();
    const tree = JSON.parse(new TextDecoder().decode(bytes.subarray(4, 4 + length)));
    const lengths = new Map();
    const collect = (node) => {
      if (node === null || typeof node !== "object") return;
      if (Array.isArray(node)) {
        node.forEach(collect);
        return;
      }
      if (TAG in node && typeof node.i === "number" && typeof node.l === "number") lengths.set(node.i, node.l);
      for (const inner of Object.values(node)) collect(inner);
    };
    collect(tree);
    const parts = [];
    let at = 4 + length;
    for (let index = 0; index < lengths.size; index++) {
      const size = lengths.get(index);
      if (size === undefined || at + size > bytes.length) throw unreadable();
      parts.push(bytes.slice(at, at + size));
      at += size;
    }
    if (at !== bytes.length) throw unreadable();
    const partAt = (index) => {
      const found = parts[Number(index)];
      if (!found) throw unreadable();
      return found;
    };
    const unwalk = (node) => {
      if (node === null || typeof node !== "object") return node;
      if (Array.isArray(node)) return node.map(unwalk);
      if (!(TAG in node)) {
        const out = {};
        for (const [key, inner] of Object.entries(node)) out[key] = unwalk(inner);
        return out;
      }
      switch (node[TAG]) {
        case "n":
          return node.v === "-0" ? -0 : Number(node.v);
        case "u":
          return undefined;
        case "i":
          return BigInt(String(node.v));
        case "d":
          return new Date(Number(node.v));
        case "r":
          return new RegExp(String(node.s), String(node.f));
        case "ab":
          return partAt(node.i).slice().buffer;
        case "v":
          return new (VIEWS[String(node.c)] || Uint8Array)(partAt(node.i).slice().buffer);
        case "b": {
          const blobPart = partAt(node.i).slice().buffer;
          const type = String(node.type || "");
          if (typeof node.name === "string") return new File([blobPart], node.name, { type, lastModified: Number(node.m) || 0 });
          return new Blob([blobPart], { type });
        }
        case "m":
          return new Map(node.v.map(([k, v]) => [unwalk(k), unwalk(v)]));
        case "s":
          return new Set(node.v.map(unwalk));
        case "e": {
          const error = new Error(String(node.m || ""));
          error.name = String(node.n || "Error");
          return error;
        }
        case "w":
          return Object(unwalk(node.v));
        case "o": {
          const out = {};
          for (const [key, inner] of Object.entries(node.v)) out[key] = unwalk(inner);
          return out;
        }
        default:
          throw unreadable();
      }
    };
    return unwalk(tree);
  }

  function writeBundle(header, values) {
    const json = new TextEncoder().encode(JSON.stringify(header));
    const out = new Uint8Array(4 + json.length + values.reduce((sum, v) => sum + v.length, 0));
    new DataView(out.buffer).setUint32(0, json.length);
    out.set(json, 4);
    let at = 4 + json.length;
    for (const value of values) {
      out.set(value, at);
      at += value.length;
    }
    return out;
  }

  function readBundle(bytes) {
    if (bytes.length < 4) throw new StorageError("RAST's answer is not readable.");
    const length = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0);
    if (4 + length > bytes.length) throw new StorageError("RAST's answer is not readable.");
    return { header: JSON.parse(new TextDecoder().decode(bytes.subarray(4, 4 + length))), rest: bytes.subarray(4 + length) };
  }

  // ------------------------------------------------- the account's storage

  /** RAST's `/api/webstorage/…`, through AI Agent (the account's own). */
  async function relay(path, init = {}) {
    const body = init.body === undefined || init.body === null ? null : typeof init.body === "string" ? new TextEncoder().encode(init.body) : copyBytes(init.body);
    const answer = await ask({
      kind: "fetch",
      method: init.method || "GET",
      path,
      type: (init.headers && init.headers["Content-Type"]) || "application/json",
      body: body ? base64(body) : null,
    });
    const status = Number(answer.status) || 502;
    const bytes = answer.body ? unbase64(answer.body) : new Uint8Array(0);
    return new Response(status === 204 || status === 304 ? null : bytes, { status, headers: { "Content-Type": String(answer.type || "") } });
  }

  async function refusal(response) {
    const payload = await response.json().catch(() => null);
    const message = (payload && payload.error && payload.error.message) || `RAST answered ${response.status}`;
    return new StorageError(message, response.status === 413 ? "QuotaExceededError" : "UnknownError");
  }

  async function answered(response) {
    if (!response.ok) throw await refusal(response);
    return response.json();
  }

  const api = {
    get: (path) => relay(path).then(answered),
    post: (path, body) => relay(path, { method: "POST", body: JSON.stringify(body), headers: { "Content-Type": "application/json" } }).then(answered),
    del: (path, query) => relay(`${path}?${new URLSearchParams(query)}`, { method: "DELETE" }).then(answered),
  };

  function mergeLocal(into, change) {
    if (change.clear) {
      into.set = {};
      into.remove = [];
      into.clear = true;
    }
    for (const key of change.remove || []) {
      delete into.set[key];
      if (!into.remove.includes(key)) into.remove.push(key);
    }
    for (const [key, value] of Object.entries(change.set || {})) {
      into.set[key] = value;
      into.remove = into.remove.filter((k) => k !== key);
    }
  }

  const isRecord = (value) => typeof value === "object" && value !== null;

  class AppStorage {
    constructor(appId, onFailure) {
      this.base = `/api/webstorage/${encodeURIComponent(appId)}`;
      this.onFailure = onFailure;
      this.pending = null;
      this.timer = null;
      this.saving = Promise.resolve();
      this.commits = Promise.resolve();
    }

    async handle(message) {
      switch (message.op) {
        case "local":
          this.changeLocal({
            set: isRecord(message.set) ? Object.fromEntries(Object.entries(message.set).map(([k, v]) => [k, String(v)])) : {},
            remove: Array.isArray(message.remove) ? message.remove.map(String) : [],
            clear: message.clear === true,
          });
          return null;
        case "idb.list": {
          const answer = await api.get(`${this.base}/idb`);
          return answer.databases || [];
        }
        case "idb.load":
          return this.load(String(message.db));
        case "idb.commit":
          return this.commit(String(message.db), message.meta, Array.isArray(message.ops) ? message.ops : []);
        case "idb.delete":
          await this.commits;
          await api.del(`${this.base}/idb/db`, { name: String(message.db) });
          return null;
        default:
          throw new StorageError("RAST does not know that storage request.");
      }
    }

    changeLocal(change) {
      if (!this.pending) this.pending = { set: {}, remove: [], clear: false };
      mergeLocal(this.pending, change);
      if (this.timer) clearTimeout(this.timer);
      this.timer = setTimeout(() => void this.flush(), LOCAL_SAVE_MS);
    }

    flush() {
      if (this.timer) clearTimeout(this.timer);
      this.timer = null;
      const changes = this.pending;
      this.pending = null;
      if (!changes) return this.saving;
      this.saving = this.saving.then(() => api.post(`${this.base}/local`, changes)).then(
        () => undefined,
        (error) => this.onFailure(error),
      );
      return this.saving;
    }

    async load(name) {
      await this.commits;
      const response = await relay(`${this.base}/idb/db?name=${encodeURIComponent(name)}`);
      if (response.status === 404) return null;
      if (!response.ok) throw await refusal(response);
      const { header, rest } = readBundle(new Uint8Array(await response.arrayBuffer()));
      const about = isRecord(header) ? header : {};
      const records = [];
      let at = 0;
      for (const entry of Array.isArray(about.records) ? about.records : []) {
        if (!isRecord(entry)) continue;
        const size = Number(entry.size);
        const value = rest.subarray(at, at + size);
        at += size;
        records.push({ store: String(entry.store), key: decodeKey(String(entry.key)), value: decodeValue(value) });
      }
      return { meta: about.meta === undefined ? null : about.meta, records };
    }

    commit(name, meta, ops) {
      const run = async () => {
        const listed = [];
        const values = [];
        for (const op of ops) {
          if (op.op === "put") {
            const bytes = await encodeValue(op.value);
            listed.push({ op: "put", store: String(op.store), key: encodeKey(op.key), size: bytes.length });
            values.push(bytes);
          } else if (op.op === "delete") {
            listed.push({ op: "delete", store: String(op.store), key: encodeKey(op.key) });
          } else if (op.op === "clear") {
            listed.push({ op: "clear", store: String(op.store) });
          }
        }
        const body = writeBundle({ ...(meta === undefined || meta === null ? {} : { meta }), ops: listed }, values);
        const response = await relay(`${this.base}/idb/db?name=${encodeURIComponent(name)}`, {
          method: "POST",
          headers: { "Content-Type": "application/octet-stream" },
          body,
        });
        if (!response.ok) throw await refusal(response);
        return null;
      };
      const done = this.commits.then(run, run);
      this.commits = done.catch(() => undefined);
      return done;
    }
  }

  // ------------------------------------------------------------ the bridge

  class Bridge {
    constructor(target, hooks) {
      this.target = target;
      this.hooks = hooks;
      this.documents = false;
      this.commands = false;
      this.startedAt = null;
      this.pending = new Map();
      this.nextId = 1;
    }

    receive(data) {
      if (!isRecord(data) || data.rastWeb !== 1) return;
      switch (data.kind) {
        case "hello":
          if (data.started === true && this.startedAt === null) this.startedAt = Date.now();
          if (data.documents === true) this.documents = true;
          if (data.commands === true) this.commands = true;
          break;
        case "ready":
          this.documents = true;
          break;
        case "save":
          if (typeof data.name === "string" && data.bytes instanceof ArrayBuffer) {
            this.hooks.save({ name: data.name, bytes: data.bytes, id: data.id, saveAs: data.saveAs === true });
          }
          break;
        case "dirty":
          this.hooks.dirty(data.dirty === true);
          break;
        case "failed":
          this.hooks.failed(typeof data.error === "string" ? data.error : "");
          break;
        case "storage": {
          const id = typeof data.id === "number" ? data.id : null;
          this.hooks.storage(data).then(
            (result) => {
              if (id !== null) this.post({ kind: "storage-reply", id, ok: true, result: result === undefined ? null : result });
            },
            (error) => {
              if (id === null) return;
              const name = isRecord(error) && typeof error.storageName === "string" ? error.storageName : "UnknownError";
              this.post({ kind: "storage-reply", id, ok: false, error: failure(error), name });
            },
          );
          break;
        }
        case "reply": {
          const waiting = this.pending.get(typeof data.id === "number" ? data.id : NaN);
          if (!waiting) return;
          this.pending.delete(data.id);
          clearTimeout(waiting.timer);
          if (data.ok === true) waiting.resolve(data.result === undefined ? null : data.result);
          else waiting.reject(new Error(typeof data.error === "string" && data.error ? data.error : "the app could not do it"));
          break;
        }
      }
    }

    settled() {
      return this.startedAt !== null && Date.now() - this.startedAt >= OFFER_GRACE_MS;
    }

    async until(decide, limitMs) {
      const end = Date.now() + limitMs;
      for (;;) {
        const answer = decide();
        if (answer !== null) return answer;
        if (Date.now() >= end) return false;
        await sleep(250);
      }
    }

    takesDocuments(limitMs) {
      return this.until(() => (this.documents ? true : this.settled() ? false : null), limitMs);
    }

    takesCommands(limitMs) {
      return this.until(() => (this.commands ? true : this.settled() ? false : null), limitMs);
    }

    open(name, bytes) {
      this.post({ kind: "open", name, bytes }, [bytes]);
    }

    saved(id) {
      this.post({ kind: "saved", id });
    }

    request(method, params, timeoutMs) {
      const id = this.nextId++;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`no answer in ${Math.round(timeoutMs / 1000)} seconds`));
        }, timeoutMs);
        this.pending.set(id, { resolve, reject, timer });
        this.post({ kind: "request", id, method, params: params === undefined || params === null ? {} : params });
      });
    }

    post(message, transfer = []) {
      const target = this.target();
      if (target) target.postMessage({ rastWeb: 1, ...message }, "*", transfer);
    }
  }

  // ------------------------------------------------------------ the window

  let frame = null;
  let bridge = null;
  let storage = null;
  let name = "The app";
  let opening = Promise.resolve();
  let saves = 0;
  let lastSave = 0;
  /** The file for the app's next file picker: `{name, bytes}`. */
  let staged = null;

  /** Store what the app saved; `saveAs` when it names the file itself. */
  function store(save, done) {
    saves += 1;
    lastSave = Date.now();
    ask({ kind: "save", name: save.name, saveAs: save.saveAs, bytes: base64(new Uint8Array(save.bytes)) })
      .then(() => done && done())
      .catch(() => undefined)
      .finally(() => {
        saves -= 1;
        lastSave = Date.now();
      });
  }

  /** A message of `app_frame.js` in the app's frame. */
  function fromFrame(data) {
    switch (data.kind) {
      case "pick": {
        const files = [];
        const transfer = [];
        if (staged) {
          const bytes = staged.bytes.slice().buffer;
          files.push({ name: staged.name, type: "", bytes });
          transfer.push(bytes);
          tell({ kind: "note", text: `${name}'s file picker got ${staged.path}.` });
        } else {
          tell({ kind: "note", text: `${name} asked for a file (its file picker): open_app with the file's path gives it one, then open it in the app again.` });
        }
        frame.contentWindow.postMessage({ rastAgentFrame: 1, kind: "picked", id: data.id, files }, "*", transfer);
        break;
      }
      case "download":
        if (typeof data.name === "string" && data.bytes instanceof ArrayBuffer) {
          store({ name: data.name, bytes: data.bytes, saveAs: true });
        }
        break;
      case "download-failed":
        tell({ kind: "note", text: `${name} saved a download that could not be read: ${String(data.error || "")}` });
        break;
    }
  }

  /** Saves the app handed over meanwhile are stored (or gave up). */
  async function settle() {
    const started = Date.now();
    await sleep(500);
    while ((saves > 0 || Date.now() - lastSave < 300) && Date.now() - started < SAVE_LIMIT_MS) await sleep(100);
  }

  window.__rast = {
    /** Frame the app's page: `{page, appId, name, local}`. */
    start(config) {
      name = String(config.name || name);
      storage = new AppStorage(String(config.appId), (error) => tell({ kind: "note", text: `${name} could not keep its settings: ${failure(error)}` }));
      frame = document.createElement("iframe");
      frame.title = name;
      frame.setAttribute("sandbox", WEB_SANDBOX);
      frame.setAttribute("allow", "fullscreen; clipboard-read; clipboard-write");
      frame.setAttribute("referrerpolicy", "no-referrer");
      bridge = new Bridge(() => frame && frame.contentWindow, {
        save: (save) =>
          store(save, () => {
            if (save.id !== undefined) bridge.saved(save.id);
          }),
        dirty: (dirty) => tell({ kind: "dirty", dirty }),
        failed: (error) => {
          if (error) tell({ kind: "note", text: `${name}: ${error}` });
        },
        storage: (request) => storage.handle(request),
      });
      addEventListener("message", (event) => {
        if (!frame || !frame.contentWindow || event.source !== frame.contentWindow) return;
        if (isRecord(event.data) && event.data.rastAgentFrame === 1) fromFrame(event.data);
        else bridge.receive(event.data);
      });
      // The browser gives the page the frame's name when it joins the page.
      frame.name = `rast-storage:${JSON.stringify({ v: 1, local: isRecord(config.local) ? config.local : {} })}`;
      frame.src = String(config.page);
      document.body.replaceChildren(frame);
      return true;
    },

    /** Hand the app a document (its bytes in base64). */
    open(file, bytes, limitMs) {
      opening = (async () => {
        if (!(await bridge.takesDocuments(limitMs))) throw new Error(`${name} opens no files from RAST`);
        bridge.open(String(file), unbase64(bytes).buffer);
        // The app reads it in its next frames.
        await sleep(600);
      })();
      return opening;
    },

    /** What the app offers once it started: `{documents, commands}`. */
    async offers(limitMs) {
      await bridge.until(() => ((bridge.documents && bridge.commands) || bridge.settled() ? true : null), limitMs);
      return { documents: bridge.documents, commands: bridge.commands };
    },

    /** The file for the app's next file picker (its bytes in base64). */
    stage(file, bytes, path) {
      staged = { name: String(file), bytes: unbase64(bytes), path: String(path) };
      return true;
    },

    /** Wait for what the app saved meanwhile (after a click or a key). */
    settle() {
      return settle();
    },

    /** One method of the app's page command API. */
    async request(method, params, startMs, limitMs) {
      await opening.catch(() => undefined);
      if (!(await bridge.takesCommands(startMs))) throw new Error(`${name} takes no commands`);
      const result = await bridge.request(String(method), params, limitMs);
      await settle();
      return result === undefined ? null : result;
    },

    /** AI Agent's answer to an asking message. */
    answer(call, ok, value) {
      const waiting = calls.get(call);
      if (!waiting) return;
      calls.delete(call);
      if (ok) waiting.resolve(value);
      else waiting.reject(new Error(String(value)));
    },

    /** What the page stored, saved now (the window is closing). */
    flush() {
      return storage ? storage.flush() : Promise.resolve();
    },
  };
})();
