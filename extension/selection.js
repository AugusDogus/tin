// Reports this page's selected text to Tin (through the extension), so a capture can include it.
let timer;
let last = "";
document.addEventListener("selectionchange", () => {
  clearTimeout(timer);
  timer = setTimeout(() => {
    const text = String(window.getSelection() ?? "");
    if (text === last) return;
    last = text;
    chrome.runtime.sendMessage({ type: "selection", url: location.href, text }).catch(() => {});
  }, 250);
});
