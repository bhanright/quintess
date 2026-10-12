// The quintess.cc Worker. It serves the static site, answers two small APIs
// and keeps a first-party, cookieless count of visits in D1:
//
//   POST /api/hit      a page view or a download click, from the page script
//   GET  /api/release  the latest GitHub release, cached, with its SHA-256
//   GET  /stats        the visit dashboard (password protected)
//
// No IP address, cookie or identifier that lasts past a day is stored. A
// visitor is counted once a day by a hash of a secret, the date, the IP and
// the browser string; the date changes the hash each day, so visits on
// different days can't be linked.

const REPOSITORY = "bhanright/quintess";
const CANONICAL_HOST = "quintess.cc";
const RELEASE_CACHE_SECONDS = 600;
const KEEP_DAYS = 400;

// Cloudflare adds its cookieless Web Analytics script to pages (page speed and
// visit totals in the Cloudflare dashboard); the policy allows that one script.
const SECURITY_HEADERS = {
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "strict-origin-when-cross-origin",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=(), interest-cohort=()",
  "Content-Security-Policy": "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' https://static.cloudflareinsights.com; connect-src 'self' https://cloudflareinsights.com; font-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
};

export default {
  async fetch(request, env, context) {
    const url = new URL(request.url);

    // One address: www and workers.dev visitors go to quintess.cc.
    if (url.hostname === "www." + CANONICAL_HOST) {
      url.hostname = CANONICAL_HOST;
      return Response.redirect(url.toString(), 301);
    }

    if (url.pathname === "/api/hit" && request.method === "POST") return recordHit(request, env);
    if (url.pathname === "/api/release") return latestRelease(context);
    if (url.pathname === "/stats" || url.pathname === "/stats/") return stats(request, env, context);

    const response = await env.ASSETS.fetch(request);
    return withHeaders(response, url);
  },

  // Daily: forget visits older than KEEP_DAYS.
  async scheduled(event, env) {
    await env.DB.prepare("DELETE FROM hits WHERE day < date('now', ?)").bind(`-${KEEP_DAYS} days`).run();
  },
};

