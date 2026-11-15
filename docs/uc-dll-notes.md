# Notes from Universal Control's `hwaccess/dspusbdevice.dll`

Read-only inspection of the DLL shipped with Universal Control (strings and data tables only, no
disassembly yet). Offsets are file offsets in that DLL.

## Revelator Dynamic hardware state variables (@0x399150, 0x54-byte records)

Record: `u32 type` (0 bool, 1 int, 3 float), `u32 index`, `char name[32]`, `f32 min, max, ?, default, step`, ...

| idx | type | name | range |
|---|---|---|---|
| 0 | int | input1SlotIndex | 0..3 |
| 1 | int | input2SlotIndex | 0..3 |
| 2 | int | presetMode | 0..2 |
| 3 | float | input1FxMix | 0..1 |
| 4 | float | input2FxMix | 0..1 |
| 15 | bool | input1HighPassFilter | |
| 16 | bool | input2HighPassFilter | |
| 5 | bool | muteMode | |
| 6 | bool | input1Mute | |
| 7 | bool | input2Mute | |
| 8 | bool | hpOutputMute | |
| 9 | bool | mainOutputMute | |
| 10 | float | hpVolume | 0..1 |
| 11 | float | mainVolume | 0..1 |
| 12 | float | monitorMix | -1..1 |
| 13 | float | input1Gain | 0..60 |
| 14 | float | input2Gain | 0..60 |
| 17 | bool | usbStatus | |
| 18 | bool | channelLink | |
| 19 | int | input1ProcessingChannel | 0..1 |
| 20 | int | input2ProcessingChannel | 0..1 |
| 23 | int | outputDelayBus | -1..4 |
| 24 | float | outputDelay | 0..0.5 |

### Match against the JaSt reply captured on hardware (hypothesis unless marked)

| slot | captured | candidate | status |
|---|---|---|---|
| 42 | 0..3, preset button | input1SlotIndex | confirmed by preset button |
| 43 | -1 | input2SlotIndex (no 2nd input on Dynamic) | fits |
| 44 | 0x81 / 0x89 | bools packed in table order: bit0 input1HighPassFilter, bit1 input2HighPassFilter, bit2 muteMode, **bit3 input1Mute**, bit4 input2Mute, bit5 hpOutputMute, bit6 mainOutputMute, bit7 usbStatus, bit8 channelLink | bit3 confirmed (mute button and knob press); others fit 0x81 (HPF on, USB up) |
| 45 | 0.90..1.00, knob | hpVolume (floats idx 10..14 in order) | knob confirmed; headphone level to confirm |
| 46 | 0.263 | mainVolume | fits |
| 47 | 0 | monitorMix (centre) | fits |
| 48 | 29.4 | input1Gain (dB) | fits range 0..60 |
| 49 | 40.0 | input2Gain | fits range |

## How UC writes parameters (from the disassembly)

- Frame builder @0x180055a60: payload = `SetP|GetP`, 8-byte address `(block u32, index u32)`, blob.
- Float sender @0x180054920 / int sender @0x1800548a0: blob `[Para|Pari, 0x14, y, x, value]`,
  address and `x`/`y` from a 16-byte binding `{block, index, x, y}`.
- Generic setter @0x180057ed0: `Para` for float parameters (record type 3), `Pari` otherwise;
  read-only parameters (record flag 0x20, e.g. meters) are never sent. The value goes through a
  per-binding conversion first: identity for most, `(v - min) / (max - min)` through the curve
  for phonesVolume / mainOutVolume (so the device gets a 0..1 position), `v - 1` for
  outputDelayBus (outgoing slot +0x30 = 0x180036ef0 subtracts 1.0; the `v + 1` at 0x180036ee0
  is the incoming direction), so UC's default 0 goes out as -1 = off.
- Voice FX model @0x18004a518: `SetP 201 0 [VoFx, 0x10, 0, model]` (matches the io24 notes).

### Device-level bindings (@0x180036f0f), all `SetP Appl 0`, y = 0

| param (id) | x |
|---|---|
| phonesVolume (7) | 1 |
| mainOutVolume (30) | 2 |
| phonesMute (1) | 6 |
| 20 (name unknown) | 8 |
| monitorBlend (2) | 10 |
| phonesSrc (0) | 11 |
| outputDelayBus (6) | 13 |
| outputDelay (5) | 14 |
| presetButtonMode (8) | 17 |
| micPattern (25) | block 200 (0xC8), x/y from a register |

### Channel-level bindings (@0x1800414xx and @0x18004438f), `SetP Appl 0`, y = channel

| param (id) | x |
|---|---|
| preampgain (142) | 3 |
| 600 (name unknown) | 4 |
| hardwareMute (105) | 7 |
| link (109) | 9 |
| processingChannel (149) | 12 |
| autogain (140) | 15 |
| activePresetSlotIndex (150) | 16 |
| autogainmode (141) | 19 |
| 48v (160) | x from a register |
| pan (143), dawpostdsp (146) | block from a register, x = 2 / 3 |

Not yet found: the block numbers of the DSP processing objects (gate, compressor, EQ, limiter,
Voice FX parameters, reverb). Their bindings take the block from a runtime object, so the io24
numeric blocks (100/101/202) in `params.rs` remain unconfirmed and are not used for writes.

## Hazards seen in the DLL (never send)

- `SetP Appl 0 [FRst, 8]` (built @0x180036099): almost certainly a factory reset. Not on the
  safety allow-list.

## DSP parameters: what is known so far

- Definitions (names, ids, ranges, defaults, curves) are in the tables below.
- They are not in the JaSt state reply (slots 53+ are always 0 on the Dynamic).
- In UC, a DSP object (EQ, compressor, Voice FX, ...) forwards each change to the currently
  selected model through a list parameter at object+0xe0 (`eqmodel` id 0, `fxmodel` id 450),
  calling `model->set(param_id, value)` (@0x18004c7d0). No constant `SetP` address for these
  was found; the path to the wire goes through further virtual calls.
- A chunked memory transfer exists (`MemP`, blob size 0x7EC, up to 0x7CE data bytes per chunk
  with offset / total / more-follows fields, @0x1800528c0), and preset memory is addressed as
  `(Appl, PrsM, n)`. Whether DSP edits travel this way is not confirmed.

## DSP parameter tables (0x68-byte records)

Record: `u32 type` (0 bool, 1 int, 2 enum, 3 float, 4 string), `u32 id`, `char name[32]`,
`f32 min, max, ?, default, step`, then pointers (unit / curve / label strings).
IDs are local to their processing object; the SetP **block numbers are not in these tables**.

