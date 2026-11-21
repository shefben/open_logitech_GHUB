# Rust port status

Standalone Rust + egui rebuild of the G HUB-parity peripheral manager, replacing the Artemis 2
host. Source of truth for behaviour is the Avalonia original under
`Artemis-Enhanced/src/Artemis.{Peripherals,UI,UI.Windows}` (deleted 2026-11-12; ghub-rs is now the
only implementation); source of truth for appearance was `Artemis-Enhanced/docs/ghub-reference/screenshots`
(23 captures at 1920x1080, deleted with it).

A row is **done** only when it behaves as the Avalonia build does. Nothing here is marked done on
the strength of compiling, and no feature is marked hardware-verified without a physical test.

## Why fidelity is reachable

The Avalonia UI is not a native-widget UI. It is flat panels plus data-driven custom painting over
assets copied out of a G HUB install, and every one of those inputs ports unchanged:

| Input | Where it came from | Status |
|---|---|---|
| Colour tokens | G HUB's own `app.min.css` dls tokens, via `GHubStyles.axaml` | ported to `theme.rs` |
| Metrics | measured from the 1920x1080 captures | ported to `theme.rs` |
| Fonts | `BrownPro`, `Brown Logitech Pan` (Logitech's own faces) | embedded, ~550 KB |
| Device renders | `front.png` + G903 side-plate covers | loaded from `assets/devices` |
| Icons | 50 from `app.asar` + 13 from `LGHUB/system_tray` | rasterised to `assets/icons` |
| Per-key geometry | G HUB's own `metadata.json` | parsed, tested against both devices |

## Workspace

| Crate | Ports | Status |
|---|---|---|
| `ghub-hidpp` | `Artemis.Peripherals/{Hid,Logitech}` - HID++ transport, features, sessions, onboard profiles | **done** |
| `ghub-core` | the rest of `Artemis.Peripherals` - devices, profiles, actions, macros, lighting, OpenRGB, G HUB import, automation, diagnostics | **done** |
| `ghub-platform` | `Artemis.UI.Windows/Peripherals` - raw input, foreground detection, `SendInput`, tray | **done** - plus the tray, autostart, single-instance and process hand-off the Avalonia host supplied |
| `ghub-host` | Artemis's DI container - builds the store, provider, device manager, service, hooks, automation API and OpenRGB into one `Stack` | **done** - one type, built by both binaries |
| `ghub-agent` | the resident process: `lghub_agent.exe`'s job, with no GUI toolkit linked | **done** - tray menu, hand-off, 1.6 MB |
| `ghub-ui` | `Artemis.UI/Screens/Peripherals` - the ten pages | **done** - all ten pages and the three shared editors |

### `ghub-hidpp` detail

| Module | Ports | Status |
|---|---|---|
| `report` | `HidppReport.cs` | done - framing, parsing, big-endian params |
| `error` | `HidppException.cs` | done - 1.0/2.0 classification, transient vs fatal |
| `hid` | `HidAbstractions.cs` | done - collection grouping, connection traits |
| `channel` | `HidppChannel.cs` | done - serialised request/response, retry, notifications |
| `replay` | `ReplayHidBackend.cs` | done - transcript replay, hooks, injected events, unplug |
| `descriptor` | HidSharp's `ReportDescriptor` | done - report lengths and top-level usage, which `hidapi` does not expose |
| `hidapi_backend` | `HidSharpBackend.cs` | done - shared-mode open, enumeration **verified on hardware** |
| `device` | `HidppDevice.cs` | done - ping, feature discovery, notification routing |
| `capability` | `Abstractions/Capabilities.cs` | done - what a device can do, with status and persistence |
| `features::device_info` | 0x0005 / 0x0003 | done - name, firmware, transport (read-only) |
| `features::dpi` | 0x2201 | done - step and discrete lists, validated writes |
| `features::report_rate` | 0x8060 | done - rate mask, validated writes |
| `features::battery` | 0x1000 / 0x1001 / 0x1004 | done - all three sources, voltage curve |
| `features::controls` | 0x1B04 | done - control table, diversion (volatile only) |
| `features::keys` | 0x8010 / 0x8020 / 0x8030 / 0x8040 / 0x4522 | done - G-keys, M-keys, MR, brightness, game-mode list |
| `features::mouse` | 0x8110 / 0x2121 | done - button spy mapping, hi-res wheel |
| `features::lighting` | 0x8071 / 0x8081 | done - clusters, effects, power timers, per-key frames |
| `features::onboard` | `OnboardProfile.cs` | done - sector parse, CRC, directory, mode and slot |
| `features::onboard_writer` | `OnboardProfileWriter.cs` | done - plan, snapshot, guarded commit, rollback |
| `identity` | `Abstractions/DeviceDescriptor.cs` + the notification records | done - stable keys, descriptors, health, events |
| `model` | `LogitechModels.cs` | done - verified identifier table, each row with its evidence |
| `sha256` | `System.Security.Cryptography.SHA256` | done - FIPS 180-4, 60 lines, no crypto dependency |
| `session` | `LogitechHidppSession.cs` | done - probe, capabilities, feature accessors, notification decoding |
| `provider` | `LogitechHidppProvider.cs` | done - enumeration, receiver-slot probing, shared channels |

### `ghub-core` detail

| Module | Ports | Status |
|---|---|---|
| `device_image` | `DeviceImageMetadata.cs` | done - G HUB's own geometry, parsed and asserted |
| `id` | `System.Guid` | done - 128-bit local ids, hex in the profile file |
| `action` | `Actions/ActionModel.cs` | done - every action and macro step, byte-identical JSON |
| `profile` | `Profiles/ProfileModel.cs` | done - profiles, rules, per-device settings, triggers |
| `store` | `Profiles/ProfileStore.cs` | done - atomic save, backups, quarantine, migration, validation |
| `resolver` | `Profiles/ProfileResolver.cs` | done - deterministic selection with a stated reason |
| `layout` | `Logitech/G815Layout.cs` | done - 115 keys, usages, LED ids and evidence |
| `effects` | `Lighting/KeyEffects.cs` + `KeyEffectState.cs` | done - all ten effects, deterministic seeds |
| `reactions` | `Lighting/KeyReactionEngine.cs` | done - momentary and toggle, overlay, virtual-key mapping |
| `animation` | `Lighting/KeyAnimationEngine.cs` | done - thread per device, self-pacing, reaction overlay |
| `ownership` | `Lighting/LightingOwnershipService.cs` | done - one writer per zone, per-zone throttle |
| `injection` | `Actions/InputInjection.cs` | done - tracking injector, guaranteed release |
| `macros` | `Actions/MacroRuntime.cs` | done - bounded loops, recursion guard, emergency stop |
| `openrgb` | `OpenRgb/OpenRgbClient.cs` | done - little-endian framing, version-aware controller parse, loopback policy |
| `openrgb_service` | `OpenRgb/OpenRgbService.cs` | done - connect/reconnect loop, ownership-gated writes, refusal is not retried |
| `import` | `Import/GHubImporter.cs` + `GHubSettingsReader.cs` | done - read through a copy, per-item preview, applier-as-data |
| `automation` | `Automation/AutomationServer.cs` | done - loopback only, bearer token from the OS CSPRNG |
| `automation_target` | `Automation/PeripheralAutomationTarget.cs` | done - reads, profile edits and volatile writes only |
| `diagnostics` | `Diagnostics/DiagnosticsLog.cs` | done - ring buffer, fan-out, the two protocol-logging switches |
| `manager` / `service` | `Devices/DeviceManager.cs` + `PeripheralService.cs` | done - sessions, notification pumps, profile application, bank/G-Shift/record workflows |

The channel is blocking rather than `async`. Every caller runs on a worker thread, the device
answers in single-digit milliseconds, and this keeps an async runtime out of a binary whose whole
point is being small.

`hidapi` uses its `windows-native` backend, so there is no C hidapi build and no vendored DLL.
`rusqlite` is needed only to read G HUB's own `settings.db` read-only during import.

Several things are not a straight translation of the C#. Each is a deliberate departure, not a
shortcut:

- **Report lengths come from the report descriptor.** `hidapi` does not expose
  `caps.InputReportByteLength`, which is what HidSharp gave the C# and what the channel uses to
  tell a device's short-report collection from its long-report one. `descriptor.rs` parses the raw
  descriptor instead, which `hidapi` does expose. The alternative - inferring from the Logitech
  usage convention, as Solaar and libratbag do - would have been wrong on this very machine: see
  the enumeration below, where the G815's 20-byte collection sits on page `0xFF43` usage `0x0602`,
  not on `0xFF00` usage `0x0002`. That inference survives only as a last-resort fallback for
  receivers, and a collection it cannot account for is left unused rather than guessed at.
- **Two handles per collection, one per direction.** The reader thread parks in a 100 ms
  `read_timeout` and the Windows backend leaves that overlapped read pending between calls, so a
  single shared handle would both serialise every write behind the read poll and give two users the
  same pending read. Shared-mode opens make the second handle legal; if it fails the connection
  falls back to one handle and logs that writes will wait.
- **Non-vendor collections cannot be opened at all.** `is_vendor_defined` was advisory in the C#;
  here `open` refuses outright. A Logitech keyboard exposes its boot keyboard collection on the
  same interface, and a report written there injects keystrokes into whatever has focus.
- **A session stores probe results, not live feature objects.** The C# kept
  `Dictionary<Type, IDeviceFeature>` and handed entries back from `GetFeature<T>()`. Every feature
  here borrows the device it talks to, so storing them beside the device would make the session
  self-referential. The session keeps the small owned answers each feature's constructor got, and
  the accessors rebuild the wrapper against `&self.device` with no further traffic. Same cost, no
  `unsafe`, and a feature can never outlive its device.
- **The reaction and ownership engines return work instead of doing it.** The C# queued its own
  writes onto the thread pool and raised events. Here `ReactionEngine` returns the frames to write
  and `LightingOwnership` collects its changes for the caller to drain, so no handler ever runs
  while a lock is held and a test can assert the exact LED and colour a press produces.
- **Only sleeping actions get a thread.** The C# chained every binding onto a per-trigger task
  chain. A binding that cannot sleep (key, mouse button, DPI, G-Shift) now runs inline - it is one
  `SendInput` call - and only macros, chords and launching a program get a thread. Press/release
  ordering stays exact without a scheduler.
- **`regex-lite`, not `regex`.** Title patterns need a real engine; the full `regex` crate is
  roughly ten times the binary. `regex-lite` also memoises its backtracker, so the C#'s 100 ms
  match timeout - which existed because .NET's engine backtracks without bound - is not needed.
- **Two C# behaviours were changed on purpose**, both noted where they live: a per-key animation
  seed now comes from a stable hash rather than `string.GetHashCode()`, which .NET randomises per
  process (Starlight twinkled in a different order on every launch); and re-requesting lighting
  ownership of a zone you already own no longer raises a spurious ownership-changed event.

## Measured so far

- Release exe: **8.8 MB** (9,231,872 bytes), single file, `assets/` (5.6 MB: the two device
  renders plus 207 KB of icons) beside it. `packaging/build-release.ps1` stages both into `dist/`.
- **758 tests pass**, 0 failures: 375 in `ghub-core`, 157 in `ghub-hidpp`, 58 in `ghub-platform`,
  168 in `ghub-ui`. `cargo clippy --all-targets` is clean, and so is `cargo build` now that the
  crate-wide `dead_code` allowance is gone.
- **50,866 Rust lines**, of which roughly 15,700 are tests. `crates/ghub-ui/src/pages` is 14,300.
- **63 icons**, all of them extracted from an installed G HUB: 50 from its web UI's `app.asar`,
  13 from its native system-tray application.
- No new dependency in this pass. `rusqlite` was already declared for the importer and is now
  actually used, which is most of the growth below.

### Where the last 2.9 MB went

The exe went 5.9 MB -> 7.3 MB -> 8.8 MB over the last two passes. None of it is new weight in the
sense of code being written twice; it is the binary finally *containing* things that were
previously dead-code eliminated, plus one genuine dependency cost:

| Pass | Size | What entered live code |
|---|---|---|
| backend complete, shell only | 5.9 MB | - |
| seven pages | 7.3 MB | the service, the macro runtime, the animation engine, the automation server |
| the last three pages | 8.8 MB | the OpenRGB client and supervisor, the G HUB importer, the tray |

The importer is the expensive one, and the reason is worth stating plainly: G HUB keeps its
configuration inside a SQLite database, so reading it pulls in `rusqlite` with the bundled SQLite
amalgamation. The static library is 4.9 MB and link-time optimisation keeps only what is reached,
but what is reached to run one `SELECT` is the pager, the b-tree and the VDBE - on the order of a
megabyte and a half.

The alternative is to parse the SQLite file format by hand, which is perfectly possible for one
table with one blob column - except that G HUB runs with write-ahead logging on, and the newest
document lives in a 4 MB `-wal` rather than in the database file. A hand-rolled reader would have
to replay WAL frames over the page images to see what G HUB is actually showing, and getting that
subtly wrong means importing someone's configuration from last week without saying so. The
dependency is the honest trade here, and it is recorded rather than hidden.

### The chrome now comes from G HUB itself, not from look-alikes

The Avalonia build approximated two things it had no access to, and both have been replaced with
the real article, taken from the installed G HUB's own `resources/app.asar` and `app.min.css`:

- **Icons.** `Material.Icons.Avalonia` was used to pick the nearest Material Design glyph for each
  place G HUB has an icon. G HUB ships 169 SVGs under content-hashed names, and its stylesheet maps
  each hash to the class that uses it (`.menu-icon-wrapper.lighting`,
  `.color-picker .picker .eye-dropper-button`, …). The 50 the peripheral pages need were resolved
  through that mapping and rasterised to `assets/icons`, named after the G HUB class. Rasterised
  rather than shipped as SVG because a renderer that handles even-odd fills and elliptical arcs is
  one to two megabytes of dependency to draw a few dozen fixed shapes; at 64px the whole set is
  under 200 KB. The monochrome ones are stored as 8-bit greyscale alpha masks and tinted at draw
  time, which is
  what lets one asset serve the icon rail's three states, because G HUB varies a rail icon by
  opacity rather than by swapping the image. The 13 that carry their own colour (the warning amber,
  the error red, the gold star) are RGBA and drawn untinted.
- **Colours and metrics.** The theme's values were measured off screenshots; they are now the `dls`
  design-system tokens. Several were a shade out, and two were wrong in a way a screenshot cannot
  show:

  | Token | Was | G HUB |
  |---|---|---|
  | `--dls-color-surface` (panel) | `#222326` | `#212225` |
  | `--dls-color-primary-rgb` (accent) | `#00A8E8` | `#00A8E0` |
  | `.lighting-key.selected` stroke | `#00B4F0` | `#00A9E0` |
  | `--neutral-400` (row text) | `#C8CACD` | `#A7A7A8` |
  | `.checkbox-label` (label text) | `#8A8D93` | `#AFB1B4` |
  | `.menu-icon-wrapper` size | 52px | **48px** |
  | `.menu-icon-wrapper:hover` | brighter | **dimmer** (opacity .6 → .4) |

  The last two are behavioural. A hovered rail icon in G HUB *dims*; the Avalonia port brightened
  it, which makes the item under the pointer look more selected than the actually-selected one.
  The form controls also now carry G HUB's own numbers rather than Fluent defaults: `.dls-button`
  is 48px tall in BrownPro-Bold 14 with 0.5px letter spacing, a primary button is a pill
  (`border-radius: 56px`) in the accent over **black** text, and state is a `:before` overlay at
  0.1/0.2 opacity rather than a second fill colour.

### Two defects found in this pass

1. **A finished background job could go unshown indefinitely.** The worker set a dirty flag that
   the frame loop polled. egui only draws on input or on an explicit request, so with the window
   idle the frame that would have noticed the flag never ran: a device write's result sat invisible
   until the user happened to move the pointer. The worker now calls
   `egui::Context::request_repaint()` from its own thread, which is the mechanism egui provides for
   exactly this. The flag is gone rather than fixed - it was redundant once the wake-up was real.
2. **`ConflictingProcesses` had no Rust equivalent and the gap was silent.** The C# used
   `Process.GetProcesses()`; `std` has nothing like it, so the coexistence warning would simply
   never have appeared. It is now a toolhelp snapshot in `ghub-platform::coexistence`. Worth having
   because two programs writing the same device's lighting produce a symptom - settings that revert
   a second after being applied - that looks exactly like a bug in this program.

### Verified against the installed G HUB's own settings database

The importer is the one new module whose input is a proprietary file rather than a protocol, so it
is tested against the real thing rather than only against a fixture. On this machine:

```
G HUB Some("2026.6.981214"): 34 items (25 importable, 9 not), 1 profiles
test import::tests::the_installed_g_hub_settings_can_be_read_and_previewed ... ok
```

That file is 148 KB with a **4.2 MB `-wal`**, which is exactly the case a fixture cannot
reproduce: a running G HUB keeps the newest document in the write-ahead log, so a reader that
copied only the database would have previewed a stale configuration without saying so.

The same test asserts the promise the module makes about the original file - that it is read
through a copy and never opened - by comparing its length and modification time before and after.
The test skips itself, with a note, on a machine where G HUB is not installed.

This verifies **reading**. It says nothing about applying an import, which writes a profile
document and is covered only by the unit tests.

### Hardware-verified: enumeration only

Run on this machine with a G815 attached and a Lightspeed receiver present. **This verifies
enumeration and descriptor parsing. No request has been sent to a device and no write path is
hardware-verified.**

| Device | Page | Usage | Report bytes | HID++? |
|---|---|---|---|---|
| G815 (`C33F`, `mi_01`) | `FF43` | `0602` | 20 | yes - long |
| G815 (`C33F`, `mi_01`) | `FF43` | `0604` | 64 | no - very long, not a channel collection |
| Receiver (`C539`, `mi_02`) | `FF00` | `0001` | 7 | yes - short |
| Receiver (`C539`, `mi_02`) | `FF00` | `0002` | 20 | yes - long |
| Receiver (`C539`, `mi_02`) | `FF00` | `0004` | 32 | no - this is the DJ interface |
| Receiver (`C539`, `mi_01`) | `FFBC` | `0088` | 2 | no |

Four things this settled that guesswork would have got wrong:

1. The G815's HID++ collection is on page `0xFF43`, usage `0x0602`. The usage-convention
   inference would have mis-sized it.
2. A vendor-defined collection is **not** necessarily a HID++ one. The receiver's 32-byte `FF00`
   usage `0x0004` collection is the Unifying DJ interface, and it enumerates a 2-byte vendor
   collection on `0xFFBC` as well. `HidInterfaceInfo::is_hidpp` therefore requires the report to
   be exactly 7 or 20 bytes, matching the filter in `LogitechHidppProvider.cs:74`.
3. The directly attached G815 has **no short collection** at all, which is the case
   `HasShortCollection` exists to handle.
4. The descriptor parser reads real Logitech descriptors correctly, returning 7, 20, 32, 64 and 2
   for the collections above.

## Pages

Routes are kept from `Routes.cs` so the tray menu and deep links still address a page the same way.

| Page | Route | Avalonia source | Rust | Status |
|---|---|---|---|---|
| Devices | `peripherals/devices` | `PeripheralDevicesTabViewModel` (99) | `devices.rs` (372) | **done** |
| Mouse | `peripherals/mouse` | `MouseTabViewModel` (822) | `mouse.rs` (2727) | **done** - DPI stages, assignments, zones, onboard memory |
| Keyboard | `peripherals/keyboard` | `KeyboardTabViewModel` (1265) | `keyboard.rs` (3178) | **done** - presets, freestyle, animations, assignments, game mode, press reactions, settings |
| App lighting | `peripherals/app-lighting` | `AppLightingTabViewModel` (339) | `app_lighting.rs` (975) | **done** |
| Macros | `peripherals/macros` | `MacrosTabViewModel` (314) | `macros.rs` (1231) | **done** - recorder, timeline, repeat modes, playback |
| Profiles & rules | `peripherals/profiles` | `ProfilesTabViewModel` (296) | `profiles.rs` (1167) | **done** - minus the Artemis-link dropdown, which is dropped below |
| Automation | `peripherals/automation` | `AutomationTabViewModel` (182) | `automation.rs` (764) | **done** |
| G HUB import | `peripherals/import` | `GHubImportTabViewModel` (89) | `import.rs` (472) | **done** |
| Diagnostics | `peripherals/diagnostics` | `PeripheralDiagnosticsTabViewModel` (78) | `diagnostics.rs` (589) | **done** |
| Options | `peripherals/options` | `PeripheralOptionsTabViewModel` (62) | `options.rs` (522) | **done** - plus the startup setting the Avalonia host owned |

Three files under `pages/` are not pages. They are the editors several pages share, and they are
where the C# had duplication rather than a type:

| Shared | Lines | Why it is its own file |
|---|---|---|
| `device_stage.rs` | 734 | the keyboard stage: one `Projection` that the draw pass and the hit test provably share |
| `action_editor.rs` | 892 | the 17-entry action library, used by Assignments, Macros and Automation |
| `power_save.rs` | 272 | the 0x8071 power timers, on both device-settings pages |

## Dropped with the Artemis host

These were Artemis features, not G HUB features, so leaving them out does not cost a parity row:

- Artemis layer/profile editor and keyframe timeline
- Artemis plugin and SDK game integrations
- RGB.NET's other-brand device ecosystem (`NativeLogitechDeviceProvider` existed only to stream
  Artemis-rendered layers into the G903/G815)
