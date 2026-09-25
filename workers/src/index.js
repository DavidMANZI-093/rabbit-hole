// rabbit-hole edge worker — Cloudflare Worker + KV.
//
// Protocol (mirrors src/edge.rs):
//   POST   /claim         {url, want?, ttl?}  -> 200 {code} | 409 {error}
//   GET    /c/{code}      Accept: application/json -> 200 {url} | 404 {error}
//   DELETE /c/{code}                          -> 200 (idempotent)
//   GET    /health                            -> 200
//
// Storage: KV key = code, value = tunnel URL, expirationTtl = ttl seconds.
// Same-URL reclaim is idempotent: re-share keeps the code and refreshes TTL.

// ------ constants ------

const CODE_RE = /^[0-9a-f]{4,6}$/i;
const DEFAULT_TTL = 14_400; // 4 hours, matching DEFUALT_TTL_SECS in edge.rs
const MAX_TTL = 86_400; // seconds, 24 hours
const ROLL_ATTEMPTS = 64; // afford more rolls than client (8)

// ------ logger ------

function makeLog(reqId) {
  const tag = `[rh] [${reqId}]`;
  return {
    info: (...a) => console.log(tag, ...a),
    warn: (...a) => console.warn(tag, ...a),
    error: (...a) => console.error(tag, ...a),
    debug: (...a) => console.debug(tag, ...a),
  };
}

// ------ helpers ------

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function rollCode() {
  return Math.floor(Math.random() * 0x1000000)
    .toString(16)
    .padStart(6, "0");
}

// ------ handlers ------

async function handleClaim(req, env, log) {
  let body;
  try {
    body = await req.json();
  } catch {
    log.warn("claim: unparseable body");
    return json({ error: "bad json" }, 400);
  }

  const { url, want = null, ttl = DEFAULT_TTL } = body ?? {};

  if (
    typeof url !== "string" ||
    !(url.startsWith("http://") || url.startsWith("https://"))
  ) {
    log.warn("claim: bad url", url);
    return json({ error: "url must be http(s)://" }, 400);
  }
  if (!Number.isInteger(ttl) || ttl < 1 || ttl > MAX_TTL) {
    log.warn("claim: bad ttl", ttl);
    return json({ error: `ttl must be 1..${MAX_TTL} seconds` }, 400);
  }

  // Preferred code path: caller requests a specific code.
  if (want !== null) {
    if (typeof want !== "string" || !CODE_RE.test(want)) {
      log.warn("claim: bad want", want);
      return json({ error: "want must be 4-6 hex chars" }, 400);
    }
    const code = want.toLowerCase();
    const cur = await env.RH_CODES.get(code);
    if (cur === null || cur === url) {
      await env.RH_CODES.put(code, url, { expirationTtl: ttl });
      log.info("claim: issued (wanted)", code);
      return json({ code });
    }
    log.debug("claim: collision on wanted code", code);
    return json({ error: "taken" }, 409);
  }

  // Server-rolled code path.
  for (let i = 0; i < ROLL_ATTEMPTS; i++) {
    const code = rollCode();
    if ((await env.RH_CODES.get(code)) === null) {
      await env.RH_CODES.put(code, url, { expirationTtl: ttl });
      log.info("claim: issued (rolled)", code, `attempts=${i + 1}`);
      return json({ code });
    }
  }
  log.error("claim: keyspace exhausted after", ROLL_ATTEMPTS, "attempts");
  return json({ error: "keyspace full, retry" }, 503);
}

async function handleCode(req, env, code, log) {
  code = code.toLowerCase();

  if (req.method === "DELETE") {
    await env.RH_CODES.delete(code); // idempotent: 404 and 200 look the same to caller
    log.info("release:", code);
    return new Response("ok\n");
  }

  if (req.method !== "GET") {
    return new Response("method not allowed\n", { status: 405 });
  }

  const target = await env.RH_CODES.get(code);
  if (target === null) {
    log.debug("lookup: unknown", code);
    return json({ error: "unknown" }, 404);
  }

  log.info("lookup:", code, "->", target);
  return json({ url: target });
}

// ------ entry point ------

export default {
  async fetch(req, env, ctx) {
    const reqId = crypto.randomUUID().slice(0, 8);
    const log = makeLog(reqId);
    const url = new URL(req.url);
    const { pathname } = url;

    log.debug(req.method, pathname);

    if (req.method === "POST" && pathname === "/claim") {
      return handleClaim(req, env, log);
    }

    const codeMatch = pathname.match(/^\/c\/([^/]+)$/);
    if (codeMatch && CODE_RE.test(codeMatch[1])) {
      return handleCode(req, env, codeMatch[1], log);
    }

    if (req.method === "GET" && (pathname === "/" || pathname === "/health")) {
      return new Response("ok\n");
    }

    log.warn("not found:", req.method, pathname);
    return new Response("not found\n", { status: 404 });
  },
};
