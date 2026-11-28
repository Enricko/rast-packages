// AI Agent's window of a RAST web page app on the server (see
// `app_window.rs`), inside the app's own frame, before the app's scripts:
// an app that opens files with the browser's file picker and saves them as
// downloads (one that takes no documents from RAST) works with the person's
// RAST files all the same:
// - its file picker gets the file the window has ready (`pick`, answered by
//   `picked` from the window's page);
// - what it saves as a download goes to the window's page (`download`),
//   which stores it in the person's files.
// Messages go only to the page that frames this one, marked
// `rastAgentFrame: 1`; the page takes them only from this frame.
(() => {
  "use strict";
  if (window.parent === window || window.__rastAgentFrame) return;
  Object.defineProperty(window, "__rastAgentFrame", { value: true });
  const parent = window.parent;
  const post = (message, transfer) => parent.postMessage({ rastAgentFrame: 1, ...message }, "*", transfer || []);

  // ------------------------------------------------------- the file picker

  const waiting = new Map();
  let nextId = 1;
  addEventListener("message", (event) => {
    const data = event.data;
    if (event.source !== parent || !data || data.rastAgentFrame !== 1 || data.kind !== "picked") return;
    const resolve = waiting.get(data.id);
    if (!resolve) return;
    waiting.delete(data.id);
    resolve(Array.isArray(data.files) ? data.files : []);
  });

  const isFileInput = (el) => el instanceof HTMLInputElement && el.type === "file";

  function pick(input) {
    const id = nextId++;
    new Promise((resolve) => {
      waiting.set(id, resolve);
      post({ kind: "pick", id, accept: input.accept || "", multiple: input.multiple === true });
    }).then((files) => {
      if (!files.length) {
        input.dispatchEvent(new Event("cancel", { bubbles: true }));
        return;
      }
      const list = new DataTransfer();
      for (const file of files) list.items.add(new File([file.bytes], String(file.name), { type: String(file.type || "") }));
      input.files = list.files;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }

  // ------------------------------------------------------------ downloads

  // The blobs behind the page's object URLs: apps often revoke the URL right
  // after the click that downloads it.
  const blobs = new Map();
  const createObjectURL = URL.createObjectURL;
  URL.createObjectURL = function (object) {
    const url = createObjectURL.call(URL, object);
    if (object instanceof Blob) blobs.set(url, object);
    return url;
  };
  const revokeObjectURL = URL.revokeObjectURL;
  URL.revokeObjectURL = function (url) {
    setTimeout(() => blobs.delete(url), 60000);
    return revokeObjectURL.call(URL, url);
  };

  const isDownload = (el) =>
    el instanceof HTMLAnchorElement && el.hasAttribute("download") && /^(blob|data):/i.test(el.href);

  function nameOf(anchor, type) {
    const given = (anchor.getAttribute("download") || "").trim();
    if (given) return given;
    const ext = { "image/png": ".png", "image/jpeg": ".jpg", "application/pdf": ".pdf", "image/svg+xml": ".svg" }[type] || "";
    return `download${ext}`;
  }

  function download(anchor) {
    const href = anchor.href;
    const held = blobs.get(href);
    // Read now, before the app lets go of the URL.
    const reading = held ? Promise.resolve(held) : fetch(href).then((response) => response.blob());
    reading
      .then(async (blob) => {
        const bytes = await blob.arrayBuffer();
        post({ kind: "download", name: nameOf(anchor, blob.type), type: blob.type, bytes }, [bytes]);
      })
      .catch((error) => post({ kind: "download-failed", error: String((error && error.message) || error) }));
  }

  // -------------------------------------------------- where they come from

  const click = HTMLElement.prototype.click;
  HTMLElement.prototype.click = function () {
    if (isFileInput(this)) return void pick(this);
    if (isDownload(this)) return void download(this);
    return click.call(this);
  };
  const showPicker = HTMLInputElement.prototype.showPicker;
  if (typeof showPicker === "function") {
    HTMLInputElement.prototype.showPicker = function () {
      if (isFileInput(this)) return void pick(this);
      return showPicker.call(this);
    };
  }
  const dispatchEvent = EventTarget.prototype.dispatchEvent;
  EventTarget.prototype.dispatchEvent = function (event) {
    if (event instanceof MouseEvent && event.type === "click") {
      if (isFileInput(this)) {
        pick(this);
        return false;
      }
      if (isDownload(this)) {
        download(this);
        return false;
      }
    }
    return dispatchEvent.call(this, event);
  };
  // A click in the page itself (ui.click) on a file input or a download link.
  addEventListener(
    "click",
    (event) => {
      const target = event.target instanceof Element ? event.target.closest("input, a") : null;
      if (!target || !event.isTrusted) return;
      if (isFileInput(target)) {
        event.preventDefault();
        pick(target);
      } else if (isDownload(target)) {
        event.preventDefault();
        download(target);
      }
    },
    true,
  );
})();
