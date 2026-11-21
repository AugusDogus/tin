// Tells Tin (through its native-messaging helper) which page each window shows.
const HOST = "dev.tin.host";

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

// A page's selected text, from selection.js, sent on with the window it's in.
chrome.runtime.onMessage.addListener((message, sender) => {
  if (message?.type === "selection" && sender.tab) {
    send({ type: "selection", window: sender.tab.windowId, url: message.url, text: message.text });
  }
});

// When the extension (re)starts, report every window once.
chrome.windows.getAll({}, (windows) => windows.forEach((w) => report(w.id)));
