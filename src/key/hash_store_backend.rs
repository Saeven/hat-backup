// Copyright 2014 Google Inc. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use backend::StoreBackend;
use blob;
use errors::RetryError;
use hash;
use key::MsgError;
use hash::tree::HashTreeBackend;

pub struct HashStoreBackend<B> {
    hash_index: Arc<hash::HashIndex>,
    blob_store: Arc<blob::BlobStore<B>>,
}
impl<B> Clone for HashStoreBackend<B> {
    fn clone(&self) -> HashStoreBackend<B> {
        HashStoreBackend {
            hash_index: self.hash_index.clone(),
            blob_store: self.blob_store.clone(),
        }
    }
}

impl<B: StoreBackend> HashStoreBackend<B> {
    pub fn new(hash_index: Arc<hash::HashIndex>,
               blob_store: Arc<blob::BlobStore<B>>)
               -> HashStoreBackend<B> {
        HashStoreBackend {
            hash_index: hash_index,
            blob_store: blob_store,
        }
    }

    fn fetch_chunk_from_hash(&self, hash: &hash::Hash) -> Result<Option<Vec<u8>>, MsgError> {
        assert!(!hash.bytes.is_empty());
        match try!(self.hash_index.fetch_persistent_ref(hash)) {
            None => Ok(None),
            Some(chunk_ref) => self.fetch_chunk_from_persistent_ref(&chunk_ref),
        }
    }

    fn fetch_chunk_from_persistent_ref(&self,
                                       chunk_ref: &blob::ChunkRef)
                                       -> Result<Option<Vec<u8>>, MsgError> {
        let res = try!(self.blob_store.retrieve(chunk_ref));
        Ok(res)
    }
}

impl<B: StoreBackend> HashTreeBackend for HashStoreBackend<B> {
    type Err = MsgError;

    fn fetch_chunk(&self,
                   hash: &hash::Hash,
                   persistent_ref: Option<blob::ChunkRef>)
                   -> Result<Option<Vec<u8>>, MsgError> {
        assert!(!hash.bytes.is_empty());

        let data_opt = if let Some(r) = persistent_ref {
            try!(self.fetch_chunk_from_persistent_ref(&r))
        } else {
            try!(self.fetch_chunk_from_hash(&hash))
        };

        Ok(data_opt.and_then(|data| {
            let actual_hash = hash::Hash::new(&data[..]);
            if *hash == actual_hash {
                Some(data)
            } else {
                error!("Data hash does not match expectation: {:?} instead of {:?}",
                       actual_hash,
                       hash);
                None
            }
        }))
    }

    fn fetch_persistent_ref(&self, hash: &hash::Hash) -> Option<blob::ChunkRef> {
        assert!(!hash.bytes.is_empty());
        loop {
            match self.hash_index.fetch_persistent_ref(hash) {
                Ok(Some(r)) => return Some(r), // done
                Ok(None) => return None, // done
                Err(RetryError) => (),  // continue loop
            }
        }
    }

    fn fetch_payload(&self, hash: &hash::Hash) -> Option<Vec<u8>> {
        match self.hash_index.fetch_payload(hash) {
            Some(p) => p, // done
            None => None, // done
        }
    }

    fn insert_chunk(&self,
                    hash: &hash::Hash,
                    level: i64,
                    payload: Option<Vec<u8>>,
                    chunk: Vec<u8>)
                    -> Result<blob::ChunkRef, MsgError> {
        assert!(!hash.bytes.is_empty());

        let mut hash_entry = hash::Entry {
            hash: hash.clone(),
            level: level,
            payload: payload,
            persistent_ref: None,
        };

        match self.hash_index.reserve(&hash_entry) {
            hash::ReserveResult::HashKnown(..) => {
                // Someone came before us: piggyback on their result.
                Ok(self.fetch_persistent_ref(hash)
                    .expect("Could not find persistent_ref for known chunk."))
            }
            hash::ReserveResult::ReserveOk(..) => {
                // We came first: this data-chunk is ours to process.
                let local_hash_index = self.hash_index.clone();

                let local_hash = hash.clone();
                let callback = Box::new(move |chunk_ref: blob::ChunkRef| {
                    local_hash_index.commit(&local_hash, chunk_ref);
                });
                let kind = if level == 0 {
                    blob::Kind::TreeLeaf
                } else {
                    blob::Kind::TreeBranch
                };
                let chunk_ref = self.blob_store.store(chunk, kind, callback);
                hash_entry.persistent_ref = Some(chunk_ref.clone());
                self.hash_index.update_reserved(hash_entry);
                Ok(chunk_ref)
            }
        }
    }
}
