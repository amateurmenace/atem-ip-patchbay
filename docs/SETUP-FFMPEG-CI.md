# Setting up the FFmpeg+DeckLink CI pipeline

This is a one-time, ~15-minute manual setup. After it's done, the
`.github/workflows/build-ffmpeg.yml` workflow can build FFmpeg
binaries with `--enable-decklink` for both Mac arm64 and Windows
x64, and the main app's installer will bundle them.

The reason the setup is manual (and can't be automated) is that
Blackmagic Design's DeckLink SDK requires accepting an end-user
license click-through on every download — there's no stable URL
or anonymous-mirror CI can curl against. So we mirror the SDK
**once** to a private repo, and CI downloads from there.

---

## What you'll be doing

1. Downloading the DeckLink SDK from blackmagicdesign.com
   (accepting the EULA).
2. Creating a **private** GitHub repo to host the SDK as a
   release asset.
3. Uploading the SDK to that private repo as a versioned release.
4. Generating a fine-grained PAT scoped read-only to that one
   repo.
5. Adding the PAT as a secret on **this** repo.

---

## Step 1 — Download the DeckLink SDK

1. Visit https://www.blackmagicdesign.com/support/family/capture-and-playback
2. Select your operating system (any — the SDK is multi-platform).
3. Find "Desktop Video SDK" in the list. As of 2026-05, the latest
   stable is **16.0** (the version pinned in `ci/ffmpeg-pins.env`).
4. Click the download link. You'll see a registration form — fill
   it in (you may need a Blackmagic Design account). Accept the
   EULA when prompted.
5. The download arrives as `Blackmagic_DeckLink_SDK_16.0.zip`
   (~30 MB). Keep this file; you'll upload it to the mirror.

> **Important:** if you bump `BMD_SDK_VERSION` in
> `ci/ffmpeg-pins.env` later, repeat steps 1-5 for the new SDK
> version and add it as a new release on the mirror (see step 3).
> The CI workflow downloads `v${BMD_SDK_VERSION}` from the mirror.

---

## Step 2 — Create the private mirror repo

```sh
gh repo create amateurmenace/bmd-decklink-sdk-mirror \
  --private \
  --description "Private mirror of Blackmagic DeckLink SDK for CI builds. Restricted by Blackmagic's EULA. Do not make public."
```

That's it. Don't commit anything; the repo just holds release
assets. We don't ever clone or check out — `gh release download`
fetches the asset directly.

---

## Step 3 — Upload the SDK as a release asset

```sh
gh release create v16.0 \
  --repo amateurmenace/bmd-decklink-sdk-mirror \
  --title "Blackmagic DeckLink SDK 16.0" \
  --notes "Mirror for CI. Source: blackmagicdesign.com (EULA-accepted download). Do not redistribute." \
  ./Blackmagic_DeckLink_SDK_16.0.zip
```

The tag (`v16.0`) is what `build-ffmpeg.yml` looks for. It must
match `BMD_SDK_VERSION` in `ci/ffmpeg-pins.env` exactly (the `v`
prefix is added by the workflow).

---

## Step 4 — Generate the fine-grained PAT

GitHub's classic PATs are too permissive; use a fine-grained PAT
scoped to ONLY the mirror repo, read-only.

1. Go to https://github.com/settings/personal-access-tokens/new
2. **Token name:** `bmd-sdk-mirror-read` (or anything memorable).
3. **Resource owner:** `amateurmenace` (your account).
4. **Expiration:** 90 days max for fine-grained tokens. Mark your
   calendar to regenerate (the CI workflow will start failing
   loudly the day after expiry).
5. **Repository access:** "Only select repositories" →
   `bmd-decklink-sdk-mirror`.
6. **Repository permissions:**
   - **Contents:** Read-only (lets `gh release download` fetch
     the asset).
   - **Metadata:** Read-only (implicit, required).
   - Everything else: leave as "No access".
7. Click "Generate token". **Copy the token immediately** —
   GitHub won't show it again.

---

## Step 5 — Add the PAT as a secret on this repo

```sh
echo "github_pat_xxxxxxxxxxxxxxxx" | gh secret set BMD_SDK_PAT \
  --repo amateurmenace/atem-ip-patchbay
```

(Replace `github_pat_xxxxxxxxxxxxxxxx` with the token you copied
from step 4.)

You can verify it landed:

```sh
gh secret list --repo amateurmenace/atem-ip-patchbay | grep BMD_SDK_PAT
```

Should show `BMD_SDK_PAT  Updated YYYY-MM-DD`.

---

## Step 6 — Dispatch the build to verify

```sh
gh workflow run build-ffmpeg.yml \
  --repo amateurmenace/atem-ip-patchbay
```

Watch the run:

```sh
gh run watch
```

First-time cold build is ~25 min for Mac arm64 and ~45 min for
Windows x64 (running in parallel — so wall clock is ~45 min).
Subsequent dispatches against the same pins re-do the full build
(no caching at this layer; the GitHub Release is the cache).

Successful build produces a prerelease at
`https://github.com/amateurmenace/atem-ip-patchbay/releases/tag/ffmpeg-decklink-8.1.1-bmd16.0-rev1`
with two assets:

- `ffmpeg-n8.1.1-macos-arm64.tar.gz` (~10 MB)
- `ffmpeg-n8.1.1-windows-x64.zip` (~80 MB)

These are what release.yml will switch to downloading once the
release.yml modifications land in a follow-up commit.

---

## After setup: what's next

1. **Verify the first build is green.** Look for the "FFmpeg
   ${FFMPEG_VERSION} includes decklink muxer" sanity-check line in
   the build logs. If it fails, iterate on configure flags in
   `.github/workflows/build-ffmpeg.yml` (most likely culprits:
   wrong header path inside the SDK zip, missing toolchain dep on
   MSYS2, or FFmpeg upstream renaming a config flag in a version
   bump).
2. **Land the release.yml changes.** A follow-up commit will swap
   the Mac block (lines ~113-129 of release.yml) and the Windows
   block (lines ~600-653) to use `gh release download` from the
   sidecar release instead of the jellyfin / gyan.dev curl steps.
   Also adds a post-staging muxer assertion to fail the main app
   build loudly if the FFmpeg lacks DeckLink (regression guard).
3. **Update CLAUDE.md.** Strike the "No prebuilt Windows FFmpeg
   ships --enable-decklink" item from "Open issues from Session 13"
   and add a Session 14 entry pointing at this pipeline.

---

## Bumping the FFmpeg or SDK version

To rebuild against a new FFmpeg release (e.g. n9.0):

1. Edit `ci/ffmpeg-pins.env` → set `FFMPEG_VERSION=n9.0`.
2. Commit + push. The paths-filter trigger fires
   `build-ffmpeg.yml` automatically.
3. Watch the run; iterate on configure flags if upstream FFmpeg
   changed any.

To rebuild against a new DeckLink SDK (e.g. 16.1):

1. Repeat steps 1-3 of this guide for the new SDK version.
2. Upload it as a new release on the mirror (e.g. `v16.1`).
3. Edit `ci/ffmpeg-pins.env` → set `BMD_SDK_VERSION=16.1`.
4. Commit + push; CI auto-triggers.

To force a rebuild without bumping either version (e.g. you
changed a configure flag):

1. Edit `ci/ffmpeg-pins.env` → bump `BUILD_REVISION=N` to `N+1`.
2. Commit + push.

---

## Troubleshooting

- **"BMD_SDK_PAT secret not configured"** — step 5 wasn't done,
  or the secret name is misspelled. Verify with `gh secret list`.
- **"Could not locate SDK root inside zip"** — Blackmagic
  changed the directory naming inside the SDK zip in a newer
  version. Inspect the extracted layout in the failed build's
  logs (search for `find /tmp/bmd-sdk/extracted` output) and
  adjust the `SDK_ROOT=$(find ...)` patterns in
  `build-ffmpeg.yml`.
- **"decklink muxer missing"** — likely a configure-flag issue
  (e.g. SDK headers not found at the path we passed to
  `--extra-cflags`). The configure log at the top of the build
  step lists every enabled muxer; check why decklink fell off.
- **PAT expired** — regenerate per step 4, update the secret
  per step 5. The workflow's `gh release download` step will
  print `gh: command failed` with an authentication error.
- **Mirror repo deleted** — re-create per step 2, re-upload
  per step 3. Tokens persist independent of the repo's
  existence.

---

## Why fine-grained PAT and not a classic PAT?

Classic PATs grant access to **every repo** on the account.
If leaked, attacker has full account write access. Fine-grained
PATs are scoped to specific repos + specific permissions —
this one grants read-only access to a single private repo, so
even a leak only exposes the BMD SDK download URL (still
guarded by GitHub auth).

The 90-day expiry is the trade-off. Set a calendar reminder.

---

## Why a separate repo and not a private branch of THIS repo?

Two reasons:

1. **Branch protection.** A protected `bmd-sdk-mirror` branch in
   this repo is visible to any contributor; a separate private
   repo is properly access-controlled.
2. **Token scoping.** The fine-grained PAT can grant read-only
   to ONLY the mirror repo. If we kept the SDK in a branch here,
   the PAT would need write access to this repo (with token
   permissions that propagate to all branches including main),
   which is over-broad.

The cost is the extra repo. Worth it for the access-control story.
