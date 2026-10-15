addEventListener("fetch", (event) => {
  event.respondWith(handleRequest(event.request));
});

async function handleRequest(request) {
  async function MethodNotAllowed(request) {
    return new Response(`Method ${request.method} not allowed.`, {
      status: 405,
      headers: {
        Allow: "GET",
      },
    });
  }
  // Only GET requests work with this proxy.
  if (request.method !== "GET") return MethodNotAllowed(request);

  const url = new URL(request.url);
  const path = url.pathname;

  const downloadBaseUrl = globalThis.PAKRO_DOWNLOAD_BASE_URL || "__PAKRO_DOWNLOAD_BASE_URL__";
  if (typeof downloadBaseUrl !== "string" || !/^https:\/\//.test(downloadBaseUrl)) {
    return new Response("Configure PAKRO_DOWNLOAD_BASE_URL before deploying pakro.", { status: 503 });
  }
  let targetUrl;

  // Route based on path
  switch (path) {
    case "/":
      targetUrl = `${downloadBaseUrl.replace(/\/$/, "")}/install.sh`;
      break;
    case "/zsh":
      targetUrl = `${downloadBaseUrl.replace(/\/$/, "")}/pakro.run/zsh`;
      break;
    case "/bash":
      targetUrl = `${downloadBaseUrl.replace(/\/$/, "")}/pakro.run/bash`;
      break;
    case "/fish":
      targetUrl = `${downloadBaseUrl.replace(/\/$/, "")}/pakro.run/fish`;
      break;
    default:
      return new Response("Not found", { status: 404 });
  }

  const r = await fetch(targetUrl);
  const response = new Response(r.body, r);
  response.headers.set(
    "cache-control",
    "public, max-age=3600, s-maxage=3600, immutable",
  );
  response.headers.set("content-type", "text/plain");
  return response;
}