- The one Artemis coupling inside the G HUB UI itself: `ProfilesTabViewModel`'s "linked Artemis
  profile" dropdown

OpenRGB is **not** dropped - it lived in `Artemis.Peripherals`, so it ports with the rest.

## Order of work

1. ~~`ghub-hidpp`: transport, the real backend, the feature layer and onboard profiles.~~
   **Done**, including the session and provider layer that turns enumerated collections into a
   device list.
2. ~~`ghub-core` profile store and action/macro model - the data the pages bind to.~~ **Done**,
   and more besides: the layout, the ten software effects, press reactions, the animation engine,
   lighting ownership, input tracking and the macro runtime all landed with it.
3. ~~`ghub-platform`: `SendInput`, the low-level input hooks and foreground detection.~~
   **Done.** The Win32 surface is declared directly in `sys.rs` rather than taken from the
   `windows` crate, and `rules.rs` holds the pure input policy so the suppression invariant is
   tested on any host.
4. ~~`ghub-core`: the device manager and `PeripheralService`.~~ **Done.** The manager owns the
   sessions and pumps the notification streams the C# dropped on the floor; the service applies
   profiles, routes hook and device events, and runs the bank, G-Shift, DPI and macro-record
   workflows.
5. ~~Diagnostics, the macro recorder, the trigger engine, the OpenRGB client and the loopback
   automation API.~~ **Done.**