```
== table @0xedd40  10 records
  t2 id    0 0x0000 phonesSrc                0 2 0 0 1 u2 ['Phones Source']
  t0 id    1 0x0001 phonesMute               0 1 0 0 1 u0 ['Headphones Muted']
  t3 id    2 0x0002 monitorBlend             -1 1 0 0 0.002 u1 ['Monitor Blend']
  t0 id    3 0x0003 aux1_mirror_main         0 1 0 0 1 u2 ['Mirror Main to Mix 1']
  t0 id    4 0x0004 aux2_mirror_main         0 1 0 0 1 u2 ['Mirror Main to Mix 2']
  t3 id    5 0x0005 outputDelay              0 0.5 0.25 0 0.002 u2 ['time', 'Output Delay']
  t2 id    6 0x0006 outputDelayBus           0 4 0 0 1 u18 ['Output Delay Bus']
  t3 id    7 0x0007 phonesVolume             -96 0 -10 -30 0.002 u0 ['gain', 'exp', 'Phones Volume']
  t1 id    8 0x0008 presetButtonMode         0 2 0 2 1 u18 ['Preset Mode']
  t0 id    9 0x0009 auxMuteMode              0 1 0 0 1 u2 ['Aux Mute Mode', 'preset_name']
== table @0xee160  7 records
  t0 id  120 0x0078 solo                     0 1 0 0 1 u10 ['Solo']
  t0 id  121 0x0079 lr                       0 1 0 1 1 u10 ['Monitor Assign']
  t0 id  122 0x007a assign_aux1              0 1 0 1 1 u10 ['Assign Mix A']
  t0 id  123 0x007b assign_aux2              0 1 0 1 1 u10 ['Assign Mix B']
  t3 id  124 0x007c aux1                     -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Mix A Level']
  t3 id  125 0x007d aux2                     -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Mix B Level']
  t3 id    0 0x0000 hpf                      24 1000 0 24 0.002 u74 ['freq.0/off', 'log', 'Hi Pass Filter', 'HI PASS Filter', 'VoiceFxOn']
== table @0xee440  3 records
  t2 id    0 0x0000 eqmodel                  0 0 0 0 1 u24 ['EQ Model']
  t2 id    1 0x0001 compmodel                0 0 0 0 1 u24 ['Compressor Model']
  t0 id    2 0x0002 swapcompeq               0 1 0 0 1 u10 ['Swap EQ/Comp Order']
== table @0xee580  5 records
  t2 id   80 0x0050 preset                   0 0 0 8 1 u16 ['Preset Titles']
  t0 id   81 0x0051 matchesPresetFile        0 1 0 0 1 u32 ['Matches Preset File']
  t0 id    0 0x0000 limiteron                0 1 0 0 1 u74 ['Limiter On', 'LIMITER Power']
  t3 id    1 0x0001 threshold                -28 0 0 0 0.002 u74 ['gain', 'linear', 'Threshold', 'LIMITER Thresh']
  t3 id    2 0x0002 reduction                -21 0 0 0 0.002 u32 ['gain', 'linear', 'Reduction']
== table @0xee790  5 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 size                     0 1 0 0.15 0.002 u2 ['percent', 'Size']
  t3 id    2 0x0002 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
  t3 id    3 0x0003 hp_freq                  0 500 250 200 0.002 u2 ['freq', 'HPF (high pass filter)']
  t3 id    4 0x0004 predelay                 0.0001 0.25 0.125 0.02 0.002 u2 ['time/off', 'Predelay']
== table @0xeea10  13 records
  t4 id  100 0x0064 chnum                    0 0 0 0 0 u0 ['Number']
  t4 id  101 0x0065 name                     0 0 0 0 0 u0 ['Name']
  t4 id  102 0x0066 username                 0 0 0 0 0 u2 ['User Name']
  t0 id  103 0x0067 select                   0 1 0 0 1 u8 ['Select']
  t0 id  104 0x0068 mute                     0 1 0 0 1 u10 ['Mute']
  t0 id  105 0x0069 hardwareMute             0 1 0 0 1 u0 ['Hardware Mute Button']
  t3 id  106 0x006a volume                   -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Volume']
  t3 id  107 0x006b monitorBlend             0 1 0 0.5 0.002 u32 ['Monitor Blend Factor']
  t0 id  108 0x006c clip                     0 1 0 0 1 u40 ['Clip']
  t0 id  109 0x006d link                     0 1 0 0 1 u10 ['Link']
  t0 id  110 0x006e linkmaster               0 1 0 0 1 u2 ['link master']
  t4 id  111 0x006f preset_name              0 0 0 0 0 u2 ['Preset Name']
  t0 id  300 0x012c mono                     0 1 0 0 1 u2 ['Mono']
== table @0xeef60  2 records
  t3 id   30 0x001e mainOutVolume            -96 0 -10 -30 0.002 u0 ['gain', 'exp', 'Main Output Volume']
  t0 id   31 0x001f enableChannelAssign      0 1 0 0 1 u0 ['Enable Processing Channel Assignment', 'icon_id']
== table @0xef120  5 records
  t4 id   90 0x005a cacheTitle1              0 0 0 0 0 u0 ['Preset Title 1']
  t4 id   91 0x005b cacheTitle2              0 0 0 0 0 u0 ['Preset Title 2']
  t0 id   92 0x005c cacheModified1           0 1 0 0 1 u32 ['Cache 1 modified']
  t0 id   93 0x005d cacheModified2           0 1 0 0 1 u32 ['Cache 2 modified']
  t1 id   94 0x005e activeCache              0 1 0 0 1 u32 ['Active Preset Cache']
== table @0xef330  11 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u74 ['Gate On', 'GATE Power']
  t0 id    1 0x0001 keylisten                0 1 0 0 1 u66 ['Key Listen', 'GATE KeyLn']
  t0 id    2 0x0002 expander                 0 1 0 0 1 u74 ['onoff', 'Expander', 'GATE Expnd']
  t3 id    3 0x0003 keyfilter                40 16000 0 20 0.002 u74 ['freq/off', 'log', 'Key Filter', 'GATE Key Fltr']
  t3 id    4 0x0004 threshold                -84 0 0 -84 0.002 u74 ['gain', 'linear', 'Threshold', 'GATE Thresh']
  t3 id    5 0x0005 range                    -84 0 -21 -84 0.002 u74 ['gain', 'gaterange', 'Range', 'GATE Range']
  t3 id    6 0x0006 attack                   2e-05 0.5 0.005 0.005 0.002 u74 ['time', 'exp', 'Attack', 'GATE Attack']
  t3 id    7 0x0007 release                  0.05 2 0.7 0.7 0.002 u74 ['time', 'skew', 'Release', 'GATE Release']
  t3 id    8 0x0008 ratio                    0 0.1 0 0 0.002 u32 ['Ratio']
  t3 id    9 0x0009 reduction                -84 0 0 0 0.002 u32 ['gain', 'linear', 'Reduction']
  t0 id  160 0x00a0 48v                      0 1 0 0 1 u2 ['48V', '_ Empty Location _']
== table @0xef820  17 records
  t0 id  140 0x008c autogain                 0 1 0 0 1 u8 ['Automatic Preamp Gain']
  t1 id  141 0x008d autogainmode             0 1 0 0 1 u8 ['Automatic Preamp Gain Mode']
  t3 id  142 0x008e preampgain               0 60 0 0 0.002 u18 ['gain.0', 'linear', 'Preamp Gain']
  t3 id  143 0x008f pan                      0 1 0 0.5 0.002 u3 ['pan.100', 'Pan']
  t3 id  144 0x0090 stereopan                0 1 0 1 0.002 u10 ['percent', 'Stereo Width']
  t3 id  145 0x0091 FXA                      -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Reverb Level']
  t2 id  146 0x0092 dawpostdsp               0 1 0 1 1 u10 ['DAW Send Post DSP']
  t0 id  147 0x0093 bypassDSP                0 1 0 0 1 u0 ['Bypass']
  t3 id  148 0x0094 dspAmount                1e-05 1 0 1 0.002 u10 ['percent', 'DSP Amount']
  t1 id  149 0x0095 processingChannel        -1 1 0 -1 1 u2 ['Assigned Processing Channel']
  t1 id  150 0x0096 activePresetSlotIndex    0 3 0 0 1 u16 ['Active Preset Slot']
  t0 id  151 0x0097 presetHotKey             0 1 0 0 1 u0 ['Hot Key']
  t4 id  152 0x0098 presetHotKeyTitle        0 0 0 0 0 u32 ['Hot Key Title']
  t4 id  153 0x0099 presetSlotTitle1         0 0 0 0 0 u32 ['Preset Slot 1']
  t4 id  154 0x009a presetSlotTitle2         0 0 0 0 0 u32 ['Preset Slot 2']
  t4 id  155 0x009b presetSlotTitle3         0 0 0 0 0 u32 ['Preset Slot 3']
  t4 id  156 0x009c presetSlotTitle4         0 0 0 0 0 u32 ['Preset Slot 4', 'voicefx']
== table @0xefff0  13 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 lows                     0 1 0 0.5 0.002 u2 ['percent', 'Lows']
  t3 id    2 0x0002 width                    0 1 0 0.5 0.002 u2 ['percent', 'Width']
  t3 id    3 0x0003 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
  t0 id    0 0x0000 eqallon                  0 1 0 0 1 u74 ['EQ On']
  t3 id    1 0x0001 bboost                   0 10 5 0 0.002 u74 ['linear', 'Low Boost', 'LOW Boost']
  t3 id    2 0x0002 batten                   0 10 5 0 0.002 u74 ['linear', 'Low Atten', 'LOW Atten']
  t2 id    3 0x0003 bfreq                    0 3 0 0 1 u74 ['Low Frequency', 'LOW Freq']
  t3 id    4 0x0004 mboost                   0 10 5 1.5 0.002 u74 ['linear', 'High Boost', 'HIGH Boost']
  t3 id    5 0x0005 bbwidth                  0 10 5 10 0.002 u74 ['linear', 'High Bandwidth', 'HIGH Bndwdth']
  t2 id    6 0x0006 mfreq                    0 6 0 4 1 u74 ['High Freq', 'HIGH Freq']
  t3 id    7 0x0007 hatten                   0 10 5 1.5 0.002 u74 ['linear', 'High Atten', 'HIGH Atten']
  t2 id    8 0x0008 hsfreq                   0 2 0 2 1 u74 ['Attenuation Select', 'ATTN Select']
== table @0xf05c0  17 records
  t2 id    0 0x0000 phonesSrc                0 2 0 0 1 u2 ['Phones Source']
  t0 id    1 0x0001 phonesMute               0 1 0 0 1 u0 ['Headphones Muted']
  t3 id    2 0x0002 monitorBlend             -1 1 0 0 0.002 u1 ['Monitor Blend']
  t0 id    3 0x0003 aux1_mirror_main         0 1 0 0 1 u2 ['Mirror Main to Mix 1']
  t0 id    4 0x0004 aux2_mirror_main         0 1 0 0 1 u2 ['Mirror Main to Mix 2']
  t3 id    5 0x0005 outputDelay              0 0.5 0.25 0 0.002 u2 ['time', 'Output Delay']
  t2 id    6 0x0006 outputDelayBus           0 4 0 0 1 u18 ['Output Delay Bus']
  t3 id    7 0x0007 phonesVolume             -96 0 -10 -30 0.002 u0 ['gain', 'exp', 'Phones Volume']
  t1 id    8 0x0008 presetButtonMode         0 2 0 2 1 u18 ['Preset Mode']
  t0 id    9 0x0009 auxMuteMode              0 1 0 0 1 u2 ['Aux Mute Mode']
  t0 id  120 0x0078 solo                     0 1 0 0 1 u10 ['Solo']
  t0 id  121 0x0079 lr                       0 1 0 1 1 u10 ['Monitor Assign']
  t0 id  122 0x007a assign_aux1              0 1 0 1 1 u10 ['Assign Mix A']
  t0 id  123 0x007b assign_aux2              0 1 0 1 1 u10 ['Assign Mix B']
  t3 id  124 0x007c aux1                     -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Mix A Level']
  t3 id  125 0x007d aux2                     -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Mix B Level']
  t3 id    0 0x0000 hpf                      24 1000 0 24 0.002 u74 ['freq.0/off', 'log', 'Hi Pass Filter', 'HI PASS Filter', 'vendor=PreSonus']
== table @0xf0cb0  3 records
  t2 id    0 0x0000 eqmodel                  0 0 0 0 1 u24 ['EQ Model']
  t2 id    1 0x0001 compmodel                0 0 0 0 1 u24 ['Compressor Model']
  t0 id    2 0x0002 swapcompeq               0 1 0 0 1 u10 ['Swap EQ/Comp Order', 'Win32 Thread']
== table @0xf0df0  21 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 time                     0.0001 0.25 0.125 0.125 0.002 u2 ['time', 'Time']
  t3 id    2 0x0002 feedback                 0 1 0 0.5 0.002 u2 ['percent', 'Feedback']
  t3 id    3 0x0003 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
  t0 id    0 0x0000 eqallon                  0 1 0 0 1 u74 ['EQ On']
  t3 id    1 0x0001 lowgain                  -16 16 0 0 0.002 u75 ['gain', 'linear', 'Low Gain', 'LOW Gain']
  t2 id    2 0x0002 lowfreq                  0 3 0 1 1 u74 ['Low Frequency', 'LOW Freq']
  t3 id    3 0x0003 lowmidgain               -16 16 0 0 0.002 u75 ['gain', 'linear', 'Low-Mid Gain', 'L-MID Gain']
  t2 id    4 0x0004 lowmidfreq               0 2 0 2 1 u74 ['Low-Mid Freq', 'L-MID Freq']
  t3 id    5 0x0005 himidgain                -16 16 0 0 0.002 u75 ['gain', 'linear', 'Hi-Mid Gain', 'H-MID Gain']
  t2 id    6 0x0006 himidfreq                0 2 0 2 1 u74 ['Hi-Mid Freq', 'H-MID Freq']
  t3 id    7 0x0007 higain                   -16 16 0 0 0.002 u75 ['gain', 'linear', 'High Gain', 'HIGH Gain']
  t0 id    0 0x0000 on                       0 1 0 0 1 u74 ['Compressor On']
  t3 id    1 0x0001 input                    -56 0 0 -43 0.002 u74 ['gain', 'linear', 'Input', 'FET Input']
  t3 id    2 0x0002 output                   -56 0 0 0 0.002 u74 ['gain', 'linear', 'Output', 'FET Output']
  t3 id    3 0x0003 attack                   2.1e-05 0.0008 0 0.0001 0.002 u74 ['time', 'linrev', 'Attack', 'FET Attack']
  t3 id    4 0x0004 release                  0.05 1.1 0 0.25 0.002 u74 ['time', 'linrev', 'Release', 'FET Release']
  t2 id    5 0x0005 ratio                    0 4 0 0 1 u74 ['Ratio', 'FET Ratio']
  t3 id    6 0x0006 reduction                -21 0 0 0 0.002 u32 ['linear', 'Reduction', 'FET Reductn']
  t3 id    7 0x0007 keyfilter                40 16000 0 20 0.002 u74 ['freq/off', 'log', 'Key Filter', 'FET Key Fltr']
  t0 id    8 0x0008 keylisten                0 1 0 0 1 u2 ['Key Listen', 'FET KeyLn', 'PID:']
== table @0xf1680  7 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u74 ['Compressor On']
  t2 id    1 0x0001 mode                     0 1 0 0 1 u74 ['Mode', 'TUBE Lim/Cmp']
  t3 id    2 0x0002 gain                     0 100 0 40 0.002 u74 ['linear', 'Gain', 'TUBE Gain']
  t3 id    3 0x0003 peak                     0 100 0 0 0.002 u74 ['linear', 'Peak Reduction', 'Peak Reductn']
  t3 id    4 0x0004 reduction                -21 0 0 0 0.002 u32 ['linear', 'Reduction', 'TUBE Reductn']
  t3 id    5 0x0005 keyfilter                40 16000 0 0 0.002 u74 ['freq/off', 'log', 'Key Filter', 'TUBE Key Fltr']
  t0 id    6 0x0006 keylisten                0 1 0 0 1 u2 ['Key Listen', 'TUBE KeyLn']
== table @0xf1960  7 records
  t2 id   80 0x0050 preset                   0 0 0 8 1 u16 ['Preset Titles']
  t0 id   81 0x0051 matchesPresetFile        0 1 0 0 1 u32 ['Matches Preset File']
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 size                     0 1 0 0.15 0.002 u2 ['percent', 'Size']
  t3 id    2 0x0002 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
  t3 id    3 0x0003 hp_freq                  0 500 250 200 0.002 u2 ['freq', 'HPF (high pass filter)']
  t3 id    4 0x0004 predelay                 0.0001 0.25 0.125 0.02 0.002 u2 ['time/off', 'Predelay']
== table @0xf1c40  3 records
  t0 id    0 0x0000 limiteron                0 1 0 0 1 u74 ['Limiter On', 'LIMITER Power']
  t3 id    1 0x0001 threshold                -28 0 0 0 0.002 u74 ['gain', 'linear', 'Threshold', 'LIMITER Thresh']
  t3 id    2 0x0002 reduction                -21 0 0 0 0.002 u32 ['gain', 'linear', 'Reduction']
== table @0xf1df0  7 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 bcarrierfreq             0.1 2000 30 30 0.002 u2 ['freq', 'skew', 'Frequency']
  t0 id    2 0x0002 bcarrier2                0 1 0 0 1 u2 ['Sub Carrier']
  t3 id    3 0x0003 bcarrier2freq            0.1 2000 30 50 0.002 u2 ['freq', 'skew', 'Sub Carrier Frequency']
  t3 id    4 0x0004 bdist                    0 1 0 0.5 0.002 u2 ['percent', 'Distortion']
  t3 id    5 0x0005 bvol                     0 1 0 1 0.002 u2 ['percent', 'Volume']
  t3 id    6 0x0006 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry', 'Win32 Thread']
== table @0xf2140  15 records
  t4 id  100 0x0064 chnum                    0 0 0 0 0 u0 ['Number']
  t4 id  101 0x0065 name                     0 0 0 0 0 u0 ['Name']
  t4 id  102 0x0066 username                 0 0 0 0 0 u2 ['User Name']
  t0 id  103 0x0067 select                   0 1 0 0 1 u8 ['Select']
  t0 id  104 0x0068 mute                     0 1 0 0 1 u10 ['Mute']
  t0 id  105 0x0069 hardwareMute             0 1 0 0 1 u0 ['Hardware Mute Button']
  t3 id  106 0x006a volume                   -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Volume']
  t3 id  107 0x006b monitorBlend             0 1 0 0.5 0.002 u32 ['Monitor Blend Factor']
  t0 id  108 0x006c clip                     0 1 0 0 1 u40 ['Clip']
  t0 id  109 0x006d link                     0 1 0 0 1 u10 ['Link']
  t0 id  110 0x006e linkmaster               0 1 0 0 1 u2 ['link master']
  t4 id  111 0x006f preset_name              0 0 0 0 0 u2 ['Preset Name']
  t3 id   30 0x001e mainOutVolume            -96 0 -10 -30 0.002 u0 ['gain', 'exp', 'Main Output Volume']
  t0 id   31 0x001f enableChannelAssign      0 1 0 0 1 u0 ['Enable Processing Channel Assignment']
  t2 id   25 0x0019 micPattern               0 2 0 1 1 u2 ['Mic', '\\']
== table @0xf27d0  15 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u74 ['Gate On', 'GATE Power']
  t0 id    1 0x0001 keylisten                0 1 0 0 1 u66 ['Key Listen', 'GATE KeyLn']
  t0 id    2 0x0002 expander                 0 1 0 0 1 u74 ['onoff', 'Expander', 'GATE Expnd']
  t3 id    3 0x0003 keyfilter                40 16000 0 20 0.002 u74 ['freq/off', 'log', 'Key Filter', 'GATE Key Fltr']
  t3 id    4 0x0004 threshold                -84 0 0 -84 0.002 u74 ['gain', 'linear', 'Threshold', 'GATE Thresh']
  t3 id    5 0x0005 range                    -84 0 -21 -84 0.002 u74 ['gain', 'gaterange', 'Range', 'GATE Range']
  t3 id    6 0x0006 attack                   2e-05 0.5 0.005 0.005 0.002 u74 ['time', 'exp', 'Attack', 'GATE Attack']
  t3 id    7 0x0007 release                  0.05 2 0.7 0.7 0.002 u74 ['time', 'skew', 'Release', 'GATE Release']
  t3 id    8 0x0008 ratio                    0 0.1 0 0 0.002 u32 ['Ratio']
  t3 id    9 0x0009 reduction                -84 0 0 0 0.002 u32 ['gain', 'linear', 'Reduction']
  t4 id   90 0x005a cacheTitle1              0 0 0 0 0 u0 ['Preset Title 1']
  t4 id   91 0x005b cacheTitle2              0 0 0 0 0 u0 ['Preset Title 2']
  t0 id   92 0x005c cacheModified1           0 1 0 0 1 u32 ['Cache 1 modified']
  t0 id   93 0x005d cacheModified2           0 1 0 0 1 u32 ['Cache 2 modified']
  t1 id   94 0x005e activeCache              0 1 0 0 1 u32 ['Active Preset Cache']
== table @0xf2df0  3 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t2 id    1 0x0001 detune                   0 8 0 4 1 u2 ['Detune']
  t3 id    2 0x0002 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
== table @0xf2f30  25 records
  t0 id    0 0x0000 eqallon                  0 1 0 0 1 u74 ['EQ On']
  t2 id    1 0x0001 eqtype1                  0 3 0 1 1 u32 ['Low Type', 'LOW Type']
  t3 id    2 0x0002 eqgain1                  -15 15 0 0 0.002 u75 ['gain', 'linear', 'Low Gain', 'LOW Gain']
  t3 id    3 0x0003 eqq1                     0.1 10 0.6 0.6 0.002 u74 ['skew', 'Low Q', 'LOW Q']
  t3 id    4 0x0004 eqfreq1                  36 18000 0 130 0.002 u74 ['freq', 'log', 'Low Freq', 'LOW Freq']
  t0 id    5 0x0005 eqbandon1                0 1 0 1 1 u74 ['Low On', 'LOW On']
  t0 id    6 0x0006 eqbandop1                0 1 0 0 1 u74 ['Low Shelf', 'LOW Shelf']
  t2 id    7 0x0007 eqtype2                  0 3 0 3 1 u32 ['Low-Mid Type', 'L-MID Type']
  t3 id    8 0x0008 eqgain2                  -15 15 0 0 0.002 u75 ['gain', 'linear', 'Low-Mid Gain', 'L-MID Gain']
  t3 id    9 0x0009 eqq2                     0.1 10 0.6 0.6 0.002 u74 ['skew', 'Low-Mid Q', 'L-MID Q']
  t3 id   10 0x000a eqfreq2                  36 18000 0 320 0.002 u74 ['freq', 'log', 'Low-Mid Freq', 'L-MID Freq']
  t0 id   11 0x000b eqbandon2                0 1 0 1 1 u74 ['Low-Mid On', 'L-MID On']
  t0 id   12 0x000c eqbandop2                0 1 0 0 1 u0 ['Low-Mid Opt', 'L-MID Opt']
  t2 id   13 0x000d eqtype3                  0 3 0 3 1 u32 ['Hi-Mid Type', 'H-MID Type']
  t3 id   14 0x000e eqgain3                  -15 15 0 0 0.002 u75 ['gain', 'linear', 'Hi-Mid Gain', 'H-MID Gain']
  t3 id   15 0x000f eqq3                     0.1 10 0.6 0.6 0.002 u74 ['skew', 'Hi-Mid Q', 'H-MID Q']
  t3 id   16 0x0010 eqfreq3                  36 18000 0 1400 0.002 u74 ['freq', 'log', 'Hi-Mid Freq', 'H-MID Freq']
  t0 id   17 0x0011 eqbandon3                0 1 0 1 1 u74 ['Hi-Mid On', 'H-MID On']
  t0 id   18 0x0012 eqbandop3                0 1 0 0 1 u0 ['Hi-Mid Opt', 'H-MID Opt']
  t2 id   19 0x0013 eqtype4                  0 3 0 2 1 u32 ['High Type', 'HIGH Type']
  t3 id   20 0x0014 eqgain4                  -15 15 0 0 0.002 u75 ['gain', 'linear', 'High Gain', 'HIGH Gain']
  t3 id   21 0x0015 eqq4                     0.1 10 0.6 0.6 0.002 u74 ['skew', 'High Q', 'HIGH Q']
  t3 id   22 0x0016 eqfreq4                  36 18000 0 5000 0.002 u74 ['freq', 'log', 'High Freq', 'HIGH Freq']
  t0 id   23 0x0017 eqbandon4                0 1 0 1 1 u74 ['High On', 'HIGH On']
  t0 id   24 0x0018 eqbandop4                0 1 0 0 1 u74 ['High Shelf', 'HIGH Shelf']
== table @0xf39d0  7 records
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 ctune                    0 1 0 0.5 0.002 u2 ['percent', 'Pitch']
  t3 id    2 0x0002 cfb                      0 1 0 0.5 0.002 u2 ['percent', 'Regeneration']
  t3 id    3 0x0003 cdamp                    0 1 0 0.5 0.002 u2 ['percent', 'Damping']
  t3 id    4 0x0004 cdist                    0 1 0 0.5 0.002 u2 ['percent', 'Distortion']
  t3 id    5 0x0005 cvol                     0 1 0 1 0.002 u2 ['percent', 'Volume']
  t3 id    6 0x0006 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry']
== table @0xf3cb0  17 records
  t0 id  140 0x008c autogain                 0 1 0 0 1 u8 ['Automatic Preamp Gain']
  t1 id  141 0x008d autogainmode             0 1 0 0 1 u8 ['Automatic Preamp Gain Mode']
  t3 id  142 0x008e preampgain               0 60 0 0 0.002 u18 ['gain.0', 'linear', 'Preamp Gain']
  t3 id  143 0x008f pan                      0 1 0 0.5 0.002 u3 ['pan.100', 'Pan']
  t3 id  144 0x0090 stereopan                0 1 0 1 0.002 u10 ['percent', 'Stereo Width']
  t3 id  145 0x0091 FXA                      -96 10 0 -96 0.002 u10 ['gain', 'fader', 'Reverb Level']
  t2 id  146 0x0092 dawpostdsp               0 1 0 1 1 u10 ['DAW Send Post DSP']
  t0 id  147 0x0093 bypassDSP                0 1 0 0 1 u0 ['Bypass']
  t3 id  148 0x0094 dspAmount                1e-05 1 0 1 0.002 u10 ['percent', 'DSP Amount']
  t1 id  149 0x0095 processingChannel        -1 1 0 -1 1 u2 ['Assigned Processing Channel']
  t1 id  150 0x0096 activePresetSlotIndex    0 3 0 0 1 u16 ['Active Preset Slot']
  t0 id  151 0x0097 presetHotKey             0 1 0 0 1 u0 ['Hot Key']
  t4 id  152 0x0098 presetHotKeyTitle        0 0 0 0 0 u32 ['Hot Key Title']
  t4 id  153 0x0099 presetSlotTitle1         0 0 0 0 0 u32 ['Preset Slot 1']
  t4 id  154 0x009a presetSlotTitle2         0 0 0 0 0 u32 ['Preset Slot 2']
  t4 id  155 0x009b presetSlotTitle3         0 0 0 0 0 u32 ['Preset Slot 3']
  t4 id  156 0x009c presetSlotTitle4         0 0 0 0 0 u32 ['Preset Slot 4']
== table @0xf4410  18 records
  t3 id   10 0x000a keyfilter                40 16000 0 20 0.002 u74 ['freq/off', 'log', 'Key Filter', 'COMP Key Fltr']
  t0 id   11 0x000b keylisten                0 1 0 0 1 u2 ['Key Listen', 'COMP KeyLn']
  t0 id    0 0x0000 on                       0 1 0 0 1 u74 ['Compressor On', 'COMP Power']
  t0 id    1 0x0001 softknee                 0 1 0 0 1 u74 ['Soft Knee', 'COMP Soft']
  t0 id    2 0x0002 automode                 0 1 0 0 1 u74 ['Auto', 'COMP Auto']
  t3 id    3 0x0003 threshold                -56 0 0 0 0.002 u74 ['gain', 'linear', 'Threshold', 'COMP Thresh']
  t3 id    4 0x0004 ratio                    1 20 2 2 0.002 u74 ['ratio/~/limit', 'ratio', 'Ratio', 'COMP Ratio']
  t3 id    5 0x0005 attack                   0.0002 0.15 0.02 0.02 0.002 u74 ['time', 'skew', 'Attack', 'COMP Attack']
  t3 id    6 0x0006 release                  0.0025 0.9 0.15 0.15 0.002 u74 ['time', 'skew', 'Release', 'COMP Release']
  t3 id    7 0x0007 gain                     0 28 0 0 0.002 u74 ['gain', 'linear', 'Gain', 'COMP Gain']
  t3 id    8 0x0008 reduction                -21 0 0 0 0.002 u32 ['gain', 'linear', 'Reduction']
  t3 id    9 0x0009 kneewidth                0 3 0 0 0.002 u32 ['gain', 'Knee Width']
  t0 id    0 0x0000 on                       0 1 0 0 1 u2 ['On']
  t3 id    1 0x0001 avol                     0 1 0 1 0.002 u2 ['percent', 'Volume']
  t2 id    2 0x0002 acarriertype             0 2 0 1 1 u2 ['Carrier Type']
  t3 id    3 0x0003 acarrierfreq             50 500 100 80 0.002 u2 ['freq', 'skew', 'Carrier Frequency']
  t3 id    4 0x0004 avoiced                  0 1 0 0 0.002 u32 ['Voiced']
  t3 id    5 0x0005 mix                      0 1 0 0.5 0.002 u2 ['percent', 'WetDry', 'Standard']
```

