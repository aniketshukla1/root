# Releasing

A tag builds, signs and publishes installers. Three things have to be set up
once before the first one works.

## 1. The updater signing key

Separate from Apple's, and does a different job: Apple's signature proves the
installer came from a known developer, this one proves an *update* did. Without
it the updater installs whatever the endpoint serves, which turns a compromised
release host into code execution on every machine that ever installed root.

```bash
npm run tauri signer generate -- -w ~/.tauri/root.key
```

Put the **public** half in `src-tauri/tauri.conf.json` under
`plugins.updater.pubkey`, and the private half and its password in repository
secrets as `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Then point `plugins.updater.endpoints` at this repository.

The release workflow refuses to build while either placeholder is still there.

## 2. Apple Developer ID

Needed because an unsigned macOS bundle does not open with a warning — it opens
with *"root is damaged and can't be opened"*, which reads as a corrupt download.
Nobody files a bug about that; they delete it.

Export the **Developer ID Application** certificate as a `.p12`, then:

```bash
base64 -i certificate.p12 | pbcopy
```

| Secret | |
| --- | --- |
| `APPLE_CERTIFICATE` | that base64 |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | the Apple account used to notarize |
| `APPLE_PASSWORD` | an **app-specific** password, not the account password |
| `APPLE_TEAM_ID` | |

The workflow verifies the result with `codesign` and `spctl` rather than
trusting that it happened: `tauri-action` builds an unsigned bundle and says
nothing when these are missing, and shipping one is the failure this whole
setup exists to prevent.

## 3. Windows and Linux

Unsigned by default, and they build and run. Windows shows a SmartScreen
warning without a code-signing certificate; add `WINDOWS_CERTIFICATE` and
`WINDOWS_CERTIFICATE_PASSWORD` and set `bundle.windows.certificateThumbprint`
when you have one.

## Cutting a release

```bash
# Keep both versions in step: the updater compares them.
npm version 0.2.0 --no-git-tag-version
# and set "version" in src-tauri/tauri.conf.json to match

git commit -am "0.2.0"
git tag v0.2.0
git push origin main --tags
```

The workflow publishes a **draft** release. Check the installers, then publish
it — `latest.json` only becomes live at that point, so a bad build is never
offered to anyone running the app.

## The server

Not part of this. It ships as a container from `deploy/`, which CI builds on
every push, and it has no updater: `docker compose pull && up -d` is the whole
upgrade path.