6. ~~The UI pages.~~ **Done.** All ten, plus the three shared editors.
7. ~~`OpenRgbService`, `PeripheralAutomationTarget` and the G HUB importer.~~ **Done.**
8. ~~Tray, autostart, single-instance, installer.~~ **Done** - see below.

### The host the Avalonia build supplied

Four things were Artemis's job rather than the peripheral code's, so they had no C# of their own
to port and are new Windows code:

| Piece | Where | What it does |
|---|---|---|
| Tray icon and menu | `ghub-platform/src/tray.rs`, `ghub-ui/src/tray_menu.rs` | the profile list, lock/unlock, auto-switch and stop-all from `PeripheralTrayMenu.cs`, plus Open and Quit |
| Single instance | `ghub-platform/src/instance.rs` | a named per-session mutex; a second launch shows the first one's window and exits |
| Start with Windows | `ghub-platform/src/autostart.rs` | one `HKCU\...\Run` value, set from the Options page |
| Installer | `packaging/` | per-user staging, install and uninstall, with no toolchain to install first |

Each carries a decision worth repeating here, because each is a place where the easy version is
worse:

- **The tray's window is a real top-level window, not `HWND_MESSAGE`.** A message-only window
  would be the obvious choice for something that never draws, but it does not receive broadcast
  messages - and a broadcast is how the second instance tells the first to show itself.
