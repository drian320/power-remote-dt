# Phase 3a — Encrypted Pipeline Smoke Test

## What changed in Phase 3a

All host ↔ viewer UDP traffic is now encrypted with Noise_NK
(Curve25519 + ChaCha20-Poly1305 + BLAKE2s). The host holds a long-term
static key; the viewer pins the host's public key out-of-band.

## Running

### Host

```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
.\target\release\prdt-host.exe --bind 127.0.0.1:9000 --monitor 0 `
    --bitrate-mbps 20 --key-file host-key.bin
```

First run generates `host-key.bin` (32 bytes of Curve25519 private key) and
prints:
```
Host public key: AbC123...XyZ0
(Pass --host-pubkey AbC123...XyZ0 to the viewer)
```

Second+ run loads the existing key file. Public key is the same across
runs (viewer's pinned key doesn't need to rotate).

### Viewer

Copy the public key from host stdout, then:
```powershell
.\target\release\prdt-viewer.exe --host 127.0.0.1:9000 `
    --host-pubkey AbC123...XyZ0
```

## Expected behavior

**Host log:**
```
INFO host starting ...
INFO listening; waiting for Noise handshake
INFO Noise handshake complete — encrypted channel established
INFO handshake complete
```

**Viewer log:**
```
INFO Noise handshake complete
```

Then normal video/input flow, identical to Phase 3 but on-wire encrypted.

## Known limitations

- **Wrong `--host-pubkey` on viewer makes it appear frozen.** No internal
  timeout in `handshake_as_client` yet. Workaround: if you don't see
  "Noise handshake complete" on the viewer within a few seconds, Ctrl-C
  and double-check the pubkey matches host's output.
- **Key file is plaintext.** `host-key.bin` is a 32-byte binary on disk
  with default Windows ACL. Treat it as a secret; anyone who reads it
  can impersonate your host. DPAPI protection is a future enhancement.
- **No key rotation.** Session keys live for the whole session. Long
  sessions should re-key periodically; deferred.
- **TOFU trust model.** Viewer blindly trusts whatever pubkey you paste
  in. A MITM at first-key-distribution can swap it. Phase 3b could add
  a known-hosts file.

## Troubleshooting

- **"invalid --host-pubkey"**: malformed base64. Copy the whole string
  after "Host public key: " (no-pad base64, 43 characters).
- **"crypto: Snow(...)"** on host logs: likely either a corrupted E1
  (network glitch) or a viewer using the wrong pubkey. Safe to ignore —
  the host will log and re-enter the handshake loop waiting for a
  correct E1 from any viewer.
