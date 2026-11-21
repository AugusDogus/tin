// `node --test extension/test`: finding the images and videos in part of a page.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = readFileSync(new URL("../media.js", import.meta.url), "utf8");
const tinMediaIn = new Function(`${source}; return tinMediaIn;`)();

const box = (left, top, width, height) => ({ left, top, width, height, right: left + width, bottom: top + height });
const img = (rect, src, alt = "") => ({ tagName: "IMG", currentSrc: src, src, alt, getBoundingClientRect: () => rect });
const video = (rect, src, poster = "") => ({
  tagName: "VIDEO",
  currentSrc: src,
  src,
  poster,
  getAttribute: () => null,
  querySelector: () => null,
  getBoundingClientRect: () => rect,
});
const page = (elements) => ({ querySelectorAll: () => elements });
const view = { innerWidth: 1000, innerHeight: 800 };

test("takes the images and videos inside the picked part of the page", () => {
  const doc = page([
    img(box(100, 100, 400, 300), "https://a.com/car.jpg", "A black Passat"),
    img(box(600, 100, 300, 300), "https://a.com/other-car.jpg"),
    video(box(100, 420, 400, 225), "blob:https://a.com/1", "https://a.com/poster.jpg"),
    img(box(110, 110, 16, 16), "https://a.com/icon.png"),
    img(box(120, 120, 200, 200), "data:image/gif;base64,R0lGOD"),
  ]);
  // The left half of the page.
  const found = tinMediaIn({ left: 0, top: 0, width: 0.5, height: 1 }, doc, view);
  assert.deepEqual(found, [
    { kind: "photo", url: "https://a.com/car.jpg", alt: "A black Passat" },
    { kind: "video", url: "blob:https://a.com/1", alt: null },
    { kind: "photo", url: "https://a.com/poster.jpg", alt: "The video's poster" },
  ]);
});

test("a small pick on a big image takes the image", () => {
  const doc = page([img(box(0, 0, 1000, 600), "https://a.com/hero.jpg")]);
  const found = tinMediaIn({ left: 0.4, top: 0.4, width: 0.1, height: 0.1 }, doc, view);
  assert.deepEqual(found.map((m) => m.url), ["https://a.com/hero.jpg"]);
});

test("an image mostly outside the pick is left out, and each address comes once", () => {
  const doc = page([
    img(box(400, 0, 400, 400), "https://a.com/edge.jpg"),
    img(box(0, 0, 200, 200), "https://a.com/same.jpg"),
    img(box(0, 200, 200, 200), "https://a.com/same.jpg"),
  ]);
  const found = tinMediaIn({ left: 0, top: 0, width: 0.5, height: 0.5 }, doc, view);
  assert.deepEqual(found.map((m) => m.url), ["https://a.com/same.jpg"]);
});
