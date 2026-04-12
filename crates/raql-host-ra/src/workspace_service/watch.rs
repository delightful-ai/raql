use crossbeam_channel::{Receiver, unbounded};
use vfs::loader::LoadingProgress;

pub(super) struct WatchBatch {
    pub(super) changed_files: Vec<(vfs::AbsPathBuf, Option<Vec<u8>>)>,
}

#[derive(Debug)]
pub(super) struct WorkspaceWatcher {
    _handle: Box<dyn vfs::loader::Handle>,
    receiver: Receiver<vfs::loader::Message>,
    ready: bool,
}

impl WorkspaceWatcher {
    pub(super) fn new(watched_entries: &[vfs::loader::Entry]) -> Self {
        let (sender, receiver) = unbounded();
        let handle: vfs_notify::NotifyHandle = vfs::loader::Handle::spawn(sender);
        let mut handle = Box::new(handle) as Box<dyn vfs::loader::Handle>;
        handle.set_config(vfs::loader::Config {
            load: watched_entries.to_vec(),
            watch: (0..watched_entries.len()).collect(),
            version: 0,
        });
        Self {
            _handle: handle,
            receiver,
            ready: false,
        }
    }

    pub(super) fn drain(&mut self) -> WatchBatch {
        let mut changed_files = Vec::new();
        for message in self.receiver.try_iter() {
            match message {
                vfs::loader::Message::Changed { files } => changed_files.extend(files),
                vfs::loader::Message::Progress { n_done, .. } => {
                    if n_done == LoadingProgress::Finished {
                        self.ready = true;
                    }
                }
                vfs::loader::Message::Loaded { .. } => {}
            }
        }
        WatchBatch {
            changed_files,
        }
    }
}
