// Passes tweets from x-main.js (in the page's world) to the extension, which sends them to Tin.
window.addEventListener("message", (event) => {
  if (event.source !== window || event.origin !== location.origin) return;
  const data = event.data;
  if (data?.source !== "tin" || data.type !== "tweets" || !Array.isArray(data.tweets)) return;
  chrome.runtime.sendMessage({ type: "tweets", tweets: data.tweets }).catch(() => {});
});