## DSP engine writes (dspusbdevice.dll)

- Generic form: `SetP <block> <proc channel> [tag u32, size u32, engine id u32 (=0, from Targ), fields...]`, with no reply. Blocks: FourCC `eq  ` / `comp` / `gate` / `lim ` / `filt`; 201 = Voice FX; 202 = reverb.
- Engine vtables: slot 5 = setParam(id, value), slot 20 = setup, slot 21 = send.
- Filter designer 0x180012f50 (double) / 0x1800113a0 (float): outputs `[b0, -a1, b1, -a2, b2]`, normalized.
- `MBdf` (0x18000cc70): size 0x1F4 = `[id, band]` plus 20 x 24-byte entries `{coefs, rate}` for 44.1/48/88.2/96 kHz, count at 0x1F0. `Bqdf`: 0x24 bytes = `[id, band, coefs]`.
- Field layouts, formulas and engine addresses: see the comments in `crates/revelator-core/src/fx.rs`.
  - gate 0x48, cpxt 0x40, lim 0x18, vrvb 0x38.
  - vech 0x1C, vdog 0x20, VoFx 0x10 (VoFx always goes to block index 0).
- `Lfdf` 0x2C = `[id, 0, 7 coefs]` (3rd order: `b0,-a1,b1,-a2,b2,-a3,b3`). `MLff` 0x294 = `[id, band 0]` plus 20 x 32-byte entries `{7 coefs, rate}`, count at 0x290.
- Passive EQ (vtable 43c400: set 0x18001ab10, send 0x18001ace0).
  - Sends, in order: Bqdf/MBdf bands 2 and 3 as identity, then the high boost and attenuation as Lfdf/MLff, then the low boost and attenuation as Bqdf/MBdf band 1.
  - Analog designers: 0x18001b3a0 (high) and the low one just after it, discretised by the bilinear transform at 0x18001c000.
