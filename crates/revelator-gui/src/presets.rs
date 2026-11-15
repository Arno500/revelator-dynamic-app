//! User presets: named scene files kept in `%APPDATA%\revelator-control\presets`, plus the
//! app-side names of the mic's four preset slots (the mic itself stores no names).

use std::fs;
use std::path::{Path, PathBuf};

const EXT: &str = "json";
/// Folder holding the user presets.
pub fn default_dir() -> PathBuf {
    base_dir().join("presets")
}

fn base_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("revelator-control")
}

/// A preset name made safe for a file name: no path separators or reserved characters, trimmed.
pub fn clean_name(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !c.is_control() && !r#"\/:*?"<>|"#.contains(*c))
        .collect();
    s.trim().trim_end_matches('.').trim().to_string()
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.{EXT}"))
}

/// Preset names, sorted case-insensitively. A missing folder is an empty list.
pub fn list(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == EXT).then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    v.sort_by_key(|n| n.to_lowercase());
    v
}

pub fn exists(dir: &Path, name: &str) -> bool {
    path(dir, name).exists()
}

/// Write `text` as preset `name` (replacing an existing one). Returns the cleaned name.
pub fn save(dir: &Path, name: &str, text: &str) -> Result<String, String> {
    let name = clean_name(name);
    if name.is_empty() {
        return Err("give the preset a name".into());
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    fs::write(path(dir, &name), text).map_err(|e| e.to_string())?;
    Ok(name)
}

pub fn read(dir: &Path, name: &str) -> Result<String, String> {
    fs::read_to_string(path(dir, name)).map_err(|e| e.to_string())
}

/// Rename `from` to `to`; refuses to overwrite another preset. Returns the cleaned new name.
pub fn rename(dir: &Path, from: &str, to: &str) -> Result<String, String> {
    let to = clean_name(to);
    if to.is_empty() {
        return Err("give the preset a name".into());
    }
    if to == from {
        return Ok(to);
    }
    // A case-only change is the same file on Windows; anything else must not clobber.
    if !to.eq_ignore_ascii_case(from) && exists(dir, &to) {
        return Err(format!("a preset named \"{to}\" already exists"));
    }
    fs::rename(path(dir, from), path(dir, &to)).map_err(|e| e.to_string())?;
    Ok(to)
}

pub fn delete(dir: &Path, name: &str) -> Result<(), String> {
    fs::remove_file(path(dir, name)).map_err(|e| e.to_string())
}

// ------------------------------------------------------------ slot names

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("revelator-presets-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  my/preset:1 . "), "mypreset1");
        assert_eq!(clean_name("..."), "");
        assert_eq!(clean_name("Été ok"), "Été ok");
    }

    #[test]
    fn save_list_rename_delete() {
        let d = tmp("crud");
        assert!(list(&d).is_empty());
        assert_eq!(save(&d, "b preset", "{}").unwrap(), "b preset");
        save(&d, "A", "1").unwrap();
        assert_eq!(list(&d), ["A", "b preset"]);
        assert!(save(&d, "  ", "x").is_err());
        // rename: refuses to clobber, allows case change
        assert!(rename(&d, "A", "b preset").is_err());
        assert_eq!(rename(&d, "A", "a").unwrap(), "a");
        assert_eq!(rename(&d, "a", "Renamed").unwrap(), "Renamed");
        assert_eq!(read(&d, "Renamed").unwrap(), "1");
        delete(&d, "Renamed").unwrap();
        assert_eq!(list(&d), ["b preset"]);
        assert!(delete(&d, "Renamed").is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