- **Closing the window hides it; only Quit exits.** A peripheral manager whose window is closed
  still has to run the hooks, the rules and the lighting. If the tray icon could not be
  registered, the close button exits as normal, because otherwise the application could not be
  closed at all.
- **The tray icon is the extracted G mark, not a drawn shape.** It is built into an `HICON` from
  the real asset through a 32-bit DIB with its own alpha, which is what keeps the glyph
  anti-aliased; `CreateIcon` with a monochrome mask - the shorter route - cannot express partial
  transparency. The window and taskbar icon is the same asset.
- **Autostart is `HKEY_CURRENT_USER`, and never a scheduled task.** A task can be created with
  "run with highest privileges", which is how applications quietly end up elevated at logon.
  Nothing here elevates and the `Run` key cannot.
- **The installer is per-user and installs no service, task or driver.** It also does *not* turn
  autostart on: that is a tick in the application's own Options page, so the choice stays with
  the user. Uninstall keeps profiles unless asked otherwise, and refuses to delete anything that
  is not under this account's own `Programs` directory.

### Where the port stands, by C# line count

| Area | C# in scope | Ported | Left |
|---|---|---|---|
| `Artemis.Peripherals` backend | 10,706 | 10,706 | 0 |
| `Artemis.UI.Windows` platform | 453 | 453 | 0 |
| UI pages and views | 6,648 | 6,648 | 0 |
| **Total** | **17,807** | **17,807** | **0** |

