//! On-disk cache of previous scan results.
//!
//! After every completed scan the tree is written to
//! `~/Library/Caches/macdirstat/<hash>.mdsc`. When a folder is opened again the
//! cached tree is shown immediately while a fresh scan runs in the background.
//! If there is no cache for the folder itself, the cache of the nearest scanned
//! ancestor is used and the matching subtree is extracted from it.
//!
//! File format: `MAGIC | version u32 | scanned_at u64 | root_path (u32 len + bytes)`
//! followed by an LZ4 block (size-prepended) holding the nodes in pre-order:
//! `varint name_len | name | u8 is_dir | varint (dir ? child_count : size)`.
//! Directory sizes and counts are recomputed on load.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::model::tree::{FileNode, FileTree};

const MAGIC: &[u8; 8] = b"MDSCACHE";
const VERSION: u32 = 1;
/// Keep at most this many cache files; the least recently written are pruned.
const MAX_CACHE_FILES: usize = 32;

/// A tree loaded from the cache, with the time its scan completed.
pub struct CachedTree {
    pub tree: FileTree,
    pub scanned_at: SystemTime,
}

/// A serialized (not yet compressed) tree, ready to be written by [`write`].
pub struct Encoded {
    root_path: String,
    scanned_at: SystemTime,
    body: Vec<u8>,
}

fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Caches/macdirstat"))
}

/// FNV-1a — stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn cache_file(root_path: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{:016x}.mdsc", fnv1a(root_path))))
}

/// Serialize a tree. Cheap enough to run before handing the tree to the UI.
pub fn encode(tree: &FileTree) -> Encoded {
    let mut body = Vec::with_capacity(tree.root.file_count as usize * 24);
    encode_node(&tree.root, &mut body);
    Encoded {
        root_path: tree.root_path.clone(),
        scanned_at: SystemTime::now(),
        body,
    }
}

fn encode_node(node: &FileNode, out: &mut Vec<u8>) {
    write_varint(out, node.name.len() as u64);
    out.extend_from_slice(node.name.as_bytes());
    out.push(node.is_dir as u8);
    if node.is_dir {
        write_varint(out, node.children.len() as u64);
        for child in node.children.iter() {
            encode_node(child, out);
        }
    } else {
        write_varint(out, node.size);
    }
}

/// Compress and atomically write an encoded tree to the cache directory.
pub fn write(encoded: &Encoded) -> io::Result<()> {
    let dir = cache_dir().ok_or_else(|| io::Error::other("HOME not set"))?;
    fs::create_dir_all(&dir)?;
    let path = cache_file(&encoded.root_path).ok_or_else(|| io::Error::other("no cache dir"))?;

    let secs = encoded
        .scanned_at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let compressed = lz4_flex::compress_prepend_size(&encoded.body);

    let mut data = Vec::with_capacity(compressed.len() + encoded.root_path.len() + 24);
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&VERSION.to_le_bytes());
    data.extend_from_slice(&secs.to_le_bytes());
    data.extend_from_slice(&(encoded.root_path.len() as u32).to_le_bytes());
    data.extend_from_slice(encoded.root_path.as_bytes());
    data.extend_from_slice(&compressed);

    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, &data)?;
    fs::rename(&tmp, &path)?;
    prune(&dir);
    Ok(())
}

