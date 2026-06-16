use std::{
    collections::HashMap,
    error::Error,
    fs::{File, OpenOptions},
    io::SeekFrom,
    path::PathBuf,
};

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

#[derive(Debug)]
struct FileStorage(File);
use std::io::{Read, Seek, Write};

impl Storage for FileStorage {
    fn get_page(&self, idx: u64) -> Result<Option<Page>, anyhow::Error> {
        let mut file = &self.0;
        let offset = idx * 8192;
        let mut page = Page::default();
        file.seek(SeekFrom::Start(offset))?;
        let n = file.read(&mut page.0)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(page))
    }
    fn set_pages(&mut self, pages: Vec<(u64, Page)>) -> Result<(), anyhow::Error> {
        for (idx, page) in pages {
            self.0.seek(SeekFrom::Start(idx * 8192))?;
            self.0.write_all(&page.0)?;
        }
        self.0.flush()?;
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

#[derive(Debug, Clone, Encode, Decode)]
struct Free {
    prev_free_page_id: u64,
    next_free_page_id: u64,
}

#[derive(Debug, Clone, Encode, Decode)]
enum PageContent {
    Ready,
    Free(Free),
    Metadata(Metadata),
    Node(Node),
}

const DAFTAR_SQL_MAGIC: u64 = 0xfedcba;

#[derive(Debug, Clone, Encode, Decode)]
struct Metadata {
    pub magic: u64,
    pub version: u64,
    pub root_node_page_id: u64,          // The page where root node exists
    pub next_page_id: u64,               // Next unallocated page
    pub first_free_page_id: Option<u64>, // First free page we can allocate
}

struct KvStore<S: Storage> {
    storage: S,
}

impl<S: Storage> KvStore<S> {
    fn set_page_content(&mut self, idx: u64, content: PageContent) -> Result<(), anyhow::Error> {
        let mut page = Page::default();
        let ser = bincode::encode_to_vec(&content, bincode::config::standard())?;
        page.0[..ser.len()].copy_from_slice(&ser);
        self.storage.set_pages(vec![(idx, page)])?;
        Ok(())
    }

    fn get_page_content(&self, idx: u64) -> Result<PageContent, anyhow::Error> {
        let page = self
            .storage
            .get_page(idx)?
            .ok_or(anyhow!("Page unavailable!"))?;
        let (content, _): (PageContent, _) =
            bincode::decode_from_slice(&page.0, bincode::config::standard())?;
        Ok(content)
    }

    fn get_node_page(&self, page_id: u64) -> Result<Node, anyhow::Error> {
        if let PageContent::Node(node) = self.get_page_content(page_id)? {
            Ok(node)
        } else {
            Err(anyhow!("Expected a node!"))
        }
    }

    fn get_free_page(&self, page_id: u64) -> Result<Free, anyhow::Error> {
        if let PageContent::Free(node) = self.get_page_content(page_id)? {
            Ok(node)
        } else {
            Err(anyhow!("Expected a free page!"))
        }
    }

    fn free(&mut self, page_id: u64) -> Result<(), anyhow::Error> {
        let mut metadata = self.metadata()?;
        if let Some(first_free_page_id) = metadata.first_free_page_id {
            let mut free = self.get_free_page(first_free_page_id)?;
            self.set_page_content(
                page_id,
                PageContent::Free(Free {
                    prev_free_page_id: first_free_page_id,
                    next_free_page_id: free.next_free_page_id,
                }),
            )?;

            let mut next_page = self.get_free_page(free.next_free_page_id)?;
            next_page.prev_free_page_id = page_id;
            self.set_page_content(free.next_free_page_id, PageContent::Free(next_page))?;

            free.next_free_page_id = page_id;
            if free.prev_free_page_id == first_free_page_id {
                free.prev_free_page_id = page_id;
            }
            self.set_page_content(first_free_page_id, PageContent::Free(free))?;
        } else {
            self.set_page_content(
                page_id,
                PageContent::Free(Free {
                    prev_free_page_id: page_id,
                    next_free_page_id: page_id,
                }),
            )?;
            metadata.first_free_page_id = Some(page_id);
            self.set_page_content(0, PageContent::Metadata(metadata))?;
        }
        Ok(())
    }

    fn alloc_page(&mut self) -> Result<u64, anyhow::Error> {
        let mut metadata = self.metadata()?;
        if let Some(free_page_id) = metadata.first_free_page_id {
            let free_page = self.get_free_page(free_page_id)?;

            let mut prev_page = self.get_free_page(free_page.prev_free_page_id)?;
            prev_page.next_free_page_id = free_page.next_free_page_id;
            self.set_page_content(free_page.prev_free_page_id, PageContent::Free(prev_page))?;

            let mut next_page = self.get_free_page(free_page.next_free_page_id)?;
            next_page.prev_free_page_id = free_page.prev_free_page_id;
            self.set_page_content(free_page.next_free_page_id, PageContent::Free(next_page))?;

            self.set_page_content(free_page_id, PageContent::Ready)?;

            metadata.first_free_page_id = if free_page.next_free_page_id != free_page_id {
                Some(free_page.next_free_page_id)
            } else {
                None
            };
            self.set_page_content(0, PageContent::Metadata(metadata))?;
            Ok(free_page_id)
        } else {
            let new_page_id = metadata.next_page_id;
            self.set_page_content(new_page_id, PageContent::Ready)?;
            metadata.next_page_id += 1;
            self.set_page_content(0, PageContent::Metadata(metadata))?;
            Ok(new_page_id)
        }
    }

    fn init(&mut self) -> Result<(), anyhow::Error> {
        if self.metadata().is_ok() {
            return Err(anyhow!("Already initialized!"));
        }
        self.set_page_content(
            0,
            PageContent::Metadata(Metadata {
                magic: DAFTAR_SQL_MAGIC,
                version: 1,
                root_node_page_id: 1,
                next_page_id: 2,
                first_free_page_id: None,
            }),
        )?;
        self.set_page_content(
            1,
            PageContent::Node(Node::Leaf(Leaf {
                keys: vec![],
                values: vec![],
                next: None,
            })),
        )?;
        Ok(())
    }

    fn metadata(&self) -> Result<Metadata, anyhow::Error> {
        match self.get_page_content(0)? {
            PageContent::Metadata(metadata) => {
                if metadata.magic != DAFTAR_SQL_MAGIC {
                    return Err(anyhow!("Invalid magic number!"));
                }
                Ok(metadata)
            }
            _ => Err(anyhow!("Page 0 is not a metadata page!")),
        }
    }

    fn get_node(&self, page_id: u64) -> Result<Node, anyhow::Error> {
        if let PageContent::Node(node) = self.get_page_content(page_id)? {
            Ok(node)
        } else {
            Err(anyhow!("Node not found!"))
        }
    }

    fn find_leaf(&self, key: &Blob, node_id: u64) -> Result<(u64, Leaf), anyhow::Error> {
        let node = self.get_node(node_id)?;
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
        let root_id = self.metadata()?.root_node_page_id;
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
            fork.set_page_content(node_id, PageContent::Node(Node::Leaf(leaf)))?;
        }
        fork.storage.commit()?;
        Ok(())
    }

    fn get(&self, key: &Blob) -> Result<Option<Blob>, anyhow::Error> {
        let root_id = self.metadata()?.root_node_page_id;
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
    let storage = FileStorage {
        0: OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .open("data.db")?,
    };
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

    Ok(())
}
