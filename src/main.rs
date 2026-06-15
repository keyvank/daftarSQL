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
        page.0.copy_from_slice(&value);
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
    fn get_node(&self, node_id: u64) -> Result<Option<Node>, anyhow::Error> {
        if let Some(page) = self.storage.get_page(node_id)? {
            Ok(Some(page.try_into()?))
        } else {
            Ok(None)
        }
    }

    fn find_leaf(&self, key: &Blob, node_id: u64) -> Result<Option<Leaf>, anyhow::Error> {
        let node = self.get_node(node_id)?;
        Ok(match node {
            Some(Node::Leaf(leaf)) => Some(leaf),
            Some(Node::Internal(internal)) => {
                for i in 0..internal.keys.len() {
                    if key <= &internal.keys[i] {
                        return self.find_leaf(key, internal.children[i]);
                    }
                }
                return self.find_leaf(key, internal.children[internal.keys.len()]);
            }
            None => None,
        })
    }

    fn insert(&mut self, pairs: Vec<(Blob, Blob)>) -> Result<(), anyhow::Error> {
        unimplemented!()
    }

    fn get(&self, key: &Blob) -> Result<Option<Blob>, anyhow::Error> {
        Ok(if let Some(leaf) = self.find_leaf(key, 0)? {
            if let Ok(idx) = leaf.keys.binary_search(key) {
                Some(leaf.values[idx].clone())
            } else {
                None
            }
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
    let mut forked = storage.fork();

    let mut tx_1 = forked.fork_mut();
    tx_1.set_pages(vec![(0, Page::default())])?;
    tx_1.set_pages(vec![(1, Page::default())])?;
    tx_1.set_pages(vec![(2, Page::default())])?;
    tx_1.commit()?; // All at once

    let mut tx_2 = forked.fork_mut();
    tx_2.set_pages(vec![(0, Page::default())])?;
    tx_2.set_pages(vec![(1, Page::default())])?;
    tx_2.set_pages(vec![(2, Page::default())])?;
    tx_2.commit()?; // All at once

    println!("{:?}", forked.fork().get_page(0)?);

    let mut storage = KvStore {
        storage: forked.fork(),
    };
    storage.insert(vec![])?;

    storage.fork_mut().fork_mut().fork_mut().fork_mut();

    println!("Hello, world!");

    Ok(())
}
