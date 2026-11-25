# revelator-control

A standalone, Windows-first control app for the PreSonus/Fender **Revelator Dynamic** (USB VID `194F`, PID `0D0A` / `0D04`). It talks to the mic's vendor-specific control interface directly, without Universal Control.

Crates: `revelator-core` (protocol, safety layer, mock device), `revelator-usb` (nusb transport, discovery, one-time WinUSB setup), `revelator-cli`, `revelator-gui`.

## Build

```
cargo build --release --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

CI (GitHub Actions, `.github/workflows/ci.yml`) runs the same fmt / clippy / test / build steps on every push and pull request, natively on x64 and ARM64 Windows, then builds the installer (`installer/revelator-control.iss`, Inno Setup 6). To publish a release, bump `version` in the root `Cargo.toml`, commit, then push a matching tag (`git tag v0.2.0 && git push origin v0.2.0`). The GitHub release gets the installer, a portable zip per architecture and SHA-256 sums. Tags with a `-` suffix (`v0.2.0-beta.1`) become pre-releases.

## Install

Download `revelator-control-<version>-setup.exe` from the [releases](https://github.com/Arno500/revelator-dynamic-app/releases). It is one installer for x64 and ARM64 PCs, and it installs the build that matches the machine. It can install for the current user only (no admin rights, `%LOCALAPPDATA%\Programs`) or for all users (`Program Files`). It adds a Start menu entry and can put `revelator-cli` on the PATH for the one-time setup below (off by default: tick the option on the "Additional tasks" page). Uninstalling removes the app, its PATH entry and its "Start on boot" entry, but keeps your settings and presets in `%APPDATA%\revelator-control`. The portable zips (`windows-x64`, `windows-arm64`) hold the same two programs without an installer. The app checks GitHub for a newer release on start and then daily, and shows a Windows notification plus a banner when there is one (it only opens the release page, nothing is installed). Untick "Check for updates" in the status bar to turn this off; the choice is kept in `%APPDATA%\revelator-control\update.json`.

CLI: `revelator-cli --help` (`--mock` runs against an in-memory device, no hardware needed). Useful commands: `diagnose`, `probe`, `dump-state`, `get`, `set`, `watch`, `setup`.

## One-time setup (Windows)

The PreSonus driver (PaeDSPUSB) owns the audio (MI_00) and MIDI (MI_03) interfaces. The vendor control interface MI_05 ("Revelator Dynamic CTRL") is already bound to Microsoft's inbox WinUSB, but its device node has no `DeviceInterfaceGUIDs` registry value, so no device interface exists and apps cannot open it (error: "Could not find DeviceInterfaceGUIDs in registry"). The setup step adds that one value.

From an **elevated** terminal (Run as administrator):

```
revelator-cli setup          # dry run: prints exactly what would change
revelator-cli setup --yes    # apply
```

What it changes, and nothing else:

- Registry: `HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_194F&PID_0D0A&MI_05\<instance>\Device Parameters`, value `DeviceInterfaceGUIDs` (REG_MULTI_SZ) = `{D1A8F3C2-47B6-4E19-9C5D-2F6E8A0B73E4}` (the project GUID).
- Restarts that single device node (`pnputil /restart-device`, fallback: SetupAPI property change on that node).

It does **not** touch the PreSonus audio/MIDI driver, Universal Control, the DFU interface (MI_06), the parent device, or any installed driver. It refuses to run unless: elevated; the node is MI_05 of VID 194F with PID 0D0A/0D04; the node is bound to the WINUSB service; and no `DeviceInterfaceGUID(s)` value exists that is not ours.

Undo (removes only that value, restarts that node):

```
revelator-cli setup --undo --yes
```

## Exclusive access

WinUSB allows one opener at a time. If Universal Control (or the "PreSonus Hardware Access Service") has the control interface open, this app gets "busy / access denied". Close Universal Control and stop that service, then retry; and do not run both at once.

## Safety rules

- The app never enters DFU/firmware-update mode, never resets the device and never sends firmware commands. Only Revelator Dynamic in normal mode (0D0A/0D04) is accepted; the "unsupported mode" PIDs and other models are refused.
- Writes go through the core safety layer; Delay stays blocked until the sample rate is known.
  The GUI also switches Echo off when Windows moves to 88.2/96 kHz (it can only react after the switch).
- Only addresses taken from Universal Control's DLL are written: device parameters
  (`SetP Appl 0`, channel 0, the parameter's own Para/Pari type, value inside its range),
  DSP blobs with their exact tag and size, and the preset read (`GetP` MemP, slots 0-3).
  The older io24-based guesses (blocks 100/101/201/202 with Para/Pari) are refused.
- DSP blobs are value-checked before sending: no NaN/Inf, and every filter section must be stable.
- Stream mixer (Main / Mix A / Mix B) writes are crosspoint gains on block 100, as UC sends them
  (`revelator-cli mixer`, GUI Mixer tab); only known buses/sources and gains in [-145, +20] dB pass.
  The mic's reverb send is the one crosspoint UC sends to block 0 (`SetP 0 0 [Para, 0x14, 0, 1, dB]`);
  nothing else is accepted on block 0.
- The setup step is explicit (`--yes` required), reversible, and limited to the one node described above.

## Status: partially verified against hardware

Verified on a real Revelator Dynamic (PID 0D0A, Windows 11):

- USB descriptors and driver binding via `revelator-cli diagnose` (PaeDSPUSB on MI_00/MI_03, WINUSB on MI_05).
- After `setup --yes`, WinUSB exposes MI_05 and nusb opens it: interface 5 alt 1, bulk OUT 0x01 / IN 0x81 (alt 0 has no endpoints).
- Protocol handshake via `revelator-cli probe`: protocol version 1, max command / response length 2048.
- Bulk framing via `revelator-cli trace`: the mic answers `GetP Appl JaSt`. Replies use header byte 3 = `0x81` (commands use `0x01`). Blob size 0x7EC gives a 2048-byte reply (exactly 4 × 512-byte packets, no zero-length packet). Blob size 0xDC gives a 240-byte reply.
- State layout, from `raw-state` / `watch-raw` while using the physical controls. Both blob sizes return the same data; non-zero values only appear in slots 0..52, slots 53+ read 0.

  | Slot | Content |
  |---|---|
  | 6-11, 14-19, 26 | level meters (change constantly) |
  | 20-25 | ~1.0, probably gain-reduction meters |
  | 42 | **active preset**, integer 0..3 |
  | 44 | **flag word**, idle 0x81; **bit 3 = mute**; bit 2 = headphone mute (confirmed by `readback-test`); mute-button and preset-button modes are not in the state |
  | 45 | **knob position**, f32 0..1 in 0.01 steps (what it controls: not confirmed) |
  | 27-37, 40, 41, 43, 46, 48, 49 | unknown (1.0 x11, 3, 4, -1, 0.263, 29.4, 40.0) |

  `dump-state` and `watch` decode the bold rows.

- Writes: `set` sends `SetP Appl 0 [Para|Pari, 0x14, y, x, value]`, with addresses for preamp gain, mic mute, preset, headphone/main volume, headphone mute and monitor blend read from Universal Control's DLL (`docs/uc-dll-notes.md`). Confirmed on the mic for mic.mute, preset and preamp.gain (the mic acknowledges `SetP` with a bare 8-byte frame, which the app reads; `set` confirms through the state). Other parameters are not writable until their addresses are known.

Not yet verified: the rest of the parameter table (its slots are io24-based guesses and read 0 on the Dynamic; EQ, dynamics, Voice FX and reverb settings are probably not in this reply), every parameter write, and `setup --undo` on hardware. Unit tests cover those with fake errors and injected node info only.

## DSP settings (`dsp` command)

HPF, gate, compressor (standard / FET / tube), EQ (parametric / passive / vintage), limiter, reverb, the EQ/compressor order switch and every Voice FX model (doubler, detune, vocoder, ring modulator, robot, echo) are written the way Universal Control does it, ported from `dspusbdevice.dll`. UC computes everything on the PC: filter coefficients, linear gains and time constants. It then sends one blob per DSP block: `SetP <block> <processing channel> [tag, size, engine id 0, fields...]`. There are no `Para` messages.

The mic cannot report these values back, so the CLI keeps them in a state file (`%APPDATA%\revelator-control\dsp-state.txt`) and always sends a whole block.

```
revelator-cli dsp list                                  # every key, current value, range
revelator-cli dsp set limiter.on 1 limiter.threshold -20 --dry   # print frames only
revelator-cli dsp set limiter.on 1 limiter.threshold -20         # send + save
revelator-cli dsp push                                  # resend everything
revelator-cli presets dump                              # read the 4 preset slots (read-only)
revelator-cli presets dump 0 --save .                   # one slot, also saved as preset-0.bin
revelator-cli mixer list                                # mixer settings + crosspoint gains
revelator-cli mixer set playback.volume -6 blend -0.5   # send + save
revelator-cli set mute.button 0                         # mute button mutes the headphones
revelator-cli mixer set mic.reverb -10 reverb.volume 0  # mic -> reverb send, reverb return on Main
revelator-cli readback-test                             # locate the button-mode / headphone-mute readback
```

Echo (`voicefx.model 5`) is refused unless `--rate 44100` or `--rate 48000` is given (Delay at 88.2/96 kHz crashes the firmware). `eq.model` (0 parametric, 1 passive, 2 vintage) only changes which `eq.*` keys are used: like UC, the model choice itself is never sent. The same goes for `comp.model`. `eq.swap` sends `SetP 'opt ' <ch> [Pari, 0x14, 0, 0, 0|1]`.

Reverb level (UC parameter FXA) is the mic's send into the reverb, a mixer crosspoint: `mixer set mic.reverb <dB>`, or "Reverb level" on the GUI's Reverb card. Like UC it is combined with the mic's mute, volume and the monitor blend, and sent as `SetP 0 0 [Para, 0x14, 0, 1, dB]`. The reverb return is the `reverb.*` mixer channel (GUI: the Reverb strip). UC's default scene has both at -96 dB, so raise the send and the return to hear it.

Not verified on hardware: the block addressing, processing channel 0, and the default sample rate of 48 kHz.

The GUI's Fat Channel and Voice FX & Reverb tabs use the same engines and the same state file. Edits made while dragging a control are merged, so each engine block is sent once per burst.

The sample rate comes from Windows' Core Audio API: the shared-mode mix format of the Revelator's active audio endpoints (`IAudioClient::GetMixFormat`). The GUI subscribes to endpoint notifications (`IMMNotificationClient`), so a format change in Sound settings, an unplug or a replug is picked up straight away. The CLI queries the rate once when `--rate` is not given. If no endpoint is found, the rate stays unknown and the echo effect stays blocked. In the GUI you can then pick the rate in the status bar.
