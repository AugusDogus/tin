// Pulls tweets out of X's own API responses (its GraphQL JSON), with everything Tin keeps: the
// full text (long posts included), author, time, quoted tweet and media. Plain script, loaded in
// the page's world on x.com before x-main.js; tested by test/x-tweets.test.mjs.

// eslint-disable-next-line no-unused-vars
function tinExtractTweets(json, limit = 200) {
  const found = new Map();
  const seen = new Set();

  const user = (tweet) => {
    const u = tweet?.core?.user_results?.result;
    return {
      handle: u?.core?.screen_name ?? u?.legacy?.screen_name ?? null,
      author: u?.core?.name ?? u?.legacy?.name ?? null,
    };
  };

  const bestMp4 = (variants = []) =>
    variants
      .filter((v) => v.content_type === "video/mp4" && v.url)
      .sort((a, b) => (b.bitrate ?? 0) - (a.bitrate ?? 0))[0]?.url ?? null;

  const media = (legacy) =>
    (legacy?.extended_entities?.media ?? legacy?.entities?.media ?? [])
      .map((m) => ({
        kind: m.type,
        url: m.type === "photo" ? (m.media_url_https ? `${m.media_url_https}?name=orig` : null) : bestMp4(m.video_info?.variants),
      }))
      .filter((m) => m.url);

  const asTweet = (node) => {
    // Some tweets come wrapped ("TweetWithVisibilityResults").
    const t = node?.__typename === "TweetWithVisibilityResults" ? node.tweet : node;
    if (!t?.rest_id || !t.legacy) return null;
    const { handle, author } = user(t);
    if (!handle) return null;
    return {
      id: t.rest_id,
      handle,
      author,
      text: t.note_tweet?.note_tweet_results?.result?.text ?? t.legacy.full_text ?? "",
      created_at: t.legacy.created_at ?? null,
      quoted_id: t.legacy.quoted_status_id_str ?? null,
      media: media(t.legacy),
    };
  };

  const walk = (value, depth) => {
    if (found.size >= limit || depth > 60 || value === null || typeof value !== "object" || seen.has(value)) return;
    seen.add(value);
    if (value.__typename === "Tweet" || value.__typename === "TweetWithVisibilityResults") {
      const tweet = asTweet(value);
      if (tweet && !found.has(tweet.id)) found.set(tweet.id, tweet);
    }
    for (const child of Array.isArray(value) ? value : Object.values(value)) walk(child, depth + 1);
  };

  walk(json, 0);
  return [...found.values()];
}