Every C# file in scope has a Rust counterpart. That is not the same claim as "every behaviour is
verified": see the hardware section above, which still says what it said - enumeration and
descriptor parsing are hardware-verified and **no write path is**.

One row was dropped rather than ported, and it is named in the "Dropped with the Artemis host"
section: `ProfilesTabViewModel`'s linked-Artemis-profile dropdown, which has nothing to link to
in a standalone build.

## Safety rules the write paths enforce

Carried from the implementation plan and each covered by a test, because these are the places
where a bug damages the user's device rather than just the UI:

- **No firmware flashing, ever.** `HidppDevice::call` refuses feature ids 0x00C2, 0x00C3 and
  0x00D0 by name even when the device's own feature table lists them.
- **No writing to standard HID collections.** `HidApiBackend::open` returns `PermissionDenied` for
  any collection below usage page 0xFF00. A report written to the boot keyboard collection injects
  keystrokes into whatever has focus.
- **No feature discovery by probing.** Indexes come from the FeatureSet table and nowhere else, so
  an unknown write id is never sent to find out what it does.
- **Nothing is written to an LED id we are not sure of.** Per-key lighting is enabled only when
  the caller supplies a layout, so a device whose key ids we do not know gets zone lighting
  rather than a frame addressing made-up ids. A zero id never reaches a frame: 0x8081 validates
  the whole packet, so one bad id blanks the entire keyboard.
- **A trigger cannot launch a program.** `store::validate` refuses a `TriggerRule` whose action is
  `Launch`, in either direction, because a trigger fires on a device event without the user
  touching anything.
- **A macro always releases what it pressed.** Every run has its own `TrackingInjector`, so
  stopping one run releases its own keys and leaves a key another binding is holding alone. A
  release the OS refuses stays tracked for the next emergency stop rather than being forgotten.
- **Loops are bounded and cancellable.** A `WhileHeld` macro whose release event is lost stops at
  its loop bound, and a ten-minute delay is slept in 5 ms slices so an emergency stop takes
  milliseconds rather than ten minutes.
- **Every setter validates against what the device reported**, then reads back and returns what
  the device now says. An out-of-range DPI, an unsupported report rate, a bank index past the
  reported count, a non-divertable control and an inversion a wheel does not support all fail
  without a single byte reaching the device.
- **Non-idempotent requests are never retried.** Onboard memory writes, the button-spy mapping and
  the lighting power timers all go out with `retry_on_timeout: false`: a lost reply does not mean
  the command was not applied.
- **Lighting writes are RAM-only.** Byte 12 of every 0x8071 effect payload is 0x01; 0x02 there
  would write the onboard configuration the user set up in G HUB.
- **The first three mouse buttons can never be muted.** If this process dies without restoring the
  0x8110 mapping, the user is left with a working pointer.
- **Onboard writes are gated five ways** - writes explicitly enabled, profile format 0x04 only,
  never the directory or a ROM sector, a persisted snapshot of the target sector, and a planned
  sector with a valid CRC and the exact sector length - then read, compared against the plan's
  `before` to catch another application having changed it, written, read back, and rolled back on
  any mismatch. A corrupt user sector is shown from the ROM default but never written.

## Quality work in this pass

### A second extracted asset set: G HUB's own system tray

The icon work so far came from `resources/app.asar` - G HUB's web UI. The tray is not a web UI: it
is a native WinUI application under `LGHUB/system_tray`, and it ships its own 24px glyphs beside
its executable. Thirteen of them are the icons G HUB uses for meanings this build also has, so
they are now extracted and used rather than approximated with a web-UI icon:

| Added | Used for |
|---|---|
| `status_info`, `status_warning`, `status_error` | the Diagnostics log's three severities |
| `battery_0/_25/_50/_75/_100`, `battery_charging` | the Devices page's battery row |
| `connection_usb`, `connection_lightspeed`, `connection_bluetooth` | how each device is attached |
| `report_rate` | the mouse's report-rate heading |

The `_dark` variant of each pair is the one taken: that is the white-on-transparent form, which is
what a coverage mask needs and what a dark UI wants. Every one of them is monochrome - G HUB
colours the *text* of a status row, not the glyph - so the set is tinted at draw time like the
other masks, and the full-colour count stayed at 13 of what is now 63 assets.

This replaced three substitutions that were honest but second-best: the log's severities had been
borrowing `.info-tooltip`, `.info-message-icon.pairing-caution` and `.integration-error-indicator`
from the web UI. Those are real G HUB assets, but they were authored for inline help and pairing
notices rather than for status severity. The tray set was authored for exactly this.

