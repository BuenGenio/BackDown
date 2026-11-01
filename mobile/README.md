# BackDown for Android and iOS

A Flutter app for the BackDown service on your computers: your drives, folders and files, search
across every drive, what the service is doing, drive health, other machines and the swarm, what's
recognised, duplicates, and the history of plans, freeing up and rescues. It talks to the service
directly, over a tailnet or LAN. Nothing goes through backdown.io.

## Connecting

The service listens on the computer only (127.0.0.1) unless it's started with `--host`:

```sh
backdown service install --host 100.x.y.z   # this computer's Tailscale address, or a LAN one
backdown token --links                       # the link for other devices, with its token
```

In the app: **Paste the link** (or scan the QR code), or **Try the demo**, which needs no network.

From another device the service only lets you look, plus queue a rescan: copying, freeing up,
rescuing, mounting and settings need someone at the computer (`serve.rs`, `refused_from`). The app
follows that. Actions come back as "Needs someone at the computer", explained in a dialog.

## Develop

Flutter 3.47 (Dart 3.13). From `mobile/`:

```sh
flutter pub get
flutter analyze                  # must be clean
flutter test                     # widget and unit tests, against the demo
flutter test screenshots/        # real-font PNGs of every screen in build/shots/ (or $SHOTS_DIR)
flutter run -d linux             # the app on this computer, against a real service
flutter build apk --release      # needs an x86-64 machine with the Android SDK (aapt2 isn't built for ARM64 Linux)
```

Release signing: `android/key.properties` (`storeFile`, `storePassword`, `keyAlias`,
`keyPassword`) or the `BACKDOWN_KEYSTORE_*` environment variables. Without them, release builds use
the debug key.

## How it's built

```
lib/
  main.dart, app.dart     start-up (preferences, servers, background), MaterialApp.router
  router.dart             Routes: every screen's path; each area mounts its own routes
  shell.dart              the tabs: Drives, Search, Activity, Health, More (a rail when wide)
  theme.dart              BackDown's colours (ui/app.css), BdColors (context.bd), Wordmark
  providers.dart          servers, apiProvider/requireApi, apiGetProvider/apiJsonProvider,
                          roots/listing/file/timeline, daemonProvider (the long poll), skewProvider
  api/                    BackDownApi: HttpBackDownApi (the service) and DemoBackDownApi (examples)
    json.dart             Json reading that never throws on odd types (j.integer('x'), j.list('y'))
    keys.dart             path keys (unpadded base64url of the path's bytes), baseName
  models/                 browse.dart (Root, Listing, Entry, FileInfo, FindHit…), daemon.dart (Job…)
  core/                   format.dart (bytes, count, when, rel, dur), kinds.dart (FileKind by
                          extension, as ui/icons.js), widgets.dart (AsyncBody, ErrorPanel,
                          EmptyState, SectionHeader, InfoRow, StatTile, Pill, Meter, showSnack),
                          actions.dart (runAction, canChange, LookOnlyNote), content_image.dart
                          (a file's picture through the API), file_io.dart (share, open in…)
  servers/                saved servers (preferences) and their tokens (secure storage)
  demo/                   the demo's made-up computer: demo_fs.dart (its drives), one file of
                          answers per area (demo_core, demo_viewers, demo_activity, demo_machines,
                          demo_library), merged in api/demo_api.dart
  features/<area>/        each area's screens, its providers, and its routes.dart
  background/             the background check and its notifications
test/                     widget tests; test/support/harness.dart pumps the app on the demo
screenshots/              screenshot tests (test/support/shots.dart: loadFonts, shoot)
```

Conventions:

- **Data**: a screen reads through a provider (`FutureProvider.autoDispose`, often `.family`) that
  calls `requireApi(ref).get('endpoint', {...})` and builds a model with the `Json` helpers. Show it
  with `AsyncBody(value:, data:, onRetry: () => ref.invalidate(p))`, and refresh with a
  `RefreshIndicator`. Untyped reads can use `apiJsonProvider(ApiQuery('endpoint', {...}))`.
- **Actions** (POST): `runAction(context, ref, (api) => api.post('x', params: {...}), done: '…')`.
  It explains "needs someone at the computer" and paid-edition refusals. Put `LookOnlyNote` at the top
  of a screen whose actions are made at the computer.
- **Live state**: `daemonProvider` keeps `/api/daemon` current with the long poll. Use
  `skewProvider` with `rel()` so times read right when the phone's clock is off.
- **Wording**: as the web UI says it (`crates/backdown-cli/ui/*.js`): plain sentences, no jargon,
  sentence case, "drive" not "volume" unless it is one.
- **Demo**: every screen works on the demo. Add the area's answers to its `lib/demo/demo_<area>.dart`
  (`DemoHandler = (DemoBackDownApi d, Map<String, String> params) => json`), using `DemoFs.instance`
  for files. Keep state in `d.state['<area>']`, and call `d.changed()` after a change so long polls
  answer. Don't start timers in the demo except under `d.animate`, since tests must not leave timers
  running.
- **Tests**: `pumpBackDown(tester, location: …)` then `find…`; `goTo(tester, container, route)`.
- **Style**: `dart format` (120 columns), single quotes, `flutter analyze` clean, no `print`.
- **Dependencies**: each one has a comment in pubspec.yaml with its licence and why it's there.
