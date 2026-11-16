//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! RAGE RBF files (`RBF0`: three `_manifest.ymf` of the base game are in this older form): a tree
//! of named elements, little-endian. After the 4-byte magic, a stream of items:
//!
//! - `FF FF`: closes the element open;
//! - `FD FF <u32 length> <bytes>`: a value (text) of the element open;
//! - `<name index> <type>`: an element. The index is that of its name among the names met so far;
//!   an index equal to their count introduces a new name, `<u16 length> <bytes>`. Type 0 opens an
//!   element (6 bytes follow), 0x10 a number (4 bytes), 0x20 and 0x30 true and false, 0x40 a float
//!   (4 bytes), 0x50 three floats (12 bytes), 0x60 text (`<u16 length> <bytes>`).
//!
//! Only the shape is read (names, nesting, texts): enough to tell what a manifest holds.

use anyhow::{Context, Result, bail};

#[derive(Debug, Default)]
pub struct Element {
    pub name: String,
    pub children: Vec<Element>,
    /// The texts held directly (`FD FF` values), without their final zero.
    pub texts: Vec<String>,
}

impl Element {
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
}

pub fn is_rbf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"RBF0")
}

/// The root element of an RBF file.
pub fn parse(bytes: &[u8]) -> Result<Element> {
    if !is_rbf(bytes) {
        bail!("not an RBF file");
    }
    let take = |at: &mut usize, n: usize| -> Result<&[u8]> {
        let s = bytes.get(*at..*at + n).context("truncated RBF")?;
        *at += n;
        Ok(s)
    };
    let mut names: Vec<(String, u8)> = Vec::new();
    // the elements open, the innermost last; the first one is a holder for the root
    let mut open: Vec<Element> = vec![Element::default()];
    let mut at = 4;
    while at < bytes.len() {
        let head = take(&mut at, 2)?;
        let (index, kind) = (head[0] as usize, head[1]);
        match (index, kind) {
            (0xFF, 0xFF) => {
                if open.len() < 2 {
                    bail!("RBF: a close with no element open");
                }
                let done = open.pop().unwrap();
                open.last_mut().unwrap().children.push(done);
            }
            (0xFD, 0xFF) => {
                let len = u32::from_le_bytes(take(&mut at, 4)?.try_into().unwrap()) as usize;
                let text = take(&mut at, len)?;
                let text = text.strip_suffix(&[0]).unwrap_or(text);
                open.last_mut().unwrap().texts.push(String::from_utf8_lossy(text).into_owned());
            }
            _ => {
                if index == names.len() {
                    let len = u16::from_le_bytes(take(&mut at, 2)?.try_into().unwrap()) as usize;
                    names.push((String::from_utf8_lossy(take(&mut at, len)?).into_owned(), kind));
                }
                let (name, declared) = names.get(index).with_context(|| format!("RBF: unknown name {index}"))?;
                if *declared != kind {
                    bail!("RBF: {name} of type {kind:02X}, {declared:02X} declared");
                }
                let leaf = |texts: Vec<String>| Element { name: name.clone(), children: Vec::new(), texts };
                match kind {
                    0x00 => {
                        take(&mut at, 6)?;
                        open.push(leaf(Vec::new()));
                    }
                    0x10 | 0x40 => {
                        take(&mut at, 4)?;
                        open.last_mut().unwrap().children.push(leaf(Vec::new()));
                    }
                    0x20 | 0x30 => open.last_mut().unwrap().children.push(leaf(Vec::new())),
                    0x50 => {
                        take(&mut at, 12)?;
                        open.last_mut().unwrap().children.push(leaf(Vec::new()));
                    }
                    0x60 => {
                        let len = u16::from_le_bytes(take(&mut at, 2)?.try_into().unwrap()) as usize;
                        let text = String::from_utf8_lossy(take(&mut at, len)?).into_owned();
                        open.last_mut().unwrap().children.push(leaf(vec![text]));
                    }
                    other => bail!("RBF: {name} of unknown type {other:02X}"),
                }
            }
        }
    }
    // the file may end without closing its root
    while open.len() > 1 {
        let done = open.pop().unwrap();
        open.last_mut().unwrap().children.push(done);
    }
    let mut holder = open.pop().unwrap();
    if holder.children.len() != 1 {
        bail!("RBF: {} roots", holder.children.len());
    }
    Ok(holder.children.pop().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree() {
        let mut b = b"RBF0".to_vec();
        let open = |b: &mut Vec<u8>, index: u8, name: Option<&str>| {
            b.extend([index, 0]);
            if let Some(n) = name {
                b.extend((n.len() as u16).to_le_bytes());
                b.extend(n.as_bytes());
            }
            b.extend([0; 6]);
        };
        let text = |b: &mut Vec<u8>, t: &str| {
            b.extend([0xFD, 0xFF]);
            b.extend((t.len() as u32 + 1).to_le_bytes());
            b.extend(t.as_bytes());
            b.push(0);
        };
        open(&mut b, 0, Some("CPackFileMetaData"));
        open(&mut b, 1, Some("MapDataGroups"));
        b.extend([0xFF, 0xFF]);
        open(&mut b, 2, Some("HDTxdBindingArray"));
        for target in ["a_lod", "b_lod"] {
            open(&mut b, 3, (target == "a_lod").then_some("Item"));
            open(&mut b, 4, (target == "a_lod").then_some("targetAsset"));
            text(&mut b, target);
            b.extend([0xFF, 0xFF, 0xFF, 0xFF]);
        }
        b.extend([0xFF, 0xFF, 0xFF, 0xFF]);
        let root = parse(&b).unwrap();
        assert_eq!(root.name, "CPackFileMetaData");
        assert!(root.child("MapDataGroups").unwrap().children.is_empty());
        let items = &root.child("HDTxdBindingArray").unwrap().children;
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].child("targetAsset").unwrap().texts, ["b_lod"]);
        // cut in the middle of a text, or a name index never introduced
        assert!(parse(&b[..b.len() - 9]).is_err());
        assert!(parse(b"RBF0\x05\x00").is_err());
        assert!(parse(b"PSIN").is_err());
    }
}