fn prune(dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mdsc"))
        .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    if files.len() <= MAX_CACHE_FILES {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(MAX_CACHE_FILES) {
        let _ = fs::remove_file(p);
    }
}

/// Load the cached tree for `path`, or extract it from the cache of the
/// nearest ancestor that has one.
pub fn load(path: &Path) -> Option<CachedTree> {
    for ancestor in path.ancestors() {
        let root_path = ancestor.display().to_string();
        let Some(cached) = load_exact(&root_path) else {
            continue;
        };
        if ancestor == path {
            return Some(cached);
        }
        let rel = path.strip_prefix(ancestor).ok()?;
        let names: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let mut node = take_descendant(cached.tree.root, &names)?;
        if !node.is_dir {
            return None;
        }
        node.name = path.display().to_string().into();
        return Some(CachedTree {
            tree: FileTree::from_root(node, path.display().to_string()),
            scanned_at: cached.scanned_at,
        });
    }
    None
}

fn take_descendant(node: FileNode, names: &[String]) -> Option<FileNode> {
    let Some((first, rest)) = names.split_first() else {
        return Some(node);
    };
    let mut children = node.children.into_vec();
    let idx = children.iter().position(|c| *c.name == **first)?;
    take_descendant(children.swap_remove(idx), rest)
}

fn load_exact(root_path: &str) -> Option<CachedTree> {
    let data = fs::read(cache_file(root_path)?).ok()?;
    let mut r = Reader { buf: &data, pos: 0 };
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let secs = r.u64()?;
    let path_len = r.u32()? as usize;
    // Guard against hash collisions.
    if r.take(path_len)? != root_path.as_bytes() {
        return None;
    }
    let body = lz4_flex::decompress_size_prepended(&data[r.pos..]).ok()?;
    let mut r = Reader { buf: &body, pos: 0 };
    let root = decode_node(&mut r)?;
    Some(CachedTree {
        tree: FileTree::from_root(root, root_path.to_string()),
        scanned_at: UNIX_EPOCH + Duration::from_secs(secs),
    })
}

fn decode_node(r: &mut Reader) -> Option<FileNode> {
    let name_len = r.varint()? as usize;
    let name: Box<str> = std::str::from_utf8(r.take(name_len)?).ok()?.into();
    let is_dir = r.byte()? != 0;
    if !is_dir {
        let size = r.varint()?;
        return Some(FileNode {
            name,
            size,
            is_dir: false,
            children: Box::new([]),
            rect: treemap::Rect::new(),
            file_count: 1,
            dir_count: 0,
        });
    }
    let count = r.varint()? as usize;
    // Every node takes at least 3 bytes; reject counts a corrupt file can't hold.
    if count > r.remaining() / 3 {
        return None;
    }
    let mut children = Vec::with_capacity(count);
    let (mut size, mut file_count, mut dir_count) = (0u64, 0u64, 1u64);
    for _ in 0..count {
        let child = decode_node(r)?;
        size += child.size;
        file_count += child.file_count;
        dir_count += child.dir_count;
        children.push(child);
    }
    Some(FileNode {
        name,
        size,
        is_dir: true,
        children: children.into(),
        rect: treemap::Rect::new(),
        file_count,
        dir_count,
    })
}

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> FileNode {
        FileNode {
            name: name.into(),
            size,
            is_dir: false,
            children: Box::new([]),
            rect: treemap::Rect::new(),
            file_count: 1,
            dir_count: 0,
        }
    }

    #[test]
    fn roundtrip() {
        let sub = FileNode {
            name: "sub".into(),
            size: 300,
            is_dir: true,
            children: vec![file("a.txt", 200), file("b", 100)].into(),
            rect: treemap::Rect::new(),
            file_count: 2,
            dir_count: 1,
        };
        let root = FileNode {
            name: "/x".into(),
            size: 100_300,
            is_dir: true,
            children: vec![file("big.mov", 100_000), sub].into(),
            rect: treemap::Rect::new(),
            file_count: 3,
            dir_count: 2,
        };
        let tree = FileTree::from_root(root, "/x".into());
        let enc = encode(&tree);
        let mut r = Reader {
            buf: &enc.body,
            pos: 0,
        };
        let back = decode_node(&mut r).unwrap();
        assert_eq!(back.size, 100_300);
        assert_eq!(back.file_count, 3);
        assert_eq!(back.dir_count, 2);
        assert_eq!(&*back.children[1].children[0].name, "a.txt");

        let sub = take_descendant(back, &["sub".to_string()]).unwrap();
        assert_eq!(sub.size, 300);
    }
}