Battery and connection were not illustrated at all before, because the web UI has no glyph for
either - that part of G HUB's own interface is the tray.

### Staging the release caught a packaging defect

The first `build-release.ps1` run produced a 15 MB `dist/`; the second produced 21 MB from the
same inputs. `Copy-Item -Recurse` onto a directory that already exists copies the source *into*
it, so staging twice gave `dist/assets/assets` - an application that would still run (the real
`assets/` is there) carrying a second, silent copy of 5.6 MB of renders. The target directory is
now removed before the copy, which also means a renamed or deleted asset cannot survive from an
earlier stage.

Worth recording because the symptom was a number in a build log, not a failure: nothing errored,
nothing looked wrong, and the installer would have shipped it.

### The packaging scripts are parse-checked

PowerShell does not fail until the line runs, and an installer's error paths are exactly the lines
that do not run in a successful install. All three scripts are therefore checked with
`[System.Management.Automation.Language.Parser]::ParseFile`, which caught one real defect:
`uninstall.ps1`'s refusal message interpolated `"$resolved: ..."`, and `$name:` in a double-quoted
string is parsed as a drive-qualified variable reference. The script would have failed to load at
all - and only when someone ran the uninstaller.

### The crate-wide `dead_code` allowance is gone

`ghub-ui` carried `#![allow(dead_code)]` from the first pass, with a note saying to remove it once
the last page landed. It is removed, and the 17 items it had been hiding were each dealt with
rather than re-silenced:

- **Deleted**, having no caller and no test: `artwork.rs::unproject` (the device stage has its own
  `Projection`, so this was a second inverse map nothing used), `shared.rs::describe_foreground`,
  `PowerSave::store` (a delegate to the snapshot every caller already used) and
  `GameModeKeyRow::name` (a second copy of a key label that `layout::by_usage` already owns, and
  so a copy that could go stale).
- **Scoped to the tests that use them**: `Changes::any` and `ActionEditor::with_macros`.
- **Kept, with the reason in the file**: the unused `theme` tokens, four ported `widgets` styles
  and the unreferenced `Icon` variants. These are deliberately complete sets - G HUB's own token
  table, its control vocabulary, and the manifest of the 50 extracted assets - and the value of
  each is being diffable against the original rather than being minimal.

`cargo build` and `cargo clippy --all-targets` are both at zero warnings with the allowance gone.

## What is finished, and what "finished" does not mean

Every C# file in scope has a counterpart, every page is real, and the host pieces Artemis used to
supply exist. These are the things that are deliberately *not* claimed:

- **No write path is hardware-verified.** Enumeration and descriptor parsing were run against the
  attached G815 and a LIGHTSPEED receiver. Every setter is validated against what the device
  reported, read back and rolled back on mismatch - by construction and by test, not by a
  physical trial of each one.
- **The OpenRGB integration has never spoken to a real OpenRGB server.** The framing, the
  version negotiation and the controller parse are tested against captured blocks; the connect
  loop is tested against a port nothing listens on and against a host it must refuse. A live
  session is untested.
- **Importing is verified for reading only.** See the section above: the real `settings.db` is
  read and previewed, and the original is provably untouched. Applying an import writes a profile
  document, and that half is covered by unit tests.
- **The installer is a per-user PowerShell install, and nothing is code-signed.** No MSI, no
  bundle, no signature. SmartScreen will warn on first run, and that is the honest state of it.
- **The hand-off between the two processes *is* hardware-verified** - see the section below -
  against a wired G815 and a G903 LIGHTSPEED on its receiver. What is still untested is the
  awkward case rather than the normal one: a wireless mouse that was asleep at the moment of a
  release. The timeout path is written to carry on and say so rather than refuse to open.
- **The tray, autostart, single-instance and process hand-off are Windows-only.** Off Windows the
  window builds and every page renders - which is what keeps the layout workable without a Windows
  machine - but
  there is no tray icon, no startup entry and no instance guard.
- **Accessibility is measured but not implemented** - see the section below: the
  dependency costs 299 KB, and the keyboard and mouse stages are painted rather than composed of
  widgets, so a screen reader cannot describe a key until each one is named by hand.

## The window is no longer what stays resident

The application now ships as two executables. This was the last thing measured in the port, and
the measurement is the reason for it:

| | `ghub.exe` (window) | `ghub-agent.exe` (resident) |
|---|---|---|
| On disk | 9,231,872 bytes | 1,679,360 bytes |
| Working set | 390.5 MB | 6.6 MB |
| Private bytes | 467.2 MB | 1.8 MB |
| Links | egui, eframe, glow, `image`, bundled SQLite | none of them |

For comparison, G HUB's own `lghub_agent.exe` sat at 230 MB of working set on the same machine
while this was measured.

Almost none of the window's cost is code. It is a GL context, a swapchain, the vendor's OpenGL
ICD, and the two device renders - `g815_us/front.png` is 3600x1374 and `g903_hero/front.png` is
1440x2858, which is 19.8 MB and 16.5 MB of RGBA once decoded and uploaded. None of that is needed
to keep a keyboard lit, and before the split the application paid all of it for as long as the
user left it running, because closing the window only hid it.

### What moved where

`ghub-host` is a new crate holding [`Stack`]: the profile store, the HID++ provider, the device
manager, the service, the input hooks, the automation API and the OpenRGB integration. It is what
`ghub-ui/src/host.rs` used to build. Both binaries build the *same* type, which is the point of
extracting it rather than copying it - a second copy would let the agent and the window disagree
about what a document means, and that is a class of bug neither one can be read to find.

What is left in `ghub-ui/src/host.rs` is what only a window needs: the worker thread and its job
queue. `Host` dereferences to `Stack`, so every page still writes `shared.host.service` and no
page was changed by the split.

The agent is `crates/ghub-agent`: 3 files, no GUI dependency, and `ghub-core` with
`default-features = false` so bundled SQLite stays out of it. SQLite is there for the G HUB
importer, which is a page in the window; 1.5 MB of it in a process meant to sit resident all day
is 1.5 MB for nothing. The feature is turned back on by `ghub-ui`, and Cargo unifies features per
build, so the window still has the importer.