- Vintage EQ (43c508: set 0x18001cd10, send 0x18001ced0).
  - Sends: low shelf as Lfdf/MLff (0x18001e200), high as band 1, low-mid as band 2, high-mid as band 3.
  - The designers were lifted from the asm into `fx_analog.rs`. A test checks them against the DLL run under an x86 emulator.
- EQ/compressor model selection (`opt ` handler 0x1800493f0) only swaps engine objects on the PC; nothing is sent. `swapcompeq` sends `SetP 'opt ' <ch> [Pari, 0x14, y 0, x 0, value]`.
- Voice FX (block 201, after `VoFx`):
  - 1 detune: Bqdf/MBdf band 0 LPF 6 kHz, then `may4` 0x24 `[id, on, step(-8..0), 0, 0, 0, mix]`.
  - 2 vocoder: MBdf bands 0-9 HPF and 10-19 LPF (Q 2), band 20 LPF 600 Hz, band 21 HPF 2.5 kHz; then `inia` 0x1C4 with the same 22 filters at fs; then `bota` 0x28 `[id, on, carrier type, carrier Hz, volume, 1, 1, mix]`.
  - 3 ring modulator: `botb` 0x28 `[id, on, freq, dist, volume, sub on, sub freq, mix]`.
  - 4 robot: `botc` 0x28 `[id, on, int(pitch*1300+250.5), regen*0.35+0.5, damping*0.6+0.3, dist, volume, mix]`.
