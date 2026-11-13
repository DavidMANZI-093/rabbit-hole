// Run with: node --test workers/test/*.test.mjs

import assert from "node:assert/strict";
import { test } from "node:test";

import worker from "../src/index.js";

// ------ helpers ------

function memoryKv() {
  const store = new Map();
  return {
    get: async (k) => store.get(k) ?? null,
    put: async (k, v) => void store.set(k, v),
    delete: async (k) => void store.delete(k),
  };
}

function call(env, method, path, body) {
  const init = { method, headers: { Accept: "application/json" } };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    init.headers["Content-Type"] = "application/json";
  }
  return worker.fetch(new Request(`https://edge.test${path}`, init), env, {});
}

// ------ tests ------

const VICTIM = "https://victim.trycloudflare.com";
const ATTACKER = "https://attacker.trycloudflare.com";

test("a stranger cannot release someone else's code", async () => {
  const env = { RH_CODES: memoryKv() };
  const claim = await call(env, "POST", "/claim", { url: VICTIM });
  const { code } = await claim.json();

  const release = await call(env, "DELETE", `/c/${code}`);
  assert.notEqual(release.status, 200);
});

test("a released code cannot be re-pointed by a stranger", async () => {
  const env = { RH_CODES: memoryKv() };
  const claim = await call(env, "POST", "/claim", { url: VICTIM });
  const { code } = await claim.json();

  await call(env, "DELETE", `/c/${code}`);
  await call(env, "POST", "/claim", { url: ATTACKER, want: code });

  const lookup = await call(env, "GET", `/c/${code}`);
  assert.equal((await lookup.json()).url, VICTIM);
});
