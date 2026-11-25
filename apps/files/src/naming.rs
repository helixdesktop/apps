//! File name helpers: extension splitting, copy/link naming, validation.

use helix_app_common::i18n::tr;
use helix_app_common::trf;

/// Compound extensions that are kept together ("x.tar.gz" → ".tar.gz").
const COMPOUND: &[&str] = &[
    "gz", "bz2", "xz", "zst", "lz", "lzma", "lz4", "z", "7z", "br",
];

/// Splits a file name into stem and extension (including the dot).
///
/// A leading dot is part of the stem (".bashrc" has no extension), an
/// extension containing spaces or longer than 10 characters is not treated
/// as one, and `.tar.<compression>` stays together.
pub fn split_extension(name: &str) -> (&str, &str) {
    let Some(dot) = name.rfind('.') else {
        return (name, "");
    };
    if dot == 0 || dot + 1 == name.len() {
        return (name, "");
    }
    let ext = &name[dot + 1..];
    if ext.contains(' ') || ext.chars().count() > 10 {
        return (name, "");
    }
    let stem = &name[..dot];
    if COMPOUND.contains(&ext.to_ascii_lowercase().as_str()) {
        if let Some(inner) = stem.rfind('.') {
            if inner > 0 && stem[inner + 1..].eq_ignore_ascii_case("tar") {
                return (&name[..inner], &name[inner..]);
            }
        }
    }
    (stem, &name[dot..])
}

/// Strips an existing " (copy)" / " (copy N)" suffix, returning the base
/// stem and the copy number that suffix represents (0 when none).
fn strip_copy_suffix(stem: &str) -> (&str, u32) {
    if let Some(base) = stem.strip_suffix(" (copy)") {
        return (base, 1);
    }
    if let Some(inner) = stem.strip_suffix(')') {
        if let Some(idx) = inner.rfind(" (copy ") {
            if let Ok(n) = inner[idx + 7..].parse::<u32>() {
                if n >= 2 {
                    return (&stem[..idx], n);
                }
            }
        }
    }
    (stem, 0)
}

/// Name for the `n`-th copy of `stem` + `ext` (n ≥ 1).
fn nth_copy(stem: &str, ext: &str, n: u32) -> String {
    if n <= 1 {
        trf!("{stem} (copy){ext}", stem = stem, ext = ext)
    } else {
        trf!("{stem} (copy {n}){ext}", stem = stem, n = n, ext = ext)
    }
}

/// Returns a free name for a duplicate of `name`: "a (copy).txt",
/// "a (copy 2).txt", … `exists` reports names already taken.
pub fn copy_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_extension(name);
    let (base, current) = strip_copy_suffix(stem);
    let mut n = current + 1;
    loop {
        let candidate = nth_copy(base, ext, n);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Returns a free name for a symbolic link to `name`.
pub fn link_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    let first = trf!("Link to {name}", name = name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = split_extension(name);
    let mut n = 2;
    loop {
        let candidate = trf!("Link to {stem} ({n}){ext}", stem = stem, n = n, ext = ext);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Returns a free name based on `name`: `name` itself when it is free,
/// else "name (2).ext", "name (3).ext", … (for archives extracted or
/// created next to existing files). `exists` reports names already taken.
pub fn numbered_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = split_extension(name);
    let mut n = 2;
    loop {
        let candidate = trf!("{stem} ({n}){ext}", stem = stem, n = n, ext = ext);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Checks a name typed for a new or renamed file.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err(tr("The name cannot be empty."));
    }
    if name.contains('/') {
        return Err(tr("File names cannot contain “/”."));
    }
    if name == "." {
        return Err(tr("A file cannot be called “.”."));
    }
    if name == ".." {
        return Err(tr("A file cannot be called “..”."));
    }
    if name.len() > 255 {
        return Err(tr("The name is too long."));
    }
    Ok(())
}

/// Character range to pre-select when renaming: the stem, or the whole
/// name for folders.
pub fn rename_selection(name: &str, is_dir: bool) -> (i32, i32) {
    let total = name.chars().count() as i32;
    if is_dir {
        return (0, total);
    }
    let (stem, _) = split_extension(name);
    (0, stem.chars().count() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(split_extension("file.txt"), ("file", ".txt"));
        assert_eq!(split_extension("archive.tar.gz"), ("archive", ".tar.gz"));
        assert_eq!(split_extension("archive.TAR.XZ"), ("archive", ".TAR.XZ"));
        assert_eq!(split_extension("notes.gz"), ("notes", ".gz"));
        assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
        assert_eq!(split_extension(".config.json"), (".config", ".json"));
        assert_eq!(split_extension("README"), ("README", ""));
        assert_eq!(split_extension("trailing."), ("trailing.", ""));
        assert_eq!(
            split_extension("My File.with space"),
            ("My File.with space", "")
        );
        assert_eq!(split_extension("a.b.c"), ("a.b", ".c"));
    }

    #[test]
    fn copy_names() {
        let none = |_: &str| false;
        assert_eq!(copy_name("file.txt", none), "file (copy).txt");
        assert_eq!(copy_name("Folder", none), "Folder (copy)");
        assert_eq!(copy_name("x.tar.gz", none), "x (copy).tar.gz");
        assert_eq!(copy_name(".hidden", none), ".hidden (copy)");
        assert_eq!(copy_name("file (copy).txt", none), "file (copy 2).txt");
        assert_eq!(copy_name("file (copy 7).txt", none), "file (copy 8).txt");
        // "(copy 1)" is not something we generate; treated as part of the name.
        assert_eq!(
            copy_name("file (copy 1).txt", none),
            "file (copy 1) (copy).txt"
        );
        let taken = ["file (copy).txt", "file (copy 2).txt"];
        assert_eq!(
            copy_name("file.txt", |n| taken.contains(&n)),
            "file (copy 3).txt"
        );
    }

    #[test]
    fn link_names() {
        assert_eq!(link_name("a.txt", |_| false), "Link to a.txt");
        let taken = ["Link to a.txt"];
        assert_eq!(
            link_name("a.txt", |n| taken.contains(&n)),
            "Link to a (2).txt"
        );
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("Photos", |_| false), "Photos");
        let taken = ["Photos", "Photos (2)"];
        assert_eq!(
            numbered_name("Photos", |n| taken.contains(&n)),
            "Photos (3)"
        );
        let taken = ["a.tar.gz"];
        assert_eq!(
            numbered_name("a.tar.gz", |n| taken.contains(&n)),
            "a (2).tar.gz"
        );
    }

    #[test]
    fn validation() {
        assert!(validate_name("ok name.txt").is_ok());
        assert!(validate_name(".hidden").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn selection() {
        assert_eq!(rename_selection("photo.jpeg", false), (0, 5));
        assert_eq!(rename_selection("a.tar.gz", false), (0, 1));
        assert_eq!(rename_selection("dir.d", true), (0, 5));
        assert_eq!(rename_selection("ünï.txt", false), (0, 3));
    }
}