- Reverb level FXA (id 145): ported as the mixer's mic → reverb send (see "Stream mixer").

## Device preset slots (dspusbdevice.dll)

- Slot contents are a UC preset document stored on the mic, **most likely binary UBJSON, not JSON text** (see "Preset document format" below). Serializer 0x180062e10
  (JSON writer, ends with `}`), parsed back by 0x18003cd50 / 0x180063030 into 0x120-byte records
  (name at +2, buffer object at +0xE8).
- Transfer object (vtables 0x180441308 / 0x180441310, embedded at +0x118 / +0xD8 / +0x38):
  - write 0x1800528c0: `SetP Appl 0 [MemP, 0x7EC, PrsM, n, u16 0, u32 offset, u8 more, u16 len,
    u32 total, data <= 0x7CE]`, chunked.
  - read request 0x180052a40: the same blob with `GetP` (op chosen at 0x180055ac3: 5th arg 1 = GetP,
    0 = SetP); replies are reassembled at 0x180052f90. At connect UC reads every slot `(PrsM, n)` (0x1800345c0).
- Callers of the write: store 0x180039524 (`StoredPreset`), delete 0x18003af47 (`DeletedPreset`),
  rename 0x18003b600 (`RenamedPreset`), generic upload 0x180037fe0. They serve the app-level
  `Stor`/`ZStr`, `Retr`/`ZRtr`, `List`, `Dele`, `Rena` requests (dispatcher 0x1800290c0); these
  never go to USB.
