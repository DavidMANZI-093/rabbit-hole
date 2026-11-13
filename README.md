## rabbit-hole

**`rh` in short.**

rabbit-hole is a command-line file transfer utility for sharing **files and folders as they are**. It doesn't require archiving, repackaging, or storing anything in between.

sender's end,

```bash
rh serve ./photos
```

receiver's end,

```bash
rh fetch URL_OR_CODE ./received
```

No account. No archive. No file storage in the middle.

### 1. Install

**Linux / macOS &mdash; [source](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/install.sh)**

```bash
curl -fsSL https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/install.sh | bash
```

**Windows (PowerShell) &mdash; [source](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/install.ps1)**

(_May require restarting your shell to reload `PATH`._)

```powershell
irm https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/install.ps1 | iex
```

The installer places `rh` under your user directory, fetches a [pinned](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/third-party/cloudflared.pin) `cloudflared` version (for [encrypted tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/#:~:text=Cloudflare%20Tunnel%20provides,that%20bypass%20Cloudflare.)), and does not require root access.

After installing, run:

```bash
rh check
```

This reports whether `rh` can find a compatible `cloudflared`.

#### i. Uninstall

**Linux / macOS &mdash; [source](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/uninstall.sh)**

```bash
curl -fsSL https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/uninstall.sh | bash
```

**Windows (PowerShell) &mdash; [source](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/uninstall.ps1)**

```powershell
irm https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/uninstall.ps1 | iex
```

### 2. Quick start

#### i. Send

```bash
rh serve ./photos
```

`rh` prints something like:

```text
...
    url: https://example.trycloudflare.com
  local: http://192.168.1.42:54321
secured: false (secure with '--secure' flag)

fetch with, `rh fetch https://example.trycloudflare.com <dest>`
```

Share the printed URL or code with the receiver, then keep the process running.

#### ii. Receive

```bash
rh fetch https://example.trycloudflare.com ./received
```

If the sender printed a short code instead,

```bash
rh fetch a1b2c3 ./received
```

The destination directory is created if needed. Matching files are reused; conflicting files stop the transfer unless you pass `--force`.

### 3. Why rh

- **Share files as they are.** No zip/tar or repackaging step.
- **Nothing stored in between.** The sender serves the data; the receiver downloads it.
- **Hash-verified blocks.** Every block is checked before it is written.
- **Resumable by design.** Already-verified blocks can be skipped on a later fetch.
- **Public or local.** Use a public tunnel, or stay on the local network with `--local`.
- **Small surface area.** One binary, no account, no daemon, no background service.

### 4. Usage

#### i. Serve a file or folder

```bash
rh serve ./report.pdf
rh serve ./photos
```

Directory contents are shared as they are.

#### ii. Fetch to a destination

```bash
rh fetch URL_OR_CODE ./received
```

Use the direct HTTP(S) URL, or the 6-character short code if the build is configured with a short-code service. Official [releases](https://github.com/DavidMANZI-093/rabbit-hole/releases/) include a default short-code service.

#### iii. LAN-only mode

```bash
rh serve --local ./photos
```

`--local` skips the public tunnel and short code. Use the printed local URL. The listener still binds to all interfaces, so use a firewall if you need to restrict access to a specific network.

#### iv. Protected shares

```bash
rh serve --secure ./photos
```

This generates a bearer token and requires it when fetching:

```bash
rh fetch URL ./received --bearer TOKEN
```

`--secure` is authentication, not encryption. LAN traffic is HTTP; public tunnel traffic is HTTPS.

#### v. Replace conflicting files

```bash
rh fetch --force URL_OR_CODE ./received
```

Without `--force`, a destination file that differs from the shared copy aborts the fetch. With it, only conflicting target files are written; matching blocks and unrelated files are preserved.

#### vi. Tuning

The defaults are fine for most transfers. These are available when needed:

- `--block-size <SIZE>` controls hashing and transfer granularity. Smaller blocks improve deduplication and resume granularity; larger blocks reduce request count.
- `--timeout <MS>` sets the per-request timeout, in milliseconds.
- `--concurrency <N>` sets concurrent block downloads.
- `--edge <URL>` overrides the short-code service URL for testing or self-hosting.

Run `rh help` for an overview, or `rh help serve` / `rh help fetch` for full options.

### 5. How it works

1. `rh serve` scans the source path and builds a manifest describing the files and their layout.
2. The receiver downloads the manifest.
3. The receiver downloads missing blocks and verifies each block's hash.
4. The receiver reconstructs the files under the destination path.

The manifest is portable and does not depend on the sender's local paths. File data is never archived into a single blob; it is served block by block.

### 6. Security and privacy

- **Integrity:** each block is verified against its [BLAKE3](https://www.ietf.org/archive/id/draft-aumasson-blake3-00.html) hash before being written.
- **No file storage in between:** the short-code service maps codes to sender URLs; it does not store shared file contents (see [source](https://github.com/DavidMANZI-093/rabbit-hole/tree/main/workers)).
- **Transport:** the public tunnel uses HTTPS. LAN-only mode uses HTTP.
- **Authentication:** `--secure` adds a bearer token. It does not encrypt local traffic.
- **Local exposure:** `--local` does not bind only to the LAN interface. It listens on all interfaces; use the host firewall if that matters for your network.

### 7. Requirements

- Linux, macOS, or Windows on a supported 64-bit architecture.
- `cloudflared` for public tunneling. The installers provide a [pinned](https://raw.githubusercontent.com/DavidMANZI-093/rabbit-hole/v1.0.0/third-party/cloudflared.pin) copy; `rh check` reports its status.
- Rust with edition 2024 support, only when building from source.

`--local` does not need a public tunnel. A missing or outdated `cloudflared` only prevents public sharing, not local-network transfers.

### 8. Troubleshooting

#### i. `rh check` says cloudflared is missing

Install or reinstall the pinned companion binary with the installer, or put a compatible `cloudflared` on your `PATH`.

#### ii. The public URL is not reachable yet

The tunnel can take a moment to become available. Keep the sender running and retry the fetch. If it still fails, check your network and run `rh check`.

#### iii. `401` while fetching

The sender used `--secure`. Pass the exact token printed by `rh serve`:

```bash
rh fetch URL ./received --bearer TOKEN
```

#### iv. The short code is unknown or expired

Use the direct URL printed by the sender instead, or restart the share to generate a new code.

#### v. Fetch stops because destination files differ

Review the destination, then rerun with `--force` if replacing those files is intended.

#### vi. Local-network fetch cannot connect

Confirm both machines are on the same network, use the printed `local` URL, and check the sender's firewall. `--local` listens on all interfaces, but the host firewall may still block the port.

### 9. Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The debug binary is written to `target/debug/rh`.

### 10. License

MIT. See [LICENSE](https://github.com/DavidMANZI-093/rabbit-hole/blob/main/LICENSE).
