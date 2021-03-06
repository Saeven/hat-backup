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

//! Helpers for reading directory structures from the local filesystem.

use std::fs;
use std::io;
use std::iter;
use std::path::PathBuf;
use std::sync::mpsc;

use scoped_threadpool;


pub trait HasPath {
    fn path(&self) -> PathBuf;
}

impl HasPath for fs::DirEntry {
    fn path(&self) -> PathBuf {
        fs::DirEntry::path(self)
    }
}

pub trait PathHandler<P: Send>: Sync {
    type DirItem: HasPath;
    type DirIter: iter::Iterator<Item = io::Result<Self::DirItem>>;

    fn read_dir(&self, &PathBuf) -> io::Result<Self::DirIter>;
    fn handle_path(&self, &P, &PathBuf) -> Option<P>;

    fn recurse(&self, root: PathBuf, payload: P) {
        let threads = 10;
        let (push_ch, work_ch) = mpsc::sync_channel(threads);
        let mut pool = scoped_threadpool::Pool::new(threads as u32);

        enum Work<P> {
            Done,
            More(PathBuf, P),
        }

        // Insert the first task into the queue:
        push_ch.send(Work::More(root, payload)).unwrap();
        let mut active_workers = 0;

        // Master thread:
        pool.scoped(|scoped| {
            loop {
                match work_ch.recv() {
                    Err(_) => unreachable!(),
                    Ok(Work::Done) => {
                        // A worker has completed a task.
                        // We are done when no more workers are active (i.e. all tasks are done):
                        active_workers -= 1;
                        if active_workers == 0 {
                            break;
                        }
                    }
                    Ok(Work::More(root, payload)) => {
                        // Execute the task in a pool thread:
                        active_workers += 1;

                        let push_ch_ = push_ch.clone();
                        scoped.execute(move || {
                            match self.read_dir(&root) {
                                Ok(dir) => {
                                    for entry_res in dir {
                                        match entry_res {
                                            Ok(entry) => {
                                                let path = entry.path();
                                                let dir_opt = self.handle_path(&payload, &path);
                                                if let Some(dir) = dir_opt {
                                                    push_ch_.send(Work::More(path, dir)).unwrap();
                                                }
                                            }
                                            Err(err) => {
                                                // For some reason, we failed to read this entry.
                                                // Just skip it and continue with the next.
                                                warn!("Could not read directory entry: {}", err);
                                            }
                                        }
                                    }
                                }
                                Err(err) => {
                                    // Cannot read this directory.
                                    warn!("Skipping unreadable directory {:?}: {}", root, err);
                                }
                            }
                            // Count this pool thread as idle:
                            push_ch_.send(Work::Done).unwrap();
                        });
                    }
                }
            }
        });
    }
}


#[cfg(test)]
mod tests {

    use std::collections::btree_map;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::vec;

    use super::*;

    type ParentOpt = Option<PathBuf>;
    type VisitedPaths = btree_map::BTreeMap<PathBuf, bool>;

    impl HasPath for PathBuf {
        fn path(&self) -> PathBuf {
            self.to_owned()
        }
    }

    struct StubPathHandler {
        paths: Mutex<VisitedPaths>,
    }

    impl StubPathHandler {
        fn new(paths: Vec<PathBuf>) -> StubPathHandler {
            let mut tree = btree_map::BTreeMap::new();
            for path in paths {
                tree.insert(path, false);
            }
            StubPathHandler { paths: Mutex::new(tree) }
        }

        fn visit(&self, path: PathBuf) -> Option<bool> {
            let mut paths = self.paths.lock().unwrap();
            paths.insert(path, true)
        }

        fn list(&self, dir: &PathBuf) -> vec::IntoIter<io::Result<PathBuf>> {
            let paths = self.paths.lock().unwrap();
            let mut contents = vec![];
            for k in paths.keys() {
                if k.parent() == Some(dir) {
                    contents.push(Ok(k.clone()));
                }
            }
            contents.into_iter()
        }

        fn not_visited(&self) -> Vec<PathBuf> {
            let paths = self.paths.lock().unwrap();
            let mut contents = vec![];
            for (path, visited) in paths.iter() {
                if !visited {
                    contents.push(path.clone());
                }
            }
            contents
        }
    }

    impl PathHandler<ParentOpt> for StubPathHandler {
        type DirItem = PathBuf;
        type DirIter = vec::IntoIter<io::Result<Self::DirItem>>;

        fn read_dir(&self, path: &PathBuf) -> io::Result<Self::DirIter> {
            Ok(self.list(path))
        }

        fn handle_path(&self, p_opt: &ParentOpt, path: &PathBuf) -> Option<ParentOpt> {
            assert_eq!(self.visit(path.clone()), Some(false));

            if let &Some(ref p) = p_opt {
                assert!(path.parent() == Some(p));
            }

            self.list(path).next().map(|_| Some(path.clone()))
        }
    }

    #[test]
    fn can_visit_all() {
        let paths: [&str; 20] = ["/",
                                 "/foo",
                                 "/bar/",
                                 "/bar/baz/",
                                 "/bar/baz/qux",
                                 "/bar/baz/foo",
                                 "/bar/baz/bar/",
                                 "/bar/baz/bar/foo",
                                 "/bar/baz/bar/bar/",
                                 "/bar/baz/bar/bar/bar",
                                 "/empty/",
                                 "/empty/1/",
                                 "/empty/2/",
                                 "/empty/3/",
                                 "/empty/4/",
                                 "/empty/5/",
                                 "/empty/6/",
                                 "/empty/7/",
                                 "/empty/8/",
                                 "/empty/9/"];

        let handler = StubPathHandler::new(paths.iter().map(PathBuf::from).collect());
        handler.recurse(PathBuf::from("/"), None);

        assert_eq!(handler.not_visited(), vec![PathBuf::from("/")]);
    }

}
