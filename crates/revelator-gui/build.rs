//! Bakes the app icon (drawn by `src/icon.rs`) and version information (the app's name, which
//! Windows shows in Task Manager, startup apps, "Open with", the file's properties...) into
//! the .exe. The resource file is written by hand (no rc.exe needed): MSVC's linker accepts a
//! compiled `.res` directly.

#[path = "src/icon.rs"]
mod icon;

use std::path::PathBuf;

const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

/// The name Windows shows for the app.
const APP_NAME: &str = "Revelator Control";

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if env("CARGO_CFG_TARGET_OS") != "windows" {
        return;
    }
    if env("CARGO_CFG_TARGET_ENV") != "msvc" {
        println!("cargo:warning=app icon not embedded: only done for the MSVC toolchain");
        return;
    }
    let out = PathBuf::from(env("OUT_DIR"));
    let images: Vec<Vec<u8>> = SIZES.iter().map(|&n| icon::dib(n, None)).collect();

    // The .ico file is not needed for the build; it is handy for shortcuts or installers.
    std::fs::write(out.join("app.ico"), ico(&images)).expect("write app.ico");

    // Icon groups: 1 = the app (Explorer uses the first), 2..=5 = the slot glyphs used by
    // the jump list tasks (icon indexes 1..=4 of the .exe).
    let mut groups = vec![SIZES.iter().copied().zip(images.iter().cloned()).collect()];
    for slot in 0..icon::SLOTS {
        let g = icon::Glyph::Slot { slot, active: true };
        groups.push(
            [16u32, 24, 32, 48]
                .iter()
                .map(|&n| (n, icon::dib_from_rgba(n, &icon::glyph_rgba(n, g))))
                .collect(),
        );
    }
    let res = out.join("app-icon.res");
    std::fs::write(&res, resource_file(&groups)).expect("write app-icon.res");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

fn ico(images: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0, 0, 1, 0]);
    v.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (img, &n) in images.iter().zip(&SIZES) {
        let b = if n >= 256 { 0 } else { n as u8 };
        v.extend_from_slice(&[b, b, 0, 0]);
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(&(img.len() as u32).to_le_bytes());
        v.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        v.extend_from_slice(img);
    }
    v
}

/// A Win32 `.res` file: the empty leading entry, then per icon group (ids 1..) one RT_ICON
/// per image (ids numbered on across groups) and the RT_GROUP_ICON listing them, then the
/// version information.
fn resource_file(groups: &[Vec<(u32, Vec<u8>)>]) -> Vec<u8> {
    let mut v = Vec::new();
    entry(&mut v, 0, 0, 0, &[]);
    let mut next_id: u16 = 1;
    for (g, images) in groups.iter().enumerate() {
        let mut group = Vec::new();
        group.extend_from_slice(&[0, 0, 1, 0]);
        group.extend_from_slice(&(images.len() as u16).to_le_bytes());
        for (n, img) in images {
            entry(&mut v, RT_ICON, next_id, 0x1010, img);
            let b = if *n >= 256 { 0 } else { *n as u8 };
            group.extend_from_slice(&[b, b, 0, 0]);
            group.extend_from_slice(&1u16.to_le_bytes());
            group.extend_from_slice(&32u16.to_le_bytes());
            group.extend_from_slice(&(img.len() as u32).to_le_bytes());
            group.extend_from_slice(&next_id.to_le_bytes());
            next_id += 1;
        }
        entry(&mut v, RT_GROUP_ICON, g as u16 + 1, 0x1030, &group);
    }
    entry(&mut v, RT_VERSION, 1, 0x0030, &version_info());
    v
}

/// A VS_VERSIONINFO block: fixed file info plus the US English / Unicode string table.
fn version_info() -> Vec<u8> {
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = ver
        .split(['.', '-', '+'])
        .map(|p| p.parse::<u32>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let ms = (major << 16) | (minor & 0xffff);
    let ls = patch << 16;

    let mut fixed = Vec::new();
    for x in [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,
        ls, // file version
        ms,
        ls,          // product version
        0x3f,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0, // subtype, date
    ] {
        fixed.extend_from_slice(&x.to_le_bytes());
    }

    let exe = format!(
        "{}.exe",
        std::env::var("CARGO_PKG_NAME").unwrap_or_default()
    );
    let strings: Vec<Vec<u8>> = [
        ("FileDescription", APP_NAME),
        ("ProductName", APP_NAME),
        ("FileVersion", &ver),
        ("ProductVersion", &ver),
        ("OriginalFilename", &exe),
        ("InternalName", APP_NAME),
    ]
    .iter()
    .map(|(k, v)| {
        let val: Vec<u8> = utf16z(v);
        block(k, 1, (val.len() / 2) as u16, &val, &[])
    })
    .collect();
    let table = block("040904B0", 1, 0, &[], &strings);
    let sfi = block("StringFileInfo", 1, 0, &[], &[table]);
    let mut tr = Vec::new();
    tr.extend_from_slice(&0x0409u16.to_le_bytes());
    tr.extend_from_slice(&0x04B0u16.to_le_bytes());
    let var = block("Translation", 0, 4, &tr, &[]);
    let vfi = block("VarFileInfo", 1, 0, &[], &[var]);
    block(
        "VS_VERSION_INFO",
        0,
        fixed.len() as u16,
        &fixed,
        &[sfi, vfi],
    )
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(Some(0))
        .flat_map(|c| c.to_le_bytes())
        .collect()
}

/// One version-info node: header, key, value and children, each DWORD aligned.
/// `value_len` is in bytes for binary values and in WCHARs for text ones (Windows' rules).
fn block(key: &str, ty: u16, value_len: u16, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let pad = |v: &mut Vec<u8>| {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    };
    let mut v = vec![0, 0];
    v.extend_from_slice(&value_len.to_le_bytes());
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&utf16z(key));
    pad(&mut v);
    v.extend_from_slice(value);
    for c in children {
        pad(&mut v);
        v.extend_from_slice(c);
    }
    let n = v.len() as u16;
    v[0..2].copy_from_slice(&n.to_le_bytes());
    v
}

/// One resource entry with a numeric type and name (header is 32 bytes, DWORD aligned).
fn entry(v: &mut Vec<u8>, ty: u16, name: u16, flags: u16, data: &[u8]) {
    let lang: u16 = if ty == 0 { 0 } else { 0x0409 };
    v.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    v.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    v.extend_from_slice(&[0xff, 0xff]);
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&[0xff, 0xff]);
    v.extend_from_slice(&name.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    v.extend_from_slice(&flags.to_le_bytes()); // MemoryFlags
    v.extend_from_slice(&lang.to_le_bytes()); // LanguageId
    v.extend_from_slice(&0u32.to_le_bytes()); // Version
    v.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    v.extend_from_slice(data);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}