- Store takes a `userpresets` tree and maps entry names to slots (0x18003d310). Writable slots are
  `[0x108]*[0x10c] .. [0x160]` (user slots after factory ones).
- Related params: `activePresetSlotIndex` (150), `presetSlotTitle1..4` (153-156), `presetHotKey`,
  `activeCache` / `cacheTitle1..2` (PresetCacheComponent).
- Preset class GUIDs: Fat Channel {5E91DC8A-E560-4115-98FA-59FB3F215BA1},
  Voice FX {9F3AF9D1-3820-46CF-A675-2F382D6D334E}.
- Frame builder 0x180055a60 copies `blob size` bytes, so UC always sends the whole 0x7EC blob:
  frames are 0x800 bytes, exactly the device's max command length.
- Read request (0x180052bd3): `GetP Appl 0 [MemP, 0x7EC, PrsM, n, u16 id, u32 offset = 0, u8 0,
  u16 0, u32 0, zeros]`. `id` is a per-session transfer counter (object +0x30, incremented).
- Reply chunk (blob offsets): +8/+0xC address, +0x10 u16 id, +0x12 u32 offset, +0x16 u8 more,
  +0x17 u16 len, +0x19 u32 total, +0x1D data. When `more == 1` UC asks again with the same
  address and id and `offset = offset + len` (0x180052f10). UC's receive buffer starts at 0x2000.
- Implemented read-only as `Device::read_preset` / `revelator-cli presets dump`; the allow-list
  only admits `GetP Appl 0 [MemP, 0x7EC, PrsM, 0..=3]` with the full blob. Unverified on hardware.
  Writing: `Device::write_preset` sends the chunked `SetP Appl 0 [MemP ...]` (`frame::memp_write`, allow-listed for slots 0..=3, document starts with `{`, at most 4096 bytes) and reads the slot back to compare. The GUI builds the document from the slot's own contents (`presetdoc`), keeping every key it does not map, and backs the old slot up first. Factory slot documents (read from the mic) are UBJSON with the layout described in `presetdoc.rs`. Unverified on hardware.

