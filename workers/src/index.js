// rabbit-hole edge worker — Cloudflare Worker + KV.
//
// Protocol (mirrors src/edge.rs):
//   POST   /claim         {url, want?, owner?, ttl?} -> 200 {code, owner?} | 409 {error}
//   GET    /c/{code}      Accept: application/json -> 200 {url} | 404 {error}
//   DELETE /c/{code}      Authorization: Bearer <owner> -> 200 | 401 | 403
//   GET    /health                            -> 200
//
// Storage: KV key = code, value = {url, ownerHash}, expirationTtl = ttl seconds.
// Same-owner reclaim may update the URL and refresh TTL. Legacy raw-URL values
// remain readable until they expire.

// ------ constants ------

const CODE_RE = /^[0-9a-f]{6}$/i;
const OWNER_RE = /^[0-9a-f]{64}$/;
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

function randomHex(bytes) {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
}

function rollCode() {
  return randomHex(3);
}

function rollOwner() {
  return randomHex(32);
}

async function hashOwner(owner) {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(owner),
  );
  return Array.from(new Uint8Array(digest), (b) =>
    b.toString(16).padStart(2, "0"),
  ).join("");
}

function sameHash(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || a.length !== b.length) {
    return false;
  }
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }
  return diff === 0;
}

function parseRecord(raw) {
  if (raw === null) {
    return null;
  }
  try {
    const value = JSON.parse(raw);
    if (value && typeof value.url === "string") {
      return {
        url: value.url,
        ownerHash: typeof value.ownerHash === "string" ? value.ownerHash : null,
      };
    }
  } catch {
    // Legacy KV values are bare URLs.
  }
  return { url: raw, ownerHash: null };
}

async function putRecord(env, code, url, ownerHash, ttl) {
  await env.RH_CODES.put(code, JSON.stringify({ url, ownerHash }), {
    expirationTtl: ttl,
  });
}

async function allowLookup(req, env, log) {
  if (!env.RH_LOOKUP_LIMITER) {
    return true;
  }
  const ip = req.headers.get("CF-Connecting-IP") || "unknown";
  try {
    const { success } = await env.RH_LOOKUP_LIMITER.limit({
      key: `lookup:${ip}`,
    });
    if (!success) {
      log.warn("lookup: rate limited", ip);
    }
    return success;
  } catch (e) {
    log.error("lookup: rate limiter failed", e);
    return true;
  }
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

  const { url, want = null, owner = null, ttl = DEFAULT_TTL } = body ?? {};

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
  if (owner !== null && (typeof owner !== "string" || !OWNER_RE.test(owner))) {
    log.warn("claim: bad owner");
    return json({ error: "owner must be 64 lowercase hex chars" }, 400);
  }

  // Preferred code path: caller requests a specific code.
  if (want !== null) {
    if (typeof want !== "string" || !CODE_RE.test(want)) {
      log.warn("claim: bad want", want);
      return json({ error: "want must be 6 hex chars" }, 400);
    }
    const code = want.toLowerCase();
    const cur = parseRecord(await env.RH_CODES.get(code));
    if (cur === null) {
      const issuedOwner = rollOwner();
      const ownerHash = await hashOwner(issuedOwner);
      await putRecord(env, code, url, ownerHash, ttl);
      log.info("claim: issued (wanted)", code);
      return json({ code, owner: issuedOwner });
    }
    if (cur.ownerHash === null) {
      log.warn("claim: legacy code cannot be reclaimed", code);
      return json({ error: "legacy code; retry without want" }, 409);
    }
    if (owner === null) {
      log.warn("claim: missing owner for wanted code", code);
      return json({ error: "taken" }, 409);
    }
    if (!sameHash(await hashOwner(owner), cur.ownerHash)) {
      log.warn("claim: wrong owner for wanted code", code);
      return json({ error: "taken" }, 409);
    }
    await putRecord(env, code, url, cur.ownerHash, ttl);
    log.info("claim: refreshed (owned)", code);
    return json({ code });
  }

  // Server-rolled code path.
  for (let i = 0; i < ROLL_ATTEMPTS; i++) {
    const code = rollCode();
    if ((await env.RH_CODES.get(code)) === null) {
      const issuedOwner = rollOwner();
      const ownerHash = await hashOwner(issuedOwner);
      await putRecord(env, code, url, ownerHash, ttl);
      log.info("claim: issued (rolled)", code, `attempts=${i + 1}`);
      return json({ code, owner: issuedOwner });
    }
  }
  log.error("claim: keyspace exhausted after", ROLL_ATTEMPTS, "attempts");
  return json({ error: "keyspace full, retry" }, 503);
}

async function handleDelete(req, env, code, log) {
  const auth = req.headers.get("Authorization") || "";
  const match = /^Bearer\s+([0-9a-f]{64})$/i.exec(auth);
  if (!match) {
    log.warn("release: missing owner", code);
    return json({ error: "missing owner" }, 401);
  }

  const cur = parseRecord(await env.RH_CODES.get(code));
  if (cur === null) {
    return new Response("ok\n"); // idempotent: unknown and deleted look the same
  }
  if (cur.ownerHash === null) {
    log.warn("release: legacy code has no owner", code);
    return json({ error: "code has no owner" }, 403);
  }
  if (!sameHash(await hashOwner(match[1]), cur.ownerHash)) {
    log.warn("release: wrong owner", code);
    return json({ error: "wrong owner" }, 403);
  }

  await env.RH_CODES.delete(code);
  log.info("release:", code);
  return new Response("ok\n");
}

async function handleCode(req, env, code, log) {
  code = code.toLowerCase();

  if (req.method === "DELETE") {
    return handleDelete(req, env, code, log);
  }

  if (req.method !== "GET") {
    return new Response("method not allowed\n", { status: 405 });
  }

  if (!(await allowLookup(req, env, log))) {
    return json({ error: "rate limited" }, 429);
  }

  const record = parseRecord(await env.RH_CODES.get(code));
  if (record === null) {
    log.debug("lookup: unknown", code);
    return json({ error: "unknown" }, 404);
  }

  log.info("lookup:", code, "->", record.url);
  return json({ url: record.url });
}

// ------ entry point ------

export default {
  async fetch(req, env, _ctx) {
    const reqId = crypto.randomUUID().slice(0, 8);
    const log = makeLog(reqId);
    const url = new URL(req.url);
    const { pathname } = url;

    log.debug(req.method, pathname);

    if (req.method === "POST" && pathname === "/claim") {
      return handleClaim(req, env, log);
    }

    const codeMatch = /^\/c\/([^/]+)$/.exec(pathname);
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
