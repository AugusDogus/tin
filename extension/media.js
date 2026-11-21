// Finds the images and videos in part of a page, for Tin to keep with a capture. The background
// runs it in the page with chrome.scripting, so it must stand alone: no outside names. Tested by
// test/media.test.mjs.

// eslint-disable-next-line no-unused-vars
function tinMediaIn(area, doc = document, view = window) {
  const limit = 20;
  // Icons and avatars aren't what a capture is about.
  const smallest = 32;
  const pick = {
    left: area.left * view.innerWidth,
    top: area.top * view.innerHeight,
    right: (area.left + area.width) * view.innerWidth,
    bottom: (area.top + area.height) * view.innerHeight,
  };
  const pickSize = (pick.right - pick.left) * (pick.bottom - pick.top);
  const shared = (r) =>
    Math.max(0, Math.min(r.right, pick.right) - Math.max(r.left, pick.left)) *
    Math.max(0, Math.min(r.bottom, pick.bottom) - Math.max(r.top, pick.top));
  // Mostly inside the pick, or the pick mostly on it.
  const inPick = (r) => {
    const s = shared(r);
    return s > 0 && (s >= 0.5 * r.width * r.height || s >= 0.5 * pickSize);
  };
  const web = (url) => typeof url === "string" && /^https?:/.test(url);

  const found = [];
  const seen = new Set();
  // `url` is null for a video the page streams without an address: Tin notes it was there.
  const add = (kind, url, alt) => {
    if (seen.has(url) || found.length >= limit) return;
    if (url) seen.add(url);
    found.push({ kind, url: url || null, alt: alt || null });
  };
  for (const el of doc.querySelectorAll("img, video")) {
    const r = el.getBoundingClientRect();
    if (r.width < smallest || r.height < smallest || !inPick(r)) continue;
    if (el.tagName === "IMG") {
      // Images made in memory (data:) are placeholders while the real one loads, or too small to matter.
      const url = el.currentSrc || el.src;
      if (web(url)) add("photo", url, el.alt);
    } else {
      // A streamed video's address is the page's own (blob:), or it has none (fed through a media
      // stream object); Tin notes it can't keep it.
      const url = el.currentSrc || el.src || el.querySelector("source[src]")?.src;
      add("video", url, el.getAttribute("aria-label"));
      if (web(el.poster)) add("photo", el.poster, "The video's poster");
    }
  }
  return found;
}
