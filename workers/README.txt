rabbit-hole edge & deploy guide
===============================

1. Purpose
----------

The edge maps short codes to tunnel URLs. It stores no files. It never sees file
content. rh clients need no account. Only hosting needs a Cloudflare account.

rh releases point at the production edge by default. The rabbit-hole project
operates it. Section 4 describes how to run your own edge instead.

Terms used in this guide:

- edge:  this Cloudflare Worker.
- code:  a short string of 6 hex chars. It maps to one URL.
- KV:    Cloudflare key-value storage. Key = code. Value = URL.

2. Operation (hosted service)
-----------------------------

The production edge serves all default rh installs.

The operator sees request metadata: claimed URLs, lookup counts and timing, and
source IPs in request logs. Mappings expire by TTL. File contents never pass
through the edge. Only codes and tunnel URLs pass through it.

If the edge is unreachable, sharing still works. The client falls back to direct
URLs. No data is lost.

For log retention periods, refer to the Cloudflare Workers Logs documentation.
For self-hosting, see section 4.

3. Endpoints
------------

3.1. POST /claim — map a URL to a code.

Request fields:

- 'url' (required):  must start with 'http://' or 'https://'.
- 'want' (optional): preferred code. Must be 6 hex chars.
- 'ttl' (optional):  lifetime in seconds. From 1 to 86400. Default is 14400.

Responses:

- 200 + {"code"}:   the claim is stored.
- 409 + {"error"}:  the wanted code belongs to a different URL. Send
  another 'want' value, or send no 'want' value.
- 400 + {"error"}:  bad 'url', bad 'want', or bad 'ttl'.

NOTE: Both 'http://' and 'https://' URLs are accepted. 'http://' suits trusted
LANs and local development only. Over untrusted networks it forfeits everything
beyond block hashes: a meddler can substitute a whole self-consistent malicious
manifest. Share public codes for 'https://' URLs only.

NOTE: Re-share of the same URL keeps the same code and refreshes the TTL. This
is by design.

3.2. GET /c/{code} — resolve a code to a URL.

Responses:

- 200 + {"url"}:    the stored URL.
- 404 + {"error"}:  unknown or expired code. Wait and retry. KV
  replicates with delay.

Send header 'Accept: application/json'.

3.3. DELETE /c/{code} — release a code.

Response:

- 200 in all cases. Delete of an unknown code also returns 200.

3.4. GET /health (and GET /) — service check.

Response:

- 200 + 'ok'.

4. Deployment
-------------

Use this section to run your own edge. Default installs use the production edge
from section 2 and do not need these steps.

Prerequisites: a Cloudflare account (Workers Free is sufficient) and wrangler 4.
Wrangler 4 needs Node.js 22 or newer. Use a maintained LTS release. Check the
current LTS on the nodejs.org release schedule. Install wrangler by any method.
This guide does not cover that step.

Do these steps in the workers/ directory:

1. Sign up at dash.cloudflare.com. Workers Free is the default plan.
2. Log in: 'wrangler login'.
3. Create the KV namespace: 'wrangler kv namespace create RH_CODES'.
4. Put the printed id into wrangler.toml, field '[[kv_namespaces]].id'.
5. Deploy: 'wrangler deploy'.
6. Note the printed URL, for example https://rabbit-hole-edge.<you>.workers.dev.
7. Check the deployment: 'curl <url>/health'. It must return 'ok'.

CI deploys automatically on push to main when files under workers/ change
(see .github/workflows/edge.yml). Manual deploy stays valid.

For CI or headless use, do not use 'wrangler login'. Create a token from the
"Edit Cloudflare Workers" template instead. Export CLOUDFLARE_API_TOKEN and
CLOUDFLARE_ACCOUNT_ID.

CAUTION: Never commit tokens. Never commit the .wrangler/ directory. Both stay
local at all times.

5. Quotas
---------

Costs are per call. Free quotas reset daily at 00:00 UTC.

- POST /claim:  1 KV write. Failed validations cost nothing.
- GET /c/{code}:  1 KV read. Misses (404) also cost 1 read.
- DELETE /c/{code}:  1 KV delete. Deletes are free of quota cost
  but still count as a request.

Free plan caps: 100000 Worker requests/day, 100000 KV reads/day, 1000 KV
writes/day, 1 GB storage. One mapping is about 60 bytes.

Budget: about 1000 shares/day (write-bound) and about 100000 fetches/day.
Prototype use stays far below these caps.

If writes run out, shares keep working: failed claims fall back to the direct
URL by design. Fetches are unaffected.

6. Consistency and expiry
-------------------------

KV replicates with delay. A code claimed this second can return 404 the next
second in another region. On 'Unknown', retry the lookup.

TTL is enforced by KV 'expirationTtl'. Default is 14400 seconds (4 hours).
Maximum is 86400 seconds (24 hours). A crashed sender leaks at most one small
key per share until TTL expiry.

The server rolls random codes up to 64 times on collision. The rh client rolls
up to 8 times, then reports an error.

7. Abuse limits
---------------

The claim endpoint is open. It needs no token. Any caller can spend your 1000
daily KV writes. This is known and accepted for prototype use. If abuse occurs,
options are: lower TTL, front the worker with Access or Turnstile, or move
claims behind a token. Failed claims never break sharing: the client falls back
to the direct URL.

8. Observability
----------------

Logs and traces are on (see wrangler.toml, '[observability]'). View live logs
with: 'wrangler tail'. Each request carries a short id in the log tag
('[rh] [<id>]'). Use the id to follow one call through claim, lookup, and
release.

9. Compatibility
----------------

Client and edge must agree on three values: code shape (6 hex), default TTL
(14400), and route paths (/claim, /c/{code}). These are defined in src/edge.rs
(client) and src/index.js (edge). Change them together. There is no version
negotiation on the edge API. The file manifest protocol (RHB1) is versioned
separately and is not affected by edge changes.
