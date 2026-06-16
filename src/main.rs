use std::{collections::HashMap, error::Error, path::PathBuf};

use anyhow::anyhow;
use bincode::{Decode, Encode};

#[derive(Debug, Clone, PartialEq)]
struct GenericPage<const SZ: usize>([u8; SZ]);

type Blob = Vec<u8>;
type Page = GenericPage<8192>;

impl TryFrom<Blob> for Page {
    type Error = anyhow::Error;
    fn try_from(value: Blob) -> Result<Self, Self::Error> {
        let mut page = Page::default();
        if value.len() > page.0.len() {
            return Err(anyhow!("Blob larger than page!"));
        }
        page.0[..value.len()].copy_from_slice(&value);
        Ok(page)
    }
}

impl Default for Page {
    fn default() -> Self {
        Self([0; 8192])
    }
}

trait Storage: Sized {
    fn get_page(&self, idx: u64) -> Result<Option<Page>, anyhow::Error>;
    fn set_pages(&mut self, pages: Vec<(u64, Page)>) -> Result<(), anyhow::Error>;
    fn fork(&self) -> OverwrittenStorage<'_, Self> {
        OverwrittenStorage {
            base: self,
            overwrite: HashMap::new(),
        }
    }
    fn fork_mut(&mut self) -> OverwrittenStorageMut<'_, Self> {
        OverwrittenStorageMut {
            base: self,
            overwrite: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RamStorage(HashMap<u64, Page>);

impl Storage for RamStorage {
    fn get_page(&self, idx: u64) -> Result<Option<Page>, anyhow::Error> {
        Ok(self.0.get(&idx).cloned())
    }
    fn set_pages(&mut self, pages: Vec<(u64, Page)>) -> Result<(), anyhow::Error> {
        for (idx, page) in pages {
            self.0.insert(idx, page);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct OverwrittenStorage<'a, S: Storage> {
    base: &'a S,
    overwrite: HashMap<u64, Page>,
}

impl<'a, S: Storage> Storage for OverwrittenStorage<'a, S> {
    fn get_page(&self, idx: u64) -> Result<Option<Page>, anyhow::Error> {
        if let Some(overwrite) = self.overwrite.get(&idx) {
            Ok(Some(overwrite.clone()))
        } else {
            self.base.get_page(idx)
        }
    }
    fn set_pages(&mut self, pages: Vec<(u64, Page)>) -> Result<(), anyhow::Error> {
        for (idx, page) in pages {
            self.overwrite.insert(idx, page);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct OverwrittenStorageMut<'a, S: Storage> {
    base: &'a mut S,
    overwrite: HashMap<u64, Page>,
}

impl<'a, S: Storage> OverwrittenStorageMut<'a, S> {
    pub fn commit(self) -> Result<(), anyhow::Error> {
        self.base
            .set_pages(self.overwrite.clone().into_iter().collect())?;
        Ok(())
    }
}

impl<'a, S: Storage> Storage for OverwrittenStorageMut<'a, S> {
    fn get_page(&self, idx: u64) -> Result<Option<Page>, anyhow::Error> {
        if let Some(overwrite) = self.overwrite.get(&idx) {
            Ok(Some(overwrite.clone()))
        } else {
            self.base.get_page(idx)
        }
    }
    fn set_pages(&mut self, pages: Vec<(u64, Page)>) -> Result<(), anyhow::Error> {
        for (idx, page) in pages {
            self.overwrite.insert(idx, page);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct Leaf {
    keys: Vec<Blob>,
    values: Vec<Blob>,
    next: Option<u64>,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct Internal {
    keys: Vec<Blob>,
    children: Vec<u64>,
}

#[derive(Debug, Clone, Encode, Decode)]
enum Node {
    Internal(Internal),
    Leaf(Leaf),
}

const DAFTAR_SQL_MAGIC: u64 = 0xfedcba;

#[derive(Debug, Clone, Encode, Decode)]
struct Metadata {
    pub magic: u64,
    pub version: u64,
    pub root_node: u64,
}

impl TryInto<Page> for Node {
    type Error = anyhow::Error;
    fn try_into(self) -> Result<Page, Self::Error> {
        let bytes = bincode::encode_to_vec(self, bincode::config::standard())?;
        bytes.try_into()
    }
}

impl TryFrom<Page> for Node {
    type Error = anyhow::Error;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        let (node, _) = bincode::decode_from_slice(&page.0, bincode::config::standard())?;
        Ok(node)
    }
}

struct KvStore<S: Storage> {
    storage: S,
}

impl<S: Storage> KvStore<S> {
    fn init(&mut self) -> Result<(), anyhow::Error> {
        if self.metadata().is_ok() {
            return Err(anyhow!("Already initialized!"));
        }
        let mut page_0 = Page::default();
        let metadata_ser = bincode::encode_to_vec(
            &Metadata {
                magic: DAFTAR_SQL_MAGIC,
                version: 1,
                root_node: 1,
            },
            bincode::config::standard(),
        )?;
        page_0.0[..metadata_ser.len()].copy_from_slice(&metadata_ser);
        self.storage.set_pages(vec![
            (0, page_0),
            (
                1,
                Node::Leaf(Leaf {
                    keys: vec![],
                    values: vec![],
                    next: None,
                })
                .try_into()?,
            ),
        ])?;
        Ok(())
    }
    fn metadata(&self) -> Result<Metadata, anyhow::Error> {
        let page_0 = self
            .storage
            .get_page(0)?
            .ok_or(anyhow!("Metadata unavailable!"))?;
        let (metadata, _): (Metadata, _) =
            bincode::decode_from_slice(&page_0.0, bincode::config::standard())?;
        if metadata.magic != DAFTAR_SQL_MAGIC {
            return Err(anyhow!("Invalid magic number!"));
        }
        Ok(metadata)
    }

    fn get_node(&self, node_id: u64) -> Result<Option<Node>, anyhow::Error> {
        if let Some(page) = self.storage.get_page(node_id)? {
            Ok(Some(page.try_into()?))
        } else {
            Ok(None)
        }
    }

    fn find_leaf(&self, key: &Blob, node_id: u64) -> Result<(u64, Leaf), anyhow::Error> {
        let node = self.get_node(node_id)?.ok_or(anyhow!("Node not found!"))?;
        Ok(match node {
            Node::Leaf(leaf) => (node_id, leaf),
            Node::Internal(internal) => {
                for i in 0..internal.keys.len() {
                    if key <= &internal.keys[i] {
                        return self.find_leaf(key, internal.children[i]);
                    }
                }
                return self.find_leaf(key, internal.children[internal.keys.len()]);
            }
        })
    }

    fn insert(&mut self, pairs: Vec<(Blob, Blob)>) -> Result<(), anyhow::Error> {
        let root_id = self.metadata()?.root_node;
        let mut fork = self.fork_mut();
        for (k, v) in pairs {
            let (node_id, mut leaf) = fork.find_leaf(&k, root_id)?;
            match leaf.keys.binary_search(&k) {
                Ok(idx) => {
                    leaf.values[idx] = v;
                }
                Err(idx) => {
                    leaf.keys.insert(idx, k);
                    leaf.values.insert(idx, v);
                }
            }
            fork.storage
                .set_pages(vec![(node_id, Node::Leaf(leaf).try_into()?)])?;
        }
        fork.storage.commit()?;
        Ok(())
    }

    fn get(&self, key: &Blob) -> Result<Option<Blob>, anyhow::Error> {
        let root_id = self.metadata()?.root_node;
        let (_, leaf) = self.find_leaf(key, root_id)?;
        Ok(if let Ok(idx) = leaf.keys.binary_search(key) {
            Some(leaf.values[idx].clone())
        } else {
            None
        })
    }
    fn fork(&self) -> KvStore<OverwrittenStorage<'_, S>> {
        KvStore {
            storage: self.storage.fork(),
        }
    }
    fn fork_mut(&mut self) -> KvStore<OverwrittenStorageMut<'_, S>> {
        KvStore {
            storage: self.storage.fork_mut(),
        }
    }
}

fn main() -> Result<(), anyhow::Error> {
    let storage = RamStorage::default();
    let mut db = KvStore { storage };
    db.init()?;

    db.insert(vec![
        (b"f".into(), b"fff".into()),
        (b"a".into(), b"aaa".into()),
        (b"2".into(), b"222".into()),
        (b"5".into(), b"555".into()),
        (b"4".into(), b"444".into()),
        (b"1".into(), b"111".into()),
        (b"3".into(), b"333".into()),
        (b"d".into(), b"ddd".into()),
    ])?;

    println!("{:?}", db.get_node(1)?);

    //println!("{:?}", db.get(&b"5".into())?);

    Ok(())
}
