---
title: Air-gapped machines
nav: Air-gapped machines
description: BackDown on a computer with no network: a checklist, licences checked offline, and updates carried over on a drive, their signature checked before they're installed.
order: 11
---

BackDown does its work without a network: indexing, hashing, duplicates, checks, browsing, freeing up drives, rescue and drive health all happen on the computer it runs on. That makes it suitable for a machine that's kept off every network, such as an evidence workstation or a lab computer. A licence is checked on the computer itself, and updates can come on a drive you carry over, checked against BackDown's signature before anything is installed.

Installing updates from a folder (`backdown update --from`) is part of **BackDown Forensic** ("Air-gapped deployment"); in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)). Checking a carried folder (`--from … --check`) and the rest of this page work in every edition.

## The checklist

On the air-gapped machine:

1. **Update checks are off.** BackDown doesn't look for updates unless someone turned that on. To confirm it, check that **Look for updates once a day** is unticked in Settings > Updates. In a terminal, the setting is `update_check` in `settings.json` in your [data folder](/docs/guides/settings-and-licences/#the-index): absent or `false` means off. If there's no `update.json` beside it, BackDown has never asked. On Linux:

   ```sh
   grep update_check ~/.local/share/backdown/settings.json   # nothing, or "update_check": false
   ls ~/.local/share/backdown/update.json                    # No such file: it never asked
   ```

   To rule it out whatever Settings say, start the background service with `BACKDOWN_NO_UPDATE_CHECK=1` in its environment (on Linux, `systemctl --user edit backdown`, then `Environment=BACKDOWN_NO_UPDATE_CHECK=1` under `[Service]`). Don't press **Check now**: with no network it only fails, but it's still a request.
2. **No other machines are added.** `backdown remote list` lists none, and the sidebar's **Machines** section is empty. Once a machine is added, the background service syncs it over ssh every few hours, so don't add one here. `backdown remote remove NAME` forgets one ([Several machines](/docs/guides/several-machines/)).
3. **Nothing listens beyond this computer.** The browser and the background service listen on 127.0.0.1 alone unless they're started with `--host`, so don't pass `--host` here.
4. **The licence is checked offline.** A licence key is checked against its signature on the computer, with no account and no network. Carry it over in a text file and add it from there:

   ```sh
   backdown license add - < /media/usb/licence.txt
   backdown license show
   ```

