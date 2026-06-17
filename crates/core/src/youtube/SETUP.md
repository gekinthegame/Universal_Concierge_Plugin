# YouTube upload — Google OAuth setup

Uploading to YouTube uses the official **YouTube Data API v3** with a per-user OAuth
login. Unlike Firebase (which can reuse the public Firebase-CLI OAuth client), there
is **no public YouTube client**, so a real build must supply its own Google Cloud
"Desktop" OAuth client. Local/dev builds carry a `UNSET` placeholder and the connect
flow fails loudly at the consent screen until a real client is baked in.

## 1. Create the Google client (one time)

1. Create a Google Cloud project.
2. Enable **YouTube Data API v3**.
3. Configure the **OAuth consent screen** (External; add yourself as a test user
   while unverified).
4. Create **OAuth client credentials → Application type: Desktop app**.
5. Note the **client ID** and **client secret**.

Scopes requested (see `youtube.rs`):

- `https://www.googleapis.com/auth/youtube.upload` — the narrow upload scope (plan §2).
- `https://www.googleapis.com/auth/youtube.force-ssl` — required for Phase 3
  (captions + playlist management). A user can decline it; uploads still work, and
  the Phase-3 extras then fail with a clear scope error.

## 2. Bake the client into the build

Inject at compile time (matches the autoupdater key convention):

```sh
UCP_YT_CLIENT_ID="<your-id>.apps.googleusercontent.com" \
UCP_YT_CLIENT_SECRET="<your-secret>" \
cargo build --release -p concierge-plugin
```

`youtube::oauth_configured()` returns `false` for unset builds so the GUI/CLI can show
a "YouTube uploads aren't configured in this build" message instead of opening a broken
consent screen.

> A Desktop client's secret is **not** a true secret (it ships in the binary, exactly
> as the Firebase CLI client does). Security comes from the user's own OAuth consent
> and the per-user refresh token stored locally in the vault — never the client secret.

## 3. Verification / audit (before public uploads)

YouTube restricts uploads from **unverified** API projects created after 2020-07-28 to
**private** viewing until the project passes audit review. Plan rollout Phase 4 tracks
the verification work; until then, keep the default privacy at `private`/`unlisted`.

## 4. Where tokens live

OAuth access/refresh tokens are stored at `<store>/security/youtube.json` (0600, via the
same `atomic_private_write` vault path as deploy tokens). They are **never** written to
canvas folders, receipts, exported sites, or git-tracked files. `Disconnect` deletes the
file. Receipts (`<store>/youtube-upload-receipts.jsonl`) contain no secrets.