## Stream mixer (dspusbdevice.dll)

The device mixer is a gain matrix. UC keeps the channel settings (from the scene: `line`,
`return`, `fxreturn`, `aux`, `main` channels) and sends one dB gain per crosspoint.

- Bus addresses (0x180044630): `main` = block 100 index 0; `aux` chN = block 100 index N
  (Stream Mix A = 1, Stream Mix B = 2). Bus `mono` (id 300): `Pari` x = 8, y = 0.
- Source numbers (channel +0x128, 0x18003e510): `return` ch1..3 (Playback, Virtual A, Virtual B)
  = 0, 1, 2; `line` ch1/2/3 = 3/4/6 (the Dynamic has ch1 = 3); `fxreturn` = 5.
- Crosspoint object (vtables 0x1804415e0/0x1804415f0, built in 0x180031ea0 for every bus and
  every `line`/`return`/`fxreturn` source): address `{100, bus, x = source, y = 0}`, sent with
  the float sender: `SetP 100 <bus> [Para, 0x14, 0, source, dB]`.
- Inputs (0x180031ea0): bus `mute` (+0x60) and `volume` (+0x58, added in dB) for every bus.
  Main bus: source `mute`, `solo`, mixer solo-any, `volume`, and `monitorBlend` factor (107; for
  line and return sources). Mix A/B: source `assign_auxN` (122/123) and `auxN` (124/125) only.
- Gain (0x1800576a0 / 0x180057840): silent (unassigned, muted, soloed out, factor 0) = -145.
  Else `lin = fader < -144 ? 6.3096e-8 : 10^(fader/20)`, `lin *= factor`,
  `dB = lin < 6.3096e-8 ? -144 : 20 log10(lin)`, `+ bus volume`, floor -144 (log10f/powf).
- Monitor blend b (0x18003784d): b < 0 → returns factor 1 + b, line 1; b >= 0 → returns 1,
  line 1 - b. So -1 = mic only, +1 = playback only.
- Mic → reverb send (`FXA`, 145, 0x180032540): crosspoint object with the same vtables, address
  `{block 0, index 0, x = 1, y = line index (0)}` written as literals (`[obj+0x10] = 0`,
  `[obj+0x18] = 1`, `[obj+0x1c] = line`), sent at once through 0x180054920. The queue
  (0x180054d00 → 0x180055280) only coalesces pending writes with the same tag and address; the
  block is never rewritten. So: `SetP 0 0 [Para, 0x14, 0, 1, dB]`. Gain inputs: mic `mute`
  (104, +0x60), `volume` (106, +0x58, added in dB), `FXA` (145, +0x50, the fader) and, when the
  channel's virtual +0x90 returns 0, `monitorBlend` (107, +0x68).

## Button modes

- `muteButtonMode` (id 20, record @0xedcd0, enum labels `Phones`, `Mic`, default 1):
  `SetP Appl 0 [Pari, 0x14, 0, 8, v]`.
- `presetButtonMode` (id 8, 0..2, default 2): `SetP Appl 0 [Pari, 0x14, 0, 17, v]`. UC's UI
  strings (language packs, `EasySetingsItem.PresetButton`): Single / Double / Multi.

## Firmware state variables (flag word)

UC's DLL embeds the mic firmware images (0x1803473b0, 0x76bf0 bytes; 0x1803bdfa0, 0x76c08 bytes,
uploaded by 0x18004d378/0x18004d558). Inside one is the firmware's state-variable table (file
offset 0x399150, records of 0x54 bytes: type, id, name[32], min, max, ...):

| id | name | type | | id | name | type |
|---|---|---|---|---|---|---|
| 0 | input1SlotIndex | int | | 9 | mainOutputMute | bool |
| 1 | input2SlotIndex | int | | 10 | hpVolume | float |
| 2 | presetMode | int | | 11 | mainVolume | float |
| 3/4 | input1/2FxMix | float | | 12 | monitorMix | float |
| 15/16 | input1/2HighPassFilter | bool | | 13/14 | input1/2Gain | float |
| 5 | muteMode | bool | | 17 | usbStatus | bool |
| 6/7 | input1/2Mute | bool | | 18 | channelLink | bool |
| 8 | hpOutputMute | bool | | 19/20, 23, 24 | processing channels, outputDelayBus, outputDelay | |

The floats land at JaSt slot id + 35 (hpVolume 45, mainVolume 46, monitorMix 47, input1Gain 48:
all confirmed). The booleans, in table order, fit the flag word (slot 44): bit 0 hpf1, 1 hpf2,
2 muteMode, 3 input1Mute (confirmed), 4 input2Mute, 5 hpOutputMute, 6 mainOutputMute,
7 usbStatus, 8 channelLink; idle 0x81 = HPF + USB connected. The app reads headphone mute from
bit 5. Mute-button mode is not read: bit 2 was 0 while the physical button muted the mic, but
UC's value for "Mic" is 1, so the polarity is unclear. presetMode (an int) has no known slot.
`revelator-cli readback-test` writes these settings and prints which bits/slots move.

## Preset document format (what UC stores in a slot)

- The document writer 0x180062e10 has two modes selected by `[ctx+8]`. Mode 1 initialises with the
  bytes `i` (0x69) and `{` (0x7b) (0x18006a340) and is **UBJSON**; the other mode is a streaming
  text JSON writer with a pretty flag (the `.scene` files). The store path (`StoredPreset`,
  0x180039524) builds its writer context with `[ctx+8] = 1`, so slots are written as UBJSON
  and then parsed back into the 0x120-byte slot records (0x18003cd50).
- Real samples: the StudioLive `.channel` files under `Fender\Offline Editor\...` are zlib streams
  of the same format. Decoded: `{ i<len>"filter" { "hpf" d<f32 BE> } "gate" { "on" i1, "keylisten", "expander",
  "keyfilter" d, "threshold" d, "range" d, "attack" d (seconds), "release" d } "comp" { "__classid" S "{GUID}",
  "on", "softknee", "automode", "threshold", "ratio", "attack", "release", "gain" } "limit" { "limiteron",
  "threshold" } "eq" { "__classid" S "{GUID}", "eqallon", "eqgain1..4", "eqq1..4", "eqfreq1..4",
  "eqbandon1..4", "eqbandop1,4" } }`. Keys are `i`+length+name (no `S`), values `i` int8, `d` float32 BE, `S` string.
- Key vocabulary near the preset strings in the DLL (0x43c1b8..): `voicefxopt voicefx insertfx fxmodel
  VoiceFxOn preset_name icon_id filter gate comp eq limit opt eqmodel compmodel line aux fxreturn
  return main`, and for the store/recall requests `presetFile presetName presetType presetIconId
  presetOptions presetTarget`, `presets/load.channel`, `presets/load.scene`.
- Decoder: `revelator_core::ubjson`; `revelator-cli presets dump` prints a slot as a UBJSON tree
  when it parses. **Still needed before writing a slot:** a real slot read from a Revelator
  (field names/units for the Revelator's own `opt`, `eqmodel`, `compmodel`, `voicefx` groups, and
  whether the mic firmware or only UC interprets the slot). Run `revelator-cli presets dump 0 1 2 3
  --save .` with the mic connected.

## Preset memory map (from the mic firmware's own handlers)