The notification-area icon moved to the agent, because an icon that only exists while the window
does is not a notification-area icon. Its menu is the same port of `PeripheralTrayMenu.cs`,
rewritten against `PeripheralService` instead of the window's `Shared`.

### The hand-over, and the two ways it can go wrong

`ghub-platform/src/handoff.rs`. Exactly one process may hold the hardware: a HID device opened
twice, two sets of low-level hooks, or two listeners on the automation port are each their own
kind of wrong.

1. The window sends `ghub-rs.takeover` to the agent's hidden window, with its own pid in `wParam`.
2. The agent closes every device session, uninstalls the hooks, stops the listener - through
   `Stack::release`, which calls the same `service.shutdown()` the window calls when it closes, so
   the keyboard is handed back to its own firmware rather than left dark between the two
   processes - and then sets the named event `Local\ghub-rs.hardware-released`.
3. The window waits on that event, then builds its stack.
4. On exit the window sends `ghub-rs.resume`.

A message *and* an event rather than one call, because a window message is synchronous for the
sender: doing a full device release inside a window procedure would block the agent's own message
pump, including its tray, for as long as the release took. The invariant is worth stating plainly,
because it is the one thing that can fail quietly: **the event is set exactly when the agent is
not holding the hardware.**

Two failure paths are handled rather than assumed away:

* **The window dies without sending a resume.** The agent also waits on the window's process
  handle - `SYNCHRONIZE` only; it has no business reading the window's memory or terminating it -
  and reports through the same message the window would have sent, so there is one path into
  `take_back` rather than two. That path is idempotent on the pid, because the normal case is two
  resumes for one window.
* **The agent does not release in time.** The window reports it to the diagnostics log and takes
  the hardware anyway. That is the lesser failure: both processes briefly having a device open
  shows up as failed transactions and a log entry, whereas refusing to continue produces an
  application that cannot be opened and gives the user no way to find out why.

### Three things the split would have broken, and what was done about them

None of them was visible from reading either binary on its own, which is why they are written
down.

* **A released process could never take the hardware back.** The first version of the hand-over
  released with `PeripheralService::shutdown`, which was the right *hardware* behaviour - it is
  the same hand-back the window has always performed on close - but it ends with
  `DeviceManager::shutdown`, and that latches a flag `scan` checks on entry. So an agent that
  released for a window and then resumed re-installed its hooks, re-read the document, reported
  "taking the devices", and held **nothing**: every later scan returned immediately. Nothing in
  the code said so, and the event-based protocol reported success throughout.

  Found by measuring rather than by reading. A fresh agent had 12 threads and 177 handles; one
  that had resumed after a window session had 5 and 171. The handle counts were close enough to
  look like noise, and the thread count was the thing that did not add up.

  `DeviceManager` now has `suspend`/`resume` beside `shutdown`: reversible, keeps the device list,
  the change listeners and the health loop, and refuses `scan`, `request_rescan` and
  `poll_health` while suspended - because touching a device the other process owns is the one
  thing the hand-over exists to prevent. `PeripheralService::shutdown` was split into
  `hand_back_hardware` plus the terminal call, with `suspend` taking the same hand-back and
  stopping short of it. Release uses `suspend`; process exit still uses `shutdown`. Four tests
  pin it, including that a `resume` cannot revive a manager that was really shut down.

* **A stale document.** The window owns the document while it is open, so when the agent takes the
  hardware back its own in-memory copy is whatever was on disk before the window started. An agent
  resuming on that copy would apply the profile the user had just renamed, and would overwrite the
  rename on its next save. `Stack::resume` now re-reads the document first, through a new
  `PeripheralService::reload_from_store` - deliberately *not* `replace_document`, which saves,
  validates, rotates a backup and pushes an undo entry. A reload must do none of those: a save
  would rotate a backup every time the window closed, and an undo entry would let the user "undo"
  a reload.
* **A tray menu writing to a document it no longer owns.** While the window is open the agent's
  menu collapses to Open and Quit. Its profile list would otherwise be built from that same stale
  copy, and a profile switch chosen from it would be overwritten by the window's next save. The
  window is in front of the user at that moment, and it is where every one of those settings
  lives.

### Smaller consequences, all of them deliberate

* **Autostart registers the agent, not the window.** Registering the window would put a window on
  screen at every logon *and* spend 390 MB from logon onwards to do a job the 1.6 MB half does.
  Falling back to the running image when the agent is not beside it keeps the tick honest on a
  half-installed copy.
* **The agent exits if it cannot create its icon.** Without the tray it has no window to receive a
  take-over request and no menu to quit from, so it would sit holding the hardware with no way to
  reach it. Giving up leaves the window able to run on its own, which is what it did before the
  split.
* **The agent ships the G mark twice.** `assets/icons/g-logo.png` for the window, and
  `assets/tray/g-logo.rgba` - the same 64x64 pixels, already decoded - for the agent, which links
  no PNG decoder. Two copies of one asset can drift, so a test in `ghub-ui`, which has the
  decoder, asserts the blob is byte-for-byte the decoded PNG. A re-extracted G mark fails the suite
  rather than quietly leaving the notification area on the old artwork.
* **Quit is reachable from the window too.** "Quit G HUB completely" on the Options page sends the
  same message the tray's Quit does, so there is one shutdown order rather than two, and a user
  whose notification area has hidden the icon can still exit.
* **Closing the window cancels nothing now.** The `CancelClose` plus `Visible(false)` pair is gone.

### Verified against the hardware

Run from `dist/`, against the attached G815 and a G903 LIGHTSPEED on its receiver. The agent's own
loopback automation API was the instrument - it is a shipped surface, and it answers from the
*agent's* stack rather than from a log that could be stale. It was switched on in the document for
the measurement and switched back off afterwards.

