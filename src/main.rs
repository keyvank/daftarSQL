use std::{collections::HashMap, error::Error, path::PathBuf};

use anyhow::anyhow;
use bincode::{Decode, Encode};

#[derive(Debug, Clone)]
struct GenericPage<const SZ: usize>([u8; SZ]);

type Blob = Vec<u8>;
type Page = GenericPage<8192>;

impl TryFrom<Blob> for Page {
    type Error = anyhow::Error;
    fn try_from(value: Blob) -> Result<Self, Self::Error> {
        Ok(Self(
            value
                .try_into()
                .map_err(|_| anyhow!("Blob does not fit into a page"))?,
        ))
    }
}

impl Default for Page {
    fn default() -> Self {
        Self([0; 8192])
    }
}

trait Storage: Sized {
    fn get_page(&self, idx: u64) -> Result<Page, anyhow::Error>;
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
    fn get_page(&self, idx: u64) -> Result<Page, anyhow::Error> {
        Ok(self.0.get(&idx).cloned().unwrap_or_default())
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
    fn get_page(&self, idx: u64) -> Result<Page, anyhow::Error> {
        if let Some(overwrite) = self.overwrite.get(&idx) {
            Ok(overwrite.clone())
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
    pub fn commit(&mut self) -> Result<(), anyhow::Error> {
        let res = self
            .base
            .set_pages(self.overwrite.clone().into_iter().collect());
        if res.is_ok() {
            self.overwrite.clear();
        }
        res
    }
}

impl<'a, S: Storage> Storage for OverwrittenStorageMut<'a, S> {
    fn get_page(&self, idx: u64) -> Result<Page, anyhow::Error> {
        if let Some(overwrite) = self.overwrite.get(&idx) {
            Ok(overwrite.clone())
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
enum Node {
    Internal {
        keys: Vec<Blob>,
        children: Vec<u64>,
    },
    Leaf {
        keys: Vec<Blob>,
        values: Vec<Blob>,
        next: Option<u64>,
    },
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
    fn insert(&mut self, pairs: Vec<(Blob, Blob)>) -> Result<(), anyhow::Error> {
        // Try to load the root.
        let root_page = self.storage.get_page(0)?;

        // If the page is all zeros, initialize it as an empty leaf.
        let mut root = Node::try_from(root_page).unwrap_or(Node::Leaf {
            keys: Vec::new(),
            values: Vec::new(),
            next: None,
        });

        match &mut root {
            Node::Leaf { keys, values, .. } => {
                for (key, value) in pairs {
                    match keys.binary_search(&key) {
                        // Replace existing value.
                        Ok(idx) => values[idx] = value,

                        // Insert while maintaining sorted order.
                        Err(idx) => {
                            keys.insert(idx, key);
                            values.insert(idx, value);
                        }
                    }
                }
            }

            Node::Internal { .. } => {
                anyhow::bail!("internal nodes are not implemented yet");
            }
        }

        let page: Page = root.try_into()?;
        self.storage.set_pages(vec![(0, page)])?;

        Ok(())
    }

    fn get(&self, key: &Blob) -> Result<Blob, anyhow::Error> {
        let root_page = self.storage.get_page(0)?;

        let root = Node::try_from(root_page)?;

        match root {
            Node::Leaf { keys, values, .. } => match keys.binary_search(key) {
                Ok(idx) => Ok(values[idx].clone()),
                Err(_) => anyhow::bail!("key not found"),
            },

            Node::Internal { .. } => {
                anyhow::bail!("internal nodes are not implemented yet");
            }
        }
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
    let mut forked2 = forked.fork_mut();

    forked2.set_pages(vec![(0, Page::default())])?;

    forked2.commit()?;

    println!("{:?}", forked2.get_page(0)?);

    let mut storage = KvStore { storage: forked2 };
    storage.insert(vec![])?;

    storage.fork_mut().fork_mut().fork_mut().fork_mut();
    
    println!("Hello, world!");

    Ok(())
}