function withHeaders(response, url) {
  const headers = new Headers(response.headers);
  for (const [name, value] of Object.entries(SECURITY_HEADERS)) headers.set(name, value);
  const type = headers.get("content-type") || "";
  if (url.pathname.startsWith("/assets/fonts/") || url.pathname.startsWith("/assets/img/")) {
    headers.set("Cache-Control", "public, max-age=2592000");
  } else if (url.pathname.startsWith("/assets/")) {
    // site.css and site.js are linked with a version stamp, so they can be kept long.
    headers.set("Cache-Control", url.searchParams.has("v") ? "public, max-age=31536000, immutable" : "public, max-age=3600");
  } else if (type.includes("text/html")) {
    // Pages are checked for changes on every visit (cheap, with ETags).
    headers.set("Cache-Control", "public, max-age=0, must-revalidate");
  }
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

// ---- Visits ----

const BOT_PATTERN = /bot|crawl|spider|slurp|preview|monitor|headless|lighthouse|curl|wget|python|httpclient|facebookexternalhit|embedly/i;

function deviceOf(agent) {
  if (/ipad|tablet/i.test(agent)) return "tablet";
  if (/mobi|android|iphone/i.test(agent)) return "phone";
  return "desktop";
}

function browserOf(agent) {
  if (/edg\//i.test(agent)) return "Edge";
  if (/opr\/|opera/i.test(agent)) return "Opera";
  if (/firefox|fxios/i.test(agent)) return "Firefox";
  if (/chrome|crios/i.test(agent)) return "Chrome";
  if (/safari/i.test(agent)) return "Safari";
  return "Other";
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function referrerHost(referrer) {
  try {
    const host = new URL(referrer).hostname.replace(/^www\./, "");
    return host === CANONICAL_HOST ? "" : host.slice(0, 120);
  } catch {
    return "";
  }
}

async function recordHit(request, env) {
  const agent = request.headers.get("user-agent") || "";
  if (BOT_PATTERN.test(agent) || request.headers.get("sec-gpc") === "1" || request.headers.get("dnt") === "1") {
    return new Response(null, { status: 204 });
  }
  let body;
  try {
    body = JSON.parse((await request.text()).slice(0, 2000));
  } catch {
    return new Response(null, { status: 400 });
  }
  const kinds = ["view", "download", "github", "video"];
  const kind = kinds.includes(body.kind) ? body.kind : null;
  if (!kind) return new Response(null, { status: 400 });
  const path = String(body.path || "/").slice(0, 200);
  const now = new Date();
  const day = now.toISOString().slice(0, 10);
  const ip = request.headers.get("cf-connecting-ip") || "";
  const visitor = (await sha256Hex(`${env.VISITOR_SECRET || "unset"}|${day}|${ip}|${agent}`)).slice(0, 20);
  await env.DB.prepare("INSERT INTO hits (at, day, kind, path, referrer, country, device, browser, visitor) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
    .bind(now.toISOString(), day, kind, path, referrerHost(String(body.referrer || "")), request.cf?.country || "", deviceOf(agent), browserOf(agent), visitor)
    .run();
  return new Response(null, { status: 204 });
}

// ---- The latest release ----

async function fetchGitHub(path) {
  return fetch(`https://api.github.com/repos/${REPOSITORY}${path}`, {
    headers: { "User-Agent": "quintess.cc", Accept: "application/vnd.github+json" },
    cf: { cacheTtl: RELEASE_CACHE_SECONDS, cacheEverything: true },
  });
}

async function latestRelease(context) {
  const cache = caches.default;
  const key = new Request("https://quintess.cc/api/release");
  const cached = await cache.match(key);
  if (cached) return cached;

  const answer = await fetchGitHub("/releases/latest");
  if (!answer.ok) return Response.json({ error: "no release yet" }, { status: 404, headers: { "Cache-Control": "public, max-age=60" } });
  const release = await answer.json();
  // Every package of a release, each with the checksum published beside it.
  const files = release.assets || [];
  const pick = (matches) => files.find((asset) => matches(asset.name.toLowerCase()));
  const packages = {
    windows: pick((name) => name.endsWith("-setup.exe") && !name.includes("arm64")),
    windows_arm64: pick((name) => name.endsWith("-arm64-setup.exe")),
    macos: pick((name) => name.endsWith(".dmg")),
    deb: pick((name) => name.endsWith(".deb")),
    appimage: pick((name) => name.endsWith(".appimage")),
    flatpak: pick((name) => name.endsWith(".flatpak")),
    tarball: pick((name) => name.endsWith(".tar.gz")),
  };
  const describe = async (asset) => {
    if (!asset) return null;
    const checksumAsset = files.find((file) => file.name.toLowerCase() === asset.name.toLowerCase() + ".sha256");
    let sha256 = null;
    if (checksumAsset) {
      try {
        const file = await fetch(checksumAsset.browser_download_url, { headers: { "User-Agent": "quintess.cc" } });
        if (file.ok) sha256 = ((await file.text()).trim().split(/\s+/)[0] || "").toLowerCase() || null;
      } catch {}
    }
    return { name: asset.name, url: asset.browser_download_url, size: asset.size, downloads: asset.download_count, sha256 };
  };
  const described = await Promise.all(Object.values(packages).map(describe));
  const assets = Object.fromEntries(Object.keys(packages).map((key, index) => [key, described[index]]));
  const windows = assets.windows;
  const body = {
    version: String(release.tag_name || "").replace(/^v/i, ""),
    published: release.published_at,
    page: release.html_url,
    assets,
    // The Windows installer on its own, as before, for anything that still reads these.
    installer: windows ? { name: windows.name, url: windows.url, size: windows.size, downloads: windows.downloads } : null,
    sha256: windows ? windows.sha256 : null,
  };
  const response = Response.json(body, { headers: { "Cache-Control": `public, max-age=${RELEASE_CACHE_SECONDS}` } });
  context.waitUntil(cache.put(key, response.clone()));
  return response;
}

// ---- The dashboard ----

function authorized(request, env) {
  const header = request.headers.get("authorization") || "";
  if (!env.STATS_PASSWORD || !header.startsWith("Basic ")) return false;
  const [, password] = atob(header.slice(6)).split(":");
  // Compare in constant time.
  const a = new TextEncoder().encode(password || "");
  const b = new TextEncoder().encode(env.STATS_PASSWORD);
  if (a.length !== b.length) return false;
  let difference = 0;
  for (let i = 0; i < a.length; i++) difference |= a[i] ^ b[i];
  return difference === 0;
}

const escape = (text) => String(text).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

async function stats(request, env, context) {
  if (!authorized(request, env)) {
    return new Response("Sign in to see the visit counts.", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="quintess stats"', "Cache-Control": "no-store" } });
  }
  const days = Math.min(365, Math.max(7, Number(new URL(request.url).searchParams.get("days")) || 30));
  const since = `-${days - 1} days`;
  const query = (sql) => env.DB.prepare(sql).bind(since).all().then((result) => result.results);
  const [daily, pages, referrers, countries, devices, browsers, totals] = await Promise.all([
    query("SELECT day, SUM(kind = 'view') AS views, COUNT(DISTINCT CASE WHEN kind = 'view' THEN visitor END) AS visitors, SUM(kind = 'download') AS downloads FROM hits WHERE day >= date('now', ?) GROUP BY day ORDER BY day"),
    query("SELECT path AS name, COUNT(*) AS n FROM hits WHERE kind = 'view' AND day >= date('now', ?) GROUP BY path ORDER BY n DESC LIMIT 12"),
    query("SELECT referrer AS name, COUNT(*) AS n FROM hits WHERE kind = 'view' AND referrer != '' AND day >= date('now', ?) GROUP BY referrer ORDER BY n DESC LIMIT 12"),
    query("SELECT country AS name, COUNT(DISTINCT visitor || day) AS n FROM hits WHERE kind = 'view' AND day >= date('now', ?) GROUP BY country ORDER BY n DESC LIMIT 12"),
    query("SELECT device AS name, COUNT(DISTINCT visitor || day) AS n FROM hits WHERE kind = 'view' AND day >= date('now', ?) GROUP BY device ORDER BY n DESC"),
    query("SELECT browser AS name, COUNT(DISTINCT visitor || day) AS n FROM hits WHERE kind = 'view' AND day >= date('now', ?) GROUP BY browser ORDER BY n DESC"),
    query("SELECT SUM(kind = 'view') AS views, COUNT(DISTINCT CASE WHEN kind = 'view' THEN visitor || day END) AS visitors, SUM(kind = 'download') AS downloads, SUM(kind = 'github') AS github FROM hits WHERE day >= date('now', ?)"),
  ]);

  // GitHub's own count of package downloads, for every release.
  let releases = [];
  try {
    const answer = await fetchGitHub("/releases?per_page=20");
    if (answer.ok) {
      releases = (await answer.json()).map((release) => ({
        tag: release.tag_name,
        downloads: (release.assets || []).filter((asset) => /\.(exe|dmg|deb|appimage|flatpak|tar\.gz)$/i.test(asset.name)).reduce((sum, asset) => sum + asset.download_count, 0),
      }));
    }
  } catch {}

  // Fill in days with no visits so the chart has no gaps.
  const byDay = new Map(daily.map((row) => [row.day, row]));
  const series = [];
  for (let i = days - 1; i >= 0; i--) {
    const day = new Date(Date.now() - i * 86400000).toISOString().slice(0, 10);
    series.push(byDay.get(day) || { day, views: 0, visitors: 0, downloads: 0 });
  }
  const peak = Math.max(1, ...series.map((row) => row.views));
  const total = totals[0] || {};
  const table = (title, rows) => `<section><h2>${title}</h2><table>${rows.length ? rows.map((row) => `<tr><td>${escape(row.name || "(none)")}</td><td>${row.n}</td></tr>`).join("") : "<tr><td>Nothing yet</td></tr>"}</table></section>`;
  const bars = series.map((row) => `<div class="bar" title="${row.day}: ${row.views} views, ${row.visitors} visitors, ${row.downloads} downloads"><i style="height:${(row.views / peak) * 100}%"></i><b style="height:${(row.visitors / peak) * 100}%"></b></div>`).join("");

  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex"><title>Quintess visits</title>
<style>
body{margin:0;background:#0a0a0f;color:#ecebf2;font:15px/1.5 "Segoe UI",system-ui,sans-serif;padding:28px clamp(16px,4vw,48px)}
h1{font-size:24px;margin:0 0 4px}p{color:#a2a1b0;margin:0 0 22px}a{color:#9b82ff}
.totals{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:14px;margin-bottom:24px}
.totals div{border:1px solid #2a2a36;padding:14px 16px;background:#101017}.totals b{display:block;font-size:30px}.totals span{color:#a2a1b0;font-size:13px}
.chart{display:flex;align-items:flex-end;gap:2px;height:180px;border:1px solid #2a2a36;padding:12px;background:#101017;margin-bottom:26px}
.bar{flex:1;height:100%;position:relative}.bar i,.bar b{position:absolute;bottom:0;left:0;right:0;display:block}.bar i{background:#2f2560}.bar b{background:#7c5cff;left:25%;right:25%}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:22px}
h2{font-size:13px;text-transform:lowercase;letter-spacing:.04em;color:#9b82ff;margin:0 0 8px;font-family:Consolas,monospace}
table{width:100%;border-collapse:collapse}td{padding:6px 0;border-bottom:1px solid #1d1d27}td:last-child{text-align:right;color:#a2a1b0}
</style></head><body>
<h1>Quintess visits</h1>
<p>Last ${days} days · <a href="?days=7">7</a> · <a href="?days=30">30</a> · <a href="?days=90">90</a> · <a href="?days=365">365</a> · visitors are counted once a day, without cookies</p>
<div class="totals">
<div><b>${total.visitors || 0}</b><span>daily visitors, summed</span></div>
<div><b>${total.views || 0}</b><span>page views</span></div>
<div><b>${total.downloads || 0}</b><span>download clicks</span></div>
<div><b>${total.github || 0}</b><span>GitHub link clicks</span></div>
<div><b>${releases.reduce((sum, release) => sum + release.downloads, 0)}</b><span>package downloads (GitHub, all time)</span></div>
</div>
<div class="chart">${bars}</div>
<div class="grid">
${table("pages", pages)}${table("referrers", referrers)}${table("countries", countries)}${table("devices", devices)}${table("browsers", browsers)}
${table("package downloads by release", releases.map((release) => ({ name: release.tag, n: release.downloads })))}
</div></body></html>`;
  return new Response(html, { headers: { "content-type": "text/html; charset=utf-8", "Cache-Control": "no-store", "X-Robots-Tag": "noindex" } });
}