Found by disassembling (ARM Thumb) the firmware image embedded in the DLL: the MemP write
handler (file offset 0x36c53c) and read handler (0x36c70c) accept only these areas:

| area | read | write | what |
|---|---|---|---|
| `PrsM 0..=7` | yes | **no** | the 8 factory presets (UBJSON docs, also stored in the firmware image) |
| `PrsM 8..=15` | yes | yes | the 8 user presets |
| `Stat 0..=3` | yes | yes | the 4 presets of the physical button |

The writes to `PrsM 0..=3` that were ignored earlier were writes to read-only factory presets
(the mic acks every `SetP` regardless). A write is also rejected when the stored size would exceed
the area's capacity (not known). `revelator-cli presets dump` reads `Stat 0..3` and `PrsM 8..15`
by default; `s0..s3` and `0..15` select areas. What exactly `Stat n` holds (a full document or a
reference) is to be confirmed from a dump.

## Firmware reversing notes (image at DLL file 0x3455b0, 0x76bf0 bytes, Cortex-M Thumb)

- Load address: **address = image file offset + 0x6001fa00** (vector table at image offset 0x600:
  SP 0x20000600, reset 0x60020335). Confirmed because 7 of 8 known function starts appear as
  Thumb pointers in the image's vtables at that base.
- `Appl` x switch (host `Para`/`Pari`): function at image 0x24570, jump table `tbh` at 0x24586.
  x=1 -> hpVolume (+0x10c0), x=2 -> mainVolume (+0x10c4), x=3 -> input1 gain (+0x10cc, also writes
  input 2 when channel-link +0x1107 is set), **x=4 -> input2 gain (+0x10d0)**, mirror of x=3. So UC
  param 600 / `fx.mix` is NOT an FX amount; the write is no longer offered.
- Preset memory (`MemP` PrsM/Stat, handlers at image 0x26f8c/0x2715c/0x271cc) is a plain chunked
  byte store; nothing in the firmware parses the preset documents (no key strings besides the 8
  factory documents), so the PC must send the DSP blobs when the active slot changes.
- **`Setu` (engine setup)**: every Voice FX engine in the firmware has a `Setu` handler. The Robot one
  (image 0x46af0) reads rate (f32 at blob+8) and an enable word (u32 at blob+0xc): non-zero builds a
  257-entry table and the delay buffers (once, flag +0x698); zero disables the engine. The firmware's
  own internal `Setu` messages (e.g. image 0x4b7ec) carry zeros, i.e. a reset. UC's sender
  (0x180048a10 / 0x180057200) builds `[Setu, 0x1C, fs, 128, 1, 1, 0]`.
  **Superseded (2026-11-15):** that UC `Setu` goes to UC's PC-side engine (receive 0x1800076b0) and
  never over USB. The firmware sets its engines up itself (`[Setu, 0x1C, fs, block, 2, 2, fmt]` from
  its audio-format function while flag +0x50c5 is set). The app no longer sends `Setu` at all
  (`with_setup` / `voicefx_setup` removed; the safety list refuses it on every block).
- Firmware `Setu` handlers exist for every Voice FX engine (vdog, may4, inia, botb, botc, vech), the
  reverb (`vrvb`, handler at image 0x476e8: rate at +8 stored to +0x39c, size at +0xC passed to the
  allocator, init only if > 0) and the biquad/limiter engines. Word +0xC is a block size (UC sends
  128). (No longer sent by the app to either block; see above.)
- **Block 0 `Pari` x=8 / `Para` x=9 are the OUTPUT DELAY bus / time**, not a Voice FX insert
  (firmware states 23/24 outputDelayBus/outputDelay, routine 0x4b7b0, default bus -1). UC never
  writes them directly; it drives them through host `Appl` Pari x=13 (UI value - 1) / Para x=14.
  Switching the delay on (x=8 >= 0) sends a zero `Setu` to the Voice FX manager, which tears Voice FX
  down. The app used to write x=8 = 0 (and x=9 = 1, x=4 = 1) on connect: removed, and refused by
  `mixer::fx_write_ok`. Block 0 `Para` x=4 is the per-channel DSP amount the firmware sets itself
  from host `Appl` x=4.
- `VFxO` is the firmware's half-rate switch (1 when fs > 48 kHz), not an enable; no longer sent.
- UC's whole Voice FX path is `VoFx` (`SetP 201 0 [VoFx, 0x10, 0, model]`) followed by the model's
  blobs (with their `on` word and `mix`); that is now exactly what the app sends.
- **`VoFx` is a deferred engine rebuild.** Firmware 0x41b44 never compares with the running model:
  it stores it and sets a pending flag (+0x44); the manager tick 0x3cf30 switches later (vtable
  +0x34, once +0x39 is set and +0x40 is 0). Until then the forwarder 0x3cbdc passes every blob to
  the OLD effect (+0x14), so blobs sent right after `VoFx` are lost. UC calls its model setter only
  on an `fxmodel` change (0x180049fc8, param 0x1c2) or insert creation (0x18003f470), and sends the
  blobs on a later tick. The app now sends `VoFx` only on a model change (GUI) and waits 200 ms
  after it (`device::VOFX_SWITCH_WAIT`).
- Robot receive 0x467f4 copies `botc` words in UC's order; pitch = comb delay in samples.
- `Targ` has no firmware handler: PC-side only.
- `processingChannel` (host `Appl` x=12) is a `Pari` in UC's binding and in the firmware's Pari
  remap (states 19/20); the app now sends it as `Pari` (it was `Para`).

## Effect gating: what is and is not established (firmware image 1)

- Host `Appl` x=4 writes +0x10d0 (second input gain; y is ignored). It is not the FX amount. Confirmed again by reading the x=3/x=4 handlers (0x248a2 / 0x248da).
- obj+0x10a4/+0x10a8 are ints initialised to -1 (0x25ede..0x25eec, 0x267f4). The tick function (0x25268) reads FxMix state ids 3/4 from the variant provider and re-sends `Para x=4` only when the value differs from these caches, so FxMix is NOT set by any host `Appl` x. It comes from the provider (per active slot), probably from stored preset data.
- UC (dspusbdevice.dll 0x1800414d9): `processingChannel` (id 149) == 0 calls 0x18003f470(true), which builds the insert-FX object from the Voice FX model (param 450). Default -1 = no insert. Host x=12 (firmware: float clamped to [-1,1] at +0x10dc) is exposed as `fx.channel`; unverified on hardware.
- Probe: `fx-probe.ps1` steps through fx.channel candidates.
- Correction from reading the tick (0x25268) fully: its `Para 0x14 y=ch x=4 FxMix` goes out through obj vtable +0x4c, i.e. it is a firmware-to-host notification emitted after a slot change (vtable +0x54 applies the slot), not an input to the DSP. FxMix is therefore not a gate for the effect engines; the gate must be host-side data (blobs, `fx.channel`, mixer routing).
- Voice FX engine (firmware 0x41b90): accepts `VoFx` models 0..5 only (0x41b44 rejects >= 6), and `VFxO` (set from the audio-format function at 0x4bf0e: 1 if fs > 48000, else 0). The firmware's own `Setu` is `[Setu, 0x1c, fs, block, 2, 2, <word at +0x10>]`, ours is `[.., 128, 1, 1, 0]`; `REVELATOR_SETU` + `fx-setu-probe.ps1` test variants.
