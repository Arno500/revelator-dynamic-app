//! The app icon, drawn in code (no image assets): a white microphone on a rounded square.
//! The plain version is the window, tray and .exe icon (see `build.rs`, which includes this
//! file). The badged versions add the active preset slot: its number on a disc of the slot's
//! colour.
#![allow(dead_code)] // build.rs uses only part of this file

/// Number of preset slots that have their own icon colour.
pub const SLOTS: usize = 4;

/// Slot colours (sRGB), also used for the slot buttons in the UI.
pub const SLOT_COLORS: [[u8; 3]; SLOTS] = [
    [0x4f, 0x9d, 0xff], // 1 blue
    [0x3c, 0xb8, 0x6a], // 2 green
    [0xf0, 0x9a, 0x2c], // 3 amber
    [0xb0, 0x6a, 0xf0], // 4 violet
];

/// Straight (non-premultiplied) RGBA pixels of the plain `n`×`n` icon.
pub fn rgba(n: u32) -> Vec<u8> {
    rgba_slot(n, None)
}

/// RGBA pixels of an `n`×`n` icon, badged with slot `slot` (0-based) when given.
pub fn rgba_slot(n: u32, slot: Option<usize>) -> Vec<u8> {
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    let s = n as f32;
    let slot = slot.map(|i| i.min(SLOTS - 1));
    let (top, bottom) = ([95.0, 155.0, 250.0], [40.0, 90.0, 205.0]);
    let white = [245.0, 248.0, 255.0];
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    // Badge: a disc at the bottom right, cut out of the mic with a background-coloured gap.
    // Larger (and bolder) on small icons such as the tray's, so the number stays readable.
    let (br, weight) = match n {
        0..=20 => (0.34, 0.14),
        21..=28 => (0.31, 0.12),
        _ => (0.27, 0.10),
    };
    let bc = (0.99 - br, 0.99 - br);
    for y in 0..n {
        for x in 0..n {
            // pixel centre in unit coordinates (y down)
            let p = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            // signed distances (unit coords) -> antialiased coverage over ~1px
            let cov = |d: f32| (0.5 - d * s).clamp(0.0, 1.0);

            let bg_a = cov(round_box(p, (0.5, 0.5), (0.46, 0.46), 0.18));
            let t = p.1;
            let bg = [0, 1, 2].map(|i| lerp(top[i], bottom[i], t));
            let mut col = bg;
            let mut alpha = bg_a;
            match slot {
                None => {
                    let fg = cov(mic(p));
                    col = [0, 1, 2].map(|i| lerp(col[i], white[i], fg));
                }
                Some(i) => {
                    let d_badge = len(p.0 - bc.0, p.1 - bc.1) - br;
                    // the mic, minus a ring around the badge
                    let fg = cov(mic(p).max(-(d_badge - 0.05)));
                    col = [0, 1, 2].map(|k| lerp(col[k], white[k], fg));
                    // the badge, in the slot's colour (it may stick out of the rounded square)
                    let b = cov(d_badge);
                    let c = SLOT_COLORS[i].map(|v| v as f32);
                    // a white outline keeps it apart from the background (slot 1 is blue too)
                    let inner = cov(d_badge + (1.0 / s).max(0.04));
                    let badge = [0, 1, 2].map(|k| lerp(white[k], c[k] * 0.9, inner));
                    col = [0, 1, 2].map(|k| lerp(col[k], badge[k], b));
                    alpha = alpha.max(b);
                    // the digit, in white
                    let g = cov(digit(p, i + 1, bc, br * 1.05, weight));
                    col = [0, 1, 2].map(|k| lerp(col[k], white[k], g));
                }
            }
            for c in col {
                px.push(c.round() as u8);
            }
            px.push((alpha * 255.0).round() as u8);
        }
    }
    px
}

/// One icon image in the Windows icon-resource format (BITMAPINFOHEADER + 32-bit BGRA rows
/// bottom-up + empty AND mask): the payload of an .ico entry, an RT_ICON resource, or
/// `CreateIconFromResourceEx`.
pub fn dib(n: u32, slot: Option<usize>) -> Vec<u8> {
    dib_from_rgba(n, &rgba_slot(n, slot))
}

/// [`dib`] of any `n`×`n` RGBA image.
pub fn dib_from_rgba(n: u32, rgba: &[u8]) -> Vec<u8> {
    let mask_row = n.div_ceil(32) * 4;
    let mut v = Vec::with_capacity(40 + (n * n * 4 + mask_row * n) as usize);
    let u32le = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    u32le(&mut v, 40); // biSize
    u32le(&mut v, n); // biWidth
    u32le(&mut v, n * 2); // biHeight: colour + mask
    v.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    v.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    u32le(&mut v, 0); // BI_RGB
    u32le(&mut v, n * n * 4 + mask_row * n); // biSizeImage
    for _ in 0..4 {
        u32le(&mut v, 0); // resolution, palette
    }
    for y in (0..n).rev() {
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            v.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    v.resize(v.len() + (mask_row * n) as usize, 0);
    v
}

/// Small glyphs for buttons and menu entries (no background square).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// The slot's number on a disc of its colour; dimmed when not the active slot.
    Slot { slot: usize, active: bool },
    /// The microphone, crossed out when muted.
    Mic { muted: bool },
}

