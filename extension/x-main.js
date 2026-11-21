// Runs in x.com's own page world: watches the API responses X's app loads and hands the tweets in
// them to x-relay.js, which passes them to Tin. Reads responses only; changes nothing.
(() => {
  const isApi = (url) => typeof url === "string" && url.includes("/i/api/graphql/");
  const post = (json) => {
    try {
      const tweets = tinExtractTweets(json);
      if (tweets.length) window.postMessage({ source: "tin", type: "tweets", tweets }, location.origin);
    } catch {
      // A response shape Tin doesn't know: skip it.
    }
  };

  const originalFetch = window.fetch;
  window.fetch = async function (...args) {
    const response = await originalFetch.apply(this, args);
    const url = typeof args[0] === "string" ? args[0] : args[0]?.url;
    if (isApi(url)) response.clone().json().then(post, () => {});
    return response;
  };

  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    if (isApi(String(url))) {
      this.addEventListener("load", () => {
        try {
          post(this.responseType === "json" ? this.response : JSON.parse(this.responseText));
        } catch {
          // Not JSON: skip it.
        }
      });
    }
    return open.call(this, method, url, ...rest);
  };
})();
