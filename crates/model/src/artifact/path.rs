//! The rules a path in a manifest must follow, checked on its bytes.
//!
//! A manifest comes from another machine, and its paths decide where files
//! are written. These checks are what keeps every write inside the
//! destination. The module is translated to Lean and proven (see `proofs/`),
//! so it is written plainly: loops and indexing over bytes, with no strings,
//! iterators or closures.

/// Longest name a file or directory may have, in bytes. Most file systems
/// allow 255, and a file's part file needs room for its suffix.
pub const MAX_NAME_BYTES: usize = 246;

/// Longest target a link may have, in bytes.
pub const MAX_TARGET_BYTES: usize = 4096;

/// What is added to a file's name while it is being written: `.mux-part`.
const PART_SUFFIX: [u8; 9] = *b".mux-part";

/// An ASCII capital as its small letter, and any other byte as itself.
fn lower(byte: u8) -> u8 {
    if byte < b'A' {
        return byte;
    }
    if byte > b'Z' {
        return byte;
    }
    byte + 32
}

/// Whether `name` ends with the part-file suffix, in any letter case.
pub fn is_part_name(name: &[u8]) -> bool {
    if name.len() < PART_SUFFIX.len() {
        return false;
    }
    let start = name.len() - PART_SUFFIX.len();
    let mut at = 0;
    while at < PART_SUFFIX.len() {
        if lower(name[start + at]) != PART_SUFFIX[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether `name` is `.`, which names the directory it is in.
fn is_current(name: &[u8]) -> bool {
    if name.len() != 1 {
        return false;
    }
    name[0] == b'.'
}

/// Whether `name` is `..`, which names the directory above.
fn is_parent(name: &[u8]) -> bool {
    if name.len() != 2 {
        return false;
    }
    if name[0] != b'.' {
        return false;
    }
    name[1] == b'.'
}

/// Whether a byte can never be part of a name: a separator on some
/// platform, a drive colon, or NUL.
fn is_separator(byte: u8) -> bool {
    if byte == b'/' {
        return true;
    }
    if byte == b'\\' {
        return true;
    }
    if byte == b':' {
        return true;
    }
    byte == 0
}

/// Whether `name` may be the name of a file or directory: it is not empty,
/// not too long, not `.` or `..`, holds no separator, drive colon or NUL,
/// and is not a part file's name. Such a name always means "one level down,
/// inside this directory", on every platform.
pub fn is_safe_name(name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.len() > MAX_NAME_BYTES {
        return false;
    }
    if is_current(name) {
        return false;
    }
    if is_parent(name) {
        return false;
    }
    let mut at = 0;
    while at < name.len() {
        if is_separator(name[at]) {
            return false;
        }
        at += 1;
    }
    !is_part_name(name)
}

/// Whether `path` is one or more safe names joined by `/`. Such a path
/// stays inside the directory it is taken from.
pub fn is_safe_path(path: &[u8]) -> bool {
    let mut start = 0;
    let mut at = 0;
    while at < path.len() {
        if path[at] == b'/' {
            if !is_safe_name(&path[start..at]) {
                return false;
            }
            start = at + 1;
        }
        at += 1;
    }
    is_safe_name(&path[start..])
}

/// How many names `path` has, which is how far below the root it lies. The
/// empty path is the root, at depth zero.
pub fn depth(path: &[u8]) -> u32 {
    if path.is_empty() {
        return 0;
    }
    let mut names: u32 = 1;
    let mut at = 0;
    while at < path.len() {
        if path[at] == b'/' && names < u32::MAX {
            names += 1;
        }
        at += 1;
    }
    names
}

/// The length of the path of the directory that holds `path`: everything
/// before its last `/`, or nothing if it has none.
pub fn parent_len(path: &[u8]) -> usize {
    let mut at = path.len();
    while at > 0 {
        at -= 1;
        if path[at] == b'/' {
            return at;
        }
    }
    0
}

/// Whether a link that lies `depth` names below the root may point at
/// `target`. The target is relative: names joined by `/`, each a safe name
/// or `..`. Followed from the directory that holds the link, it must never
/// climb above the root.
pub fn is_safe_target(target: &[u8], depth: u32) -> bool {
    if target.is_empty() || target.len() > MAX_TARGET_BYTES || depth == 0 {
        return false;
    }
    // How far below the root the walk is. It starts in the link's directory.
    let mut level = u64::from(depth - 1);
    let mut start = 0;
    let mut at = 0;
    while at <= target.len() {
        if at == target.len() || target[at] == b'/' {
            let name = &target[start..at];
            if is_parent(name) {
                if level == 0 {
                    return false;
                }
                level -= 1;
            } else if is_safe_name(name) {
                level += 1;
            } else {
                return false;
            }
            start = at + 1;
        }
        at += 1;
    }
    true
}

/// Whether `a` sorts strictly before `b`, comparing byte by byte. Entries
/// are kept in this order, which also makes every path different from the
/// one before it.
pub fn precedes(a: &[u8], b: &[u8]) -> bool {
    let mut at = 0;
    while at < a.len() && at < b.len() {
        if a[at] != b[at] {
            return a[at] < b[at];
        }
        at += 1;
    }
    a.len() < b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_safe_name_is_one_level_down() {
        for name in [
            "a", "a.bin", "..a", "a..", "...", "é", "a b", "mux-part", "a.part",
        ] {
            assert!(is_safe_name(name.as_bytes()), "{name:?}");
        }
        for name in ["", ".", "..", "a/b", "a\\b", "c:", "a\0", "/"] {
            assert!(!is_safe_name(name.as_bytes()), "{name:?}");
        }
    }

    #[test]
    fn a_name_is_at_most_as_long_as_leaves_room_for_the_part_suffix() {
        assert!(is_safe_name(&[b'a'; MAX_NAME_BYTES]));
        assert!(!is_safe_name(&[b'a'; MAX_NAME_BYTES + 1]));
        assert_eq!(MAX_NAME_BYTES + PART_SUFFIX.len(), 255);
    }

    #[test]
    fn a_name_may_not_look_like_a_part_file() {
        for name in ["a.mux-part", "a.MUX-Part", ".mux-part", ".MUX-PART"] {
            assert!(is_part_name(name.as_bytes()), "{name:?}");
            assert!(!is_safe_name(name.as_bytes()), "{name:?}");
        }
        for name in [
            "a.mux-part.bin",
            "mux-part",
            "a.part",
            "",
            ".mux-par",
            "a.mux_part",
        ] {
            assert!(!is_part_name(name.as_bytes()), "{name:?}");
        }
    }

    #[test]
    fn a_safe_path_is_safe_names_joined_by_slashes() {
        for path in ["a", "a/b", "a/b/c.bin", "..a/b"] {
            assert!(is_safe_path(path.as_bytes()), "{path:?}");
        }
        for path in [
            "",
            "/",
            "/a",
            "a/",
            "a//b",
            "../x",
            "a/../../x",
            "a/./b",
            "a\\b",
            "c:x",
            "a/b.mux-part/c",
        ] {
            assert!(!is_safe_path(path.as_bytes()), "{path:?}");
        }
    }

    #[test]
    fn depth_counts_names_and_the_parent_is_what_comes_before_the_last() {
        for (path, names, parent) in [
            ("", 0, ""),
            ("a", 1, ""),
            ("a/b", 2, "a"),
            ("a/b/c", 3, "a/b"),
        ] {
            assert_eq!(depth(path.as_bytes()), names, "{path:?}");
            assert_eq!(&path[..parent_len(path.as_bytes())], parent, "{path:?}");
        }
    }

    #[test]
    fn a_link_may_point_anywhere_inside_the_root_and_nowhere_else() {
        // (target, depth of the link, allowed)
        for (target, depth, allowed) in [
            ("b", 1, true),
            ("b/c", 1, true),
            ("../b", 2, true),
            ("../../b", 3, true),
            ("../b/../c", 2, true),
            ("x/../..", 2, true),
            ("..", 2, true),
            ("..", 1, false),
            ("../b", 1, false),
            ("../../b", 2, false),
            ("b/../../c", 1, false),
            ("/etc/passwd", 3, false),
            ("", 1, false),
            ("b/", 1, false),
            ("b//c", 1, false),
            ("./b", 1, false),
            ("b\\c", 1, false),
            ("b", 0, false),
        ] {
            assert_eq!(
                is_safe_target(target.as_bytes(), depth),
                allowed,
                "{target:?} from depth {depth}"
            );
        }
        assert!(is_safe_target(&[b'a'; MAX_NAME_BYTES], 1));
        assert!(!is_safe_target(&[b'a'; MAX_NAME_BYTES + 1], 1));
        let long = "a/".repeat(MAX_TARGET_BYTES / 2);
        assert!(is_safe_target(&long.as_bytes()[..MAX_TARGET_BYTES - 1], 1));
        assert!(!is_safe_target(format!("{long}a").as_bytes(), 1));
    }

    #[test]
    fn precedes_is_the_order_of_byte_strings() {
        let samples = ["", "a", "a-b", "a/b", "a0", "ab", "b", "é", "A"];
        for a in samples {
            for b in samples {
                assert_eq!(
                    precedes(a.as_bytes(), b.as_bytes()),
                    a.as_bytes() < b.as_bytes(),
                    "{a:?} and {b:?}"
                );
            }
        }
    }
}

/// Bounded proofs, run by `just kani`. They hold for every string of up to
/// `MAX_BYTES` bytes. Each compares a check with the obvious way to walk a
/// path: one name at a time, as a file system does.
#[cfg(kani)]
mod proofs {
    use super::*;

    const MAX_BYTES: usize = 5;

    /// Any bytes, and how many of them count.
    fn any_bytes() -> ([u8; MAX_BYTES], usize) {
        let bytes: [u8; MAX_BYTES] = kani::any();
        let len: usize = kani::any_where(|&len| len <= MAX_BYTES);
        (bytes, len)
    }

    /// Where the name that starts at `start` ends: at the next `/`, or at
    /// the end.
    fn name_end(path: &[u8], start: usize) -> usize {
        let mut end = start;
        while end < path.len() && path[end] != b'/' {
            end += 1;
        }
        end
    }

    /// Every name of a path that is accepted goes one level down: none is
    /// empty, `.` or `..`, and none holds a separator of another platform.
    /// So the path stays inside the directory it is taken from.
    #[kani::proof]
    #[kani::unwind(7)]
    fn a_safe_path_only_goes_down() {
        let (bytes, len) = any_bytes();
        let path = &bytes[..len];
        if !is_safe_path(path) {
            return;
        }
        let mut start = 0;
        while start <= path.len() {
            let end = name_end(path, start);
            let name = &path[start..end];
            assert!(!name.is_empty());
            assert!(name != b"." && name != b"..");
            let mut at = 0;
            while at < name.len() {
                assert!(name[at] != b'\\' && name[at] != b':' && name[at] != 0);
                at += 1;
            }
            start = end + 1;
        }
    }

    /// Following a target that is accepted, one name at a time from the
    /// directory that holds the link, never climbs above the root.
    #[kani::proof]
    #[kani::unwind(7)]
    fn a_safe_target_never_climbs_above_the_root() {
        let (bytes, len) = any_bytes();
        let target = &bytes[..len];
        let depth: u32 = kani::any();
        if !is_safe_target(target, depth) {
            return;
        }
        assert!(depth > 0 && !target.is_empty());
        let mut level = u64::from(depth - 1);
        let mut start = 0;
        while start <= target.len() {
            let end = name_end(target, start);
            let name = &target[start..end];
            if name == b".." {
                assert!(level > 0);
                level -= 1;
            } else {
                assert!(!name.is_empty() && name != b".");
                level += 1;
            }
            start = end + 1;
        }
    }

    /// `precedes` is the order of byte strings, so it is never true both
    /// ways round or of a string and itself.
    #[kani::proof]
    #[kani::unwind(7)]
    fn precedes_is_the_order_of_byte_strings() {
        let (first, first_len) = any_bytes();
        let (second, second_len) = any_bytes();
        let (a, b) = (&first[..first_len], &second[..second_len]);
        assert!(precedes(a, b) == (a < b));
        assert!(!(precedes(a, b) && precedes(b, a)));
        assert!(!precedes(a, a));
    }

    /// No check can fail on any bytes: none reads past the end or
    /// overflows.
    #[kani::proof]
    #[kani::unwind(7)]
    fn no_check_can_fail() {
        let (bytes, len) = any_bytes();
        let path = &bytes[..len];
        let _ = is_safe_name(path);
        let _ = is_part_name(path);
        // One name more than there are separators, and none for the root.
        let names = depth(path);
        assert!(names as usize <= len + 1);
        assert!((names == 0) == (len == 0));
        assert!(parent_len(path) <= len);
    }
}
