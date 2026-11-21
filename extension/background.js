// Tells Tin (through its native-messaging helper) which page each window shows, and answers its
// questions while it captures.
importScripts("media.js");

const HOST = "dev.tin.host";
// How long to wait before reconnecting when Tin's helper goes away (or isn't installed).
const RECONNECT_MS = 30_000;

function send(message) {
  chrome.runtime.sendNativeMessage(HOST, message, () => {
    // Tin isn't installed or its helper isn't registered: nothing to do but stay quiet.
    void chrome.runtime.lastError;
  });
}

async function report(windowId) {
  const [tab] = await chrome.tabs.query({ active: true, windowId });
  if (tab?.url) send({ type: "tab", window: windowId, url: tab.url, title: tab.title ?? "" });
}

chrome.tabs.onActivated.addListener(({ windowId }) => report(windowId));
chrome.tabs.onUpdated.addListener((_tabId, change, tab) => {
  if (tab.active && (change.url || change.title || change.status === "complete")) report(tab.windowId);
});
chrome.windows.onFocusChanged.addListener((windowId) => {
  if (windowId === chrome.windows.WINDOW_ID_NONE) return;
  send({ type: "focus", window: windowId });
  report(windowId);
});
chrome.windows.onRemoved.addListener((windowId) => send({ type: "closed", window: windowId }));

// A page's selected text (selection.js), sent on with the window it's in; and tweets from x.com.
chrome.runtime.onMessage.addListener((message, sender) => {
  if (message?.type === "selection" && sender.tab) {
    send({ type: "selection", window: sender.tab.windowId, url: message.url, text: message.text });
  }
  // Tweets X's app loaded, from x-main.js through x-relay.js.
  if (message?.type === "tweets" && Array.isArray(message.tweets)) {
    send({ type: "tweets", tweets: message.tweets });
  }
});

// Tin's questions come through one port kept open to its helper, which listens for Tin. An open
// port also keeps this worker running.
function serve() {
  const port = chrome.runtime.connectNative(HOST);
  port.onMessage.addListener(async (message) => {
    if (message?.type !== "request") return;
    let reply;
    try {
      reply = await answer(message.request);
    } catch (e) {
      reply = { ok: false, error: String(e?.message ?? e) };
    }
    port.postMessage({ type: "reply", id: message.id, reply });
  });
  port.onDisconnect.addListener(() => {
    void chrome.runtime.lastError;
    setTimeout(serve, RECONNECT_MS);
  });
  port.postMessage({ type: "serve" });
}

async function answer(request) {
  if (request?.type === "media_in") {
    const [tab] = await chrome.tabs.query({ active: true, windowId: request.window });
    if (!tab?.id) return { ok: false, error: `window ${request.window} has no active tab` };
    const [frame] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: tinMediaIn,
      args: [request.area],
    });
    return { ok: true, media: frame?.result ?? [] };
  }
  return { ok: false, error: `this version of the extension doesn't know "${request?.type}" questions; update it` };
}

serve();

// When the extension (re)starts, report every window once.
chrome.windows.getAll({}, (windows) => windows.forEach((w) => report(w.id)));