5. **Updates come from a folder you carry over**, with `backdown update --from` ([below](#carry-an-update-over)).
6. **Check a carried folder before you install from it**, with `backdown update --from FOLDER --check`: on the connected computer before you carry it over, and again on the air-gapped one.

On Linux, the machine-wide service is cut off from any network beyond the computer by systemd itself, whatever BackDown does ([a machine-wide service](/docs/guides/settings-and-licences/#a-machine-wide-service)).

## Carry an update over {#carry-an-update-over}

On a computer with a network, open the release on [BackDown's releases page](https://github.com/BuenGenio/backdown-releases/releases) and download three files into one folder, on a USB drive say:

- the program for the air-gapped machine's system: `backdown-v…-linux-x86_64.tar.gz`, `-linux-arm64.tar.gz`, `-macos-universal.tar.gz` or `-windows-x86_64.zip`;
- `SHA256SUMS.txt`, the SHA-256 of every file in the release;
- `SHA256SUMS.txt.sig`, the signature of `SHA256SUMS.txt`, made with BackDown's release key.

If BackDown there was installed from a package (the `.deb`, an AppImage, the `.dmg` or the `.msi`), take the new package instead of the program, or as well.

Check the folder before you carry it over:

```sh
backdown update --from /media/usb --check
```

```text
  SHA256SUMS.txt is signed with BackDown's release key
  ok  backdown-v1.0.0-linux-x86_64.tar.gz
1 file in /media/usb matches SHA256SUMS.txt, which is signed with BackDown's release key.
Here (linux-arm64): there's no backdown-v1.0.0-linux-arm64.tar.gz in the folder, so it can't update this computer (it can still update others).
Nothing was changed.
```

`--check` checks every file in the folder that `SHA256SUMS.txt` lists, for any system, and says which files it doesn't list. It changes nothing, and exits with 1 if anything doesn't match.

Then, on the air-gapped machine:

```sh
backdown update --from /media/usb --check
backdown update --from /media/usb
```

```text
  SHA256SUMS.txt is signed with BackDown's release key
  SHA-256 of backdown-v1.0.0-linux-x86_64.tar.gz matches SHA256SUMS.txt
  installed BackDown 1.0.0 in /home/you/.local/bin/backdown
Updated BackDown 1.0.0-beta.4 → 1.0.0 (from /media/usb).
restarted the BackDown service
```

In that order, it:

1. checks the signature of `SHA256SUMS.txt`, against the release key built into BackDown. A signature that doesn't match stops everything: the files weren't signed by BackDown, or were changed after they were;
2. copies the program next to the one you have, and checks its SHA-256 against `SHA256SUMS.txt`;
3. checks that it's newer than the version you have;
4. installs it the way an [update over the network](/docs/guides/settings-and-licences/#updates) does: it checks the new program runs and says the expected version, keeps the old one in your data folder (`previous/`), swaps them, restarts the background service, and puts the old one back if the new one doesn't start.

It makes no network request at all. `backdown update --rollback` puts back the version it replaced.

| Option | What it does |
|---|---|
| `--check` | Only check the folder; change nothing |
| `--allow-unsigned` | Go ahead without `SHA256SUMS.txt.sig`. The checksums then show only that the files arrived whole, not that they're BackDown's: whoever could change the files could change `SHA256SUMS.txt` too. A signature that's there but doesn't match is refused even so |
| `--allow-older` | Install it even if it isn't newer than the version you have (or is the same) |

Releases before v1.0.0-beta.4 weren't signed, so updating from one of those needs `--allow-unsigned`.

### Installed from a package

A copy of BackDown installed from a package can't replace itself, so `backdown update --from` says what to do with the carried files instead, and checks the package against `SHA256SUMS.txt` first. For the `.deb`:

```text
backdown: /usr/bin/backdown is managed by your package manager (it came in a .deb or another package): install the new package from the carried files instead: sudo apt install /media/usb/backdown-io_1.0.0_amd64.deb (backdown-io_1.0.0_amd64.deb matches SHA256SUMS.txt, which is signed with BackDown's release key)
```

For an AppImage, use the new AppImage in place of the old one; for BackDown.app, open the new `.dmg`; for the `.msi`, run the new `.msi`, which upgrades the one you have.

## Check a signature without BackDown

To check a release on a computer without BackDown, or as a second opinion, OpenSSL 3 can check the signature. This is BackDown's release key (`b88db195d0c4084e5ae25f07fbf2f046939021deca58dce2d4e2591875d9e2a8`) as a public-key file:

```sh
cat > backdown-release-key.pem <<'KEY'
-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAuI2xldDECE5a4l8H+/LwRpOQId7KWNzi1OJZGHXZ4qg=
-----END PUBLIC KEY-----
KEY
# the signature is base64url, one line: turn it into raw bytes
(tr '_-' '/+' < SHA256SUMS.txt.sig | tr -d '\n'; echo '==') | base64 -d > SHA256SUMS.txt.sig.bin
openssl pkeyutl -verify -pubin -inkey backdown-release-key.pem -rawin -in SHA256SUMS.txt -sigfile SHA256SUMS.txt.sig.bin
sha256sum -c --ignore-missing SHA256SUMS.txt            # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

`Signature Verified Successfully`, and `OK` for each file you have, mean the files are the ones BackDown released.

## What can reach the network

Nothing in BackDown reaches the network unless you ask for it. These are the only things that can:

- **Update checks and updates over the network**: **Check now**, **Look for updates once a day**, **Update now**, and `backdown update` without `--from`. They ask GitHub's list of BackDown releases and download from it.
- **Other machines**: adding, testing, syncing and browsing a machine over ssh, and the background service's sync of added machines every few hours.
- **`--host`**: listening on an address other than 127.0.0.1, so other devices can look.
- **Opening a file in another app** from the browser: that app may go online by itself.

A licence, indexing, hashing, checking, the report, mirrors, Rescue, drive health and notifications never do. (A drive or a mirror on a network share is read and written through the system, like any other folder.)
