// `node --test extension/test`: tweet extraction from X's API responses.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = readFileSync(new URL("../x-tweets.js", import.meta.url), "utf8");
const tinExtractTweets = new Function(`${source}; return tinExtractTweets;`)();

const user = (screen_name, name) => ({ user_results: { result: { core: { screen_name, name } } } });

const timeline = {
  data: {
    home: {
      home_timeline_urt: {
        instructions: [
          {
            entries: [
              {
                content: {
                  itemContent: {
                    tweet_results: {
                      result: {
                        __typename: "Tweet",
                        rest_id: "2123928352205439115",
                        core: user("southpolesteve", "Steve Faulkner"),
                        legacy: {
                          full_text: "Every LLM API is built wrong… dig +tcp +short…",
                          created_at: "Fri Nov 20 23:00:00 +0000 2026",
                          quoted_status_id_str: "99",
                          extended_entities: {
                            media: [
                              { type: "photo", media_url_https: "https://pbs.twimg.com/media/A.jpg" },
                              {
                                type: "video",
                                video_info: {
                                  variants: [
                                    { content_type: "application/x-mpegURL", url: "https://video.twimg.com/a.m3u8" },
                                    { content_type: "video/mp4", bitrate: 256000, url: "https://video.twimg.com/low.mp4" },
                                    { content_type: "video/mp4", bitrate: 2176000, url: "https://video.twimg.com/high.mp4" },
                                  ],
                                },
                              },
                            ],
                          },
                        },
                        note_tweet: { note_tweet_results: { result: { text: "Every LLM API is built wrong. … the whole post." } } },
                        quoted_status_result: {
                          result: {
                            __typename: "TweetWithVisibilityResults",
                            tweet: { rest_id: "99", core: user("adam", "Adam"), legacy: { full_text: "original" } },
                          },
                        },
                      },
                    },
                  },
                },
              },
            ],
          },
        ],
      },
    },
  },
};

test("takes the full text of long posts, the author, the best media and the quoted tweet", () => {
  const tweets = tinExtractTweets(timeline);
  const main = tweets.find((t) => t.id === "2123928352205439115");
  assert.equal(main.handle, "southpolesteve");
  assert.equal(main.author, "Steve Faulkner");
  assert.equal(main.text, "Every LLM API is built wrong. … the whole post.");
  assert.equal(main.quoted_id, "99");
  assert.deepEqual(main.media, [
    { kind: "photo", url: "https://pbs.twimg.com/media/A.jpg?name=orig" },
    { kind: "video", url: "https://video.twimg.com/high.mp4" },
  ]);
  const quoted = tweets.find((t) => t.id === "99");
  assert.equal(quoted.text, "original");
  assert.equal(quoted.handle, "adam");
});

test("ignores things that aren't tweets and survives odd shapes", () => {
  assert.deepEqual(tinExtractTweets({ data: { user: { __typename: "User" } } }), []);
  assert.deepEqual(tinExtractTweets(null), []);
  assert.deepEqual(tinExtractTweets({ __typename: "Tweet" }), []);
});
