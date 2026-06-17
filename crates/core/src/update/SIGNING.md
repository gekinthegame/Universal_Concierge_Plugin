# Autoupdater signing keys

The autoupdater's integrity rests entirely on **offline Ed25519 keys whose public
half is baked into the binary** (`baseline.rs`). IPNS and GitHub only provide mutable
"latest" pointers; they are never the trust anchor (Decision D-AU-3).

## Keys currently baked

Local/dev builds use placeholder public keys in `baseline.rs`:

```
rules: 0901e6ac503835e62f9a83f02eabb79ea9941c0d11dd6e3430a85345b8508308
app:   f3ca8179a00248c6af8e12278afbdbdba3f4d6e75191d6d84e0226ad5b1923a0
```

Their private halves were intentionally discarded. They keep local builds honest
without committing a usable signing secret. Release builds should inject real public
keys via `UCP_RULES_PUBKEYS_HEX` and `UCP_APP_PUBKEYS_HEX` so the binary bakes keys
that match the private signing material held outside source control.

## Production custody (plan §9 — the open risk)

Before shipping real updates:

1. Generate a fresh keypair on an **air-gapped** machine (or an HSM / hardware
   security key — YubiKey supports Ed25519). Never let the secret touch CI.
2. Bake the public halves into release builds with `UCP_RULES_PUBKEYS_HEX` and
   `UCP_APP_PUBKEYS_HEX`. The values may be comma-separated to support rotation.
3. Store the secret as the `RULES_SIGNING_KEY` / `APP_SIGNING_KEY` GitHub Actions
   secrets **only** if you
   accept Actions-secret custody as the weak v1; prefer an external/offline signer.
4. Add a backup public key so a lost primary does not brick the channel — recovery
   reuses the project's append-only rotation-log pattern (`recovery.rs`).

## What the signature covers

### Rules manifest (`verify::Manifest::signing_bytes`)

A fixed, newline-delimited payload with a domain-separation prefix — reproduce it
**byte-for-byte** in any signer:

```
ucp-rules-manifest-v1\n{epoch}\n{version}\n{cid}\n{sha256}\n{ts}\n{engine}
```

The 64-byte raw signature is base64 (standard, padded) in `manifest.sig`.

### Release binaries (app channel)

`release.yml` emits `SHASUMS256.txt` and a **detached** `SHASUMS256.txt.sig` — the raw
64-byte Ed25519 signature over the exact bytes of `SHASUMS256.txt`. The updater verifies
the signature against `APP_PUBKEYS_HEX`, then verifies each asset's SHA-256 against the
now-trusted SHASUMS.

## Generating a manifest signature (reference)

```python
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
import base64
seed = bytes.fromhex(open("secret.hex").read().strip())
sk = Ed25519PrivateKey.from_private_bytes(seed)
payload = f"ucp-rules-manifest-v1\n{epoch}\n{version}\n{cid}\n{sha256}\n{ts}\n{engine}".encode()
print(base64.standard_b64encode(sk.sign(payload)).decode())
```