| Step | Observed |
|---|---|
| Agent started alone | 2 devices connected: `G815 RGB Mechanical Gaming Keyboard` firmware `U1 31.02.0018` wired, `G903 LIGHTSPEED HERO` firmware `MPM 23.01.0013` on `LightspeedReceiver`, battery 80%. 9.2 MB working set, 12 threads |
| Window started | Agent released: 12 threads to 9, `Local\ghub-rs.hardware-released` set. Window up at 58-81 MB working set, 90-115 MB private |
| Window closed (`WM_CLOSE`) | Window process exited - it does not hide any more. Agent resumed: released event clear, port 9696 back under the **agent's** pid, both devices reporting `connected=true` and the mouse's battery live at 80% |
| `ghub-rs.quit` sent to the tray window | `SendMessageTimeoutW` answered 1 and the agent exited |

Two things about measuring this are worth writing down, because both produced a wrong answer
first.

**Thread and handle counts cannot tell you whether the devices are back.** A resumed agent settles
at 8-9 threads and ~174 handles against a fresh one's 9-12 and ~177, and that gap stays whether or
not the devices were re-opened - it is the first scan's transient thread and one session's worth of
handles. It was enough to *notice* the defect above and nowhere near enough to confirm the fix. The
API's device list is what confirmed it.

**With the API enabled, the window starts a listener on the same port.** A reading taken while the
window is open comes from the *window's* stack, which does hold two devices - so the first attempt
at this table reported success for a release that had happened correctly, while proving nothing
about it. Check which pid owns the port before believing the answer. That the window can bind 9696
at all is incidentally a check on the ordering: the agent stops its listener inside `release`,
before the window builds the stack that starts its own.

### What the split did not change

The ten pages, the HID++ layer, the profile format, the safety rules and every one of the write
paths are untouched. `Host` dereferencing to `Stack` is what made that possible: the diff for the
pages is one line in `automation.rs`, where a moved function is now called by its new path.

One measured consequence of the fix is worth keeping: the first take-over of a process's life
calls `DeviceManager::start`, and every later one calls `resume`. `start` also spawns the health
loop, and calling it twice would leave two loops polling the same devices.

The costs are honest ones. Opening the window is slower than un-hiding it was, by one device
enumeration - the agent's release plus the window's scan - and on a machine with a Unifying
receiver that is seconds, not milliseconds. Animated lighting restarts its phase across a
hand-over, because the new owner re-applies the profile from the document rather than inheriting a
running animation. And there is a sub-second window during a hand-over when no process holds the
input hooks, so a G-key pressed at exactly that moment does nothing.

## The accessibility question, measured

The open question from every previous pass was what `accesskit` costs. It is measured rather than
estimated now, by building the release twice with nothing else changed:

| Build | Size |
|---|---|
| `eframe` features `glow, wayland, x11` (shipped) | 9,231,872 bytes |
| the same plus `accesskit` | 9,537,536 bytes |
| **difference** | **305,664 bytes (299 KB, 3.3%)** |

The earlier guess was "roughly 300-600 KB"; it is at the bottom of that range, and 299 KB on an
8.8 MB binary is not the reason to leave accessibility out.

What it is *not* is the whole job, and this is the part worth being clear about. `accesskit` gives
the platform an accessibility tree for the widgets egui itself knows about - buttons, text fields,
checkboxes, combo boxes - which is most of the Options, Diagnostics, Macros, Profiles and import
pages. It does **nothing automatically** for the two screens that matter most here: the keyboard
stage and the mouse stage are *painted*, not composed of widgets. A key is a rounded rectangle and
a galley, so no tree can describe it until each one is given a name, a role and a selected state
by hand.

So the honest state of it is: the dependency is cheap and should go in, and the assignment flow
the Avalonia plan called for - selecting a key from the keyboard and binding it without a mouse -
is work on the stage itself rather than a feature flag. Neither is done, and the flag is still off
so the measurement above stays reproducible.


## G HUB's built-in presets

All taken from G HUB 39.1.2's own data. The source files are copied into `data/ghub-source/`
(`fetch_from_ghub.py` refreshes them from an install), and `data/ghub-source/gen_presets.py`
turns them into what ships, so neither building nor running needs G HUB.

| G HUB list | Here | Shipped as |
|---|---|---|
| Assignments > SYSTEM (`card_presets.json`, 60 Windows cards) | Action editor > "Choose from G HUB presets" > SYSTEM. 32 map to actions; the rest (webcam, headset audio, battery LEDs, firmware brightness, report-rate cycle, latching DPI shift, double click) are listed with the reason they are unavailable | `ghub-core/data/ghub_commands.json`, embedded |
| Assignments > COMMANDS, Desktop (44 shortcuts) | DESKTOP tab; all 44 work. Win+L becomes `LockWorkStation`, because Windows ignores a synthesised Win+L | embedded |
| Assignments > COMMANDS, games (816 games, 24,891 commands) | GAMES tab; 24,610 work, the 281 that press two plain keys at once are shown as unavailable | `assets/presets/games.json`, 1.2 MB, read by the window only, on first use |
| LIGHTSYNC > ANIMATIONS (Contrastic, Lightning, Ocean Wave, Red White and Blue, Verticool) | Played from G HUB's keyframes: per-frame length x speed, its CSS easing curves, CYCLE / BOUNCE / RANDOM order | `ghub-core/data/ghub_keyframes.json`, 40 KB, embedded |
| LIGHTSYNC > PRESETS: Echo Press | New software effect | - |
| LIGHTSYNC > PRESETS: Screen Sampler | G HUB's five screen strips and its per-key map; GDI read-back at 15 Hz on its own thread (about 12 ms per read) | `ghub_screen_sampler.json` |
| LIGHTSYNC > PRESETS: Audio Visualizer | G HUB's per-key column layout and 60-6000 Hz bands; WASAPI loopback and a 2048-point FFT | `ghub_audio_visualizer.json` |
| Assignments > "Cycle Maximum Brightness" | New `LightingCommand::BrightnessCycle`: 100/75/50/25/0 % | - |

Screen and audio capture only exist while one of those two effects is playing; both close
themselves three seconds after the last frame asked for them.

Not hardware-verified yet: none of the new effects or cards has been run on the G815 or G903.
They use only write paths that already are (per-key 0x8081 frames, brightness 0x8040, key and
mouse injection), and the renderers are unit-tested against G HUB's data.