/// RGBA pixels of an `n`×`n` glyph. A dark outline keeps light parts visible on light
/// backgrounds (light-theme taskbar previews and menus).
pub fn glyph_rgba(n: u32, g: Glyph) -> Vec<u8> {
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    let s = n as f32;
    let white = [250.0, 251.0, 255.0];
    let dark = [30.0, 32.0, 40.0];
    let red = [230.0, 70.0, 70.0];
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let outline = (1.0 / s).max(0.045);
    for y in 0..n {
        for x in 0..n {
            let p = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            let cov = |d: f32| (0.5 - d * s).clamp(0.0, 1.0);
            let (col, a) = match g {
                Glyph::Slot { slot, active } => {
                    let i = slot.min(SLOTS - 1);
                    let d = len(p.0 - 0.5, p.1 - 0.5) - 0.46;
                    let c = SLOT_COLORS[i].map(|v| v as f32);
                    let fill = if active {
                        c
                    } else {
                        // greyed and darker: clearly "not selected"
                        let grey = (c[0] + c[1] + c[2]) / 3.0;
                        c.map(|v| lerp(v, grey, 0.6) * 0.6)
                    };
                    let inner = cov(d + outline);
                    let mut col = [0, 1, 2].map(|k| lerp(dark[k], fill[k], inner));
                    let w = if n <= 20 { 0.15 } else { 0.12 };
                    let dg = cov(digit(p, i + 1, (0.5, 0.5), 0.5, w));
                    col = [0, 1, 2].map(|k| lerp(col[k], white[k], dg));
                    (col, cov(d))
                }
                Glyph::Mic { muted } => {
                    // the mic glyph spans about 0.14..0.85: centre and enlarge it a little
                    let q = (0.5 + (p.0 - 0.5) * 0.95, 0.47 + (p.1 - 0.5) * 0.95);
                    let mut d = mic(q);
                    let slash = segment(p, (0.16, 0.12), (0.84, 0.88)) - 0.05;
                    if muted {
                        // cut a gap around the slash
                        d = d.max(-(slash - outline * 1.5));
                    }
                    let fg = cov(d);
                    let mut col = [0, 1, 2].map(|k| lerp(dark[k], white[k], fg));
                    let mut a = cov(d - outline);
                    if muted {
                        let r_in = cov(slash);
                        col = [0, 1, 2].map(|k| lerp(col[k], dark[k], cov(slash - outline)));
                        col = [0, 1, 2].map(|k| lerp(col[k], red[k], r_in));
                        a = a.max(cov(slash - outline));
                    }
                    (col, a)
                }
            };
            for c in col {
                px.push(c.round() as u8);
            }
            px.push((a * 255.0).round() as u8);
        }
    }
    px
}

/// Distance to the microphone glyph: capsule, U-shaped holder, stem and base.
fn mic(p: (f32, f32)) -> f32 {
    let capsule = segment(p, (0.5, 0.27), (0.5, 0.45)) - 0.125;
    let holder = u_arc(p, (0.5, 0.45), 0.215) - 0.035;
    let stem = segment(p, (0.5, 0.67), (0.5, 0.80)) - 0.035;
    let base = segment(p, (0.36, 0.81), (0.64, 0.81)) - 0.035;
    capsule.min(holder).min(stem).min(base)
}

/// Distance to digit `d` (1..=4) drawn with round strokes, centred on `c`, `h` tall, with
/// strokes `weight * h` wide on each side.
fn digit(p: (f32, f32), d: usize, c: (f32, f32), h: f32, weight: f32) -> f32 {
    let w = h * 0.55;
    // map stroke points given in a unit box (x right, y down) to the badge
    let m = |x: f32, y: f32| (c.0 + (x - 0.5) * w, c.1 + (y - 0.5) * h);
    type Stroke = ((f32, f32), (f32, f32));
    let strokes: &[Stroke] = match d {
        1 => &[((0.6, 0.0), (0.6, 1.0)), ((0.15, 0.3), (0.6, 0.0))],
        2 => &[
            ((0.0, 0.0), (1.0, 0.0)),
            ((1.0, 0.0), (1.0, 0.5)),
            ((1.0, 0.5), (0.0, 0.5)),
            ((0.0, 0.5), (0.0, 1.0)),
            ((0.0, 1.0), (1.0, 1.0)),
        ],
        3 => &[
            ((0.0, 0.0), (1.0, 0.0)),
            ((1.0, 0.0), (1.0, 1.0)),
            ((0.1, 0.5), (1.0, 0.5)),
            ((0.0, 1.0), (1.0, 1.0)),
        ],
        _ => &[
            ((0.0, 0.0), (0.0, 0.6)),
            ((0.0, 0.6), (1.0, 0.6)),
            ((0.8, 0.0), (0.8, 1.0)),
        ],
    };
    let mut best = f32::MAX;
    for &((ax, ay), (bx, by)) in strokes {
        best = best.min(segment(p, m(ax, ay), m(bx, by)));
    }
    best - h * weight
}

fn len(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (pax, pay) = (p.0 - a.0, p.1 - a.1);
    let (bax, bay) = (b.0 - a.0, b.1 - a.1);
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    len(pax - bax * h, pay - bay * h)
}

/// Distance to the lower half of a circle of radius `r` (an open "U").
fn u_arc(p: (f32, f32), c: (f32, f32), r: f32) -> f32 {
    if p.1 >= c.1 {
        (len(p.0 - c.0, p.1 - c.1) - r).abs()
    } else {
        len(p.0 - (c.0 - r), p.1 - c.1).min(len(p.0 - (c.0 + r), p.1 - c.1))
    }
}

fn round_box(p: (f32, f32), c: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let qx = (p.0 - c.0).abs() - half.0 + r;
    let qy = (p.1 - c.1).abs() - half.1 + r;
    len(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - r
}
