//! Bridges the GDK clipboard and the RDP clipboard channel.
//!
//! Remote → local: the server's formats are advertised through a custom
//! [`gdk::ContentProvider`] that fetches the data only when something pastes.
//! Local → remote: local mime types are announced on every change and read
//! lazily when the server asks for them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use rdp_core::SessionHandle;

/// Mime types we can translate to RDP clipboard formats.
const SUPPORTED: &[&str] = &[
    "text/uri-list",
    "x-special/gnome-copied-files",
    "text/html",
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "image/png",
    "image/bmp",
    "image/jpeg",
    "image/webp",
    "image/tiff",
];

const REMOTE_TIMEOUT: Duration = Duration::from_secs(60);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RemoteContent {
        pub mimes: RefCell<Vec<String>>,
        pub bridge: RefCell<Weak<super::Inner>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RemoteContent {
        const NAME: &'static str = "FerrumRemoteContent";
        type Type = super::RemoteContent;
        type ParentType = gdk::ContentProvider;
    }

    impl ObjectImpl for RemoteContent {}

    impl ContentProviderImpl for RemoteContent {
        fn formats(&self) -> gdk::ContentFormats {
            let mut builder = gdk::ContentFormatsBuilder::new();
            for m in self.mimes.borrow().iter() {
                builder = builder.add_mime_type(m);
            }
            builder.build()
        }

        fn write_mime_type_future(
            &self,
            mime_type: &str,
            stream: &gio::OutputStream,
            io_priority: glib::Priority,
        ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
            let bridge = self.bridge.borrow().upgrade();
            let mime = mime_type.to_string();
            let stream = stream.clone();
            Box::pin(async move {
                let fail = |msg: &str| glib::Error::new(gio::IOErrorEnum::Failed, msg);
                let bridge = bridge.ok_or_else(|| fail("session closed"))?;
                let rx = bridge.request_remote(&mime);
                let data = glib::future_with_timeout(REMOTE_TIMEOUT, rx.recv())
                    .await
                    .map_err(|_| fail("remote clipboard timed out"))?
                    .ok()
                    .flatten()
                    .ok_or_else(|| fail("remote clipboard data unavailable"))?;
                stream
                    .write_all_future(data, io_priority)
                    .await
                    .map_err(|(_, e)| e)?;
                Ok(())
            })
        }
    }
}

glib::wrapper! {
    pub struct RemoteContent(ObjectSubclass<imp::RemoteContent>) @extends gdk::ContentProvider;
}

pub struct Inner {
    handle: SessionHandle,
    clipboard: gdk::Clipboard,
    to_local: bool,
    to_remote: bool,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, async_channel::Sender<Option<Vec<u8>>>>>,
    provider: RefCell<Option<RemoteContent>>,
    changed: RefCell<Option<glib::SignalHandlerId>>,
}

impl Inner {
    fn request_remote(&self, mime: &str) -> async_channel::Receiver<Option<Vec<u8>>> {
        let (tx, rx) = async_channel::bounded(1);
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        self.pending.borrow_mut().insert(id, tx);
        self.handle.clipboard_request(mime, id);
        rx
    }
}

pub struct ClipboardBridge {
    inner: Rc<Inner>,
}

impl ClipboardBridge {
    pub fn new(
        display: &gdk::Display,
        handle: SessionHandle,
        to_local: bool,
        to_remote: bool,
    ) -> Self {
        let inner = Rc::new(Inner {
            handle,
            clipboard: display.clipboard(),
            to_local,
            to_remote,
            next_id: Cell::new(0),
            pending: RefCell::new(HashMap::new()),
            provider: RefCell::new(None),
            changed: RefCell::new(None),
        });
        if to_remote {
            let weak = Rc::downgrade(&inner);
            let id = inner.clipboard.connect_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    announce_local(&inner);
                }
            });
            inner.changed.replace(Some(id));
        }
        ClipboardBridge { inner }
    }

    /// Announce what is currently on the local clipboard (call once connected).
    pub fn sync(&self) {
        if self.inner.to_remote {
            announce_local(&self.inner);
        }
    }

    /// The server's clipboard changed.
    pub fn remote_offer(&self, mimes: Vec<String>) {
        if !self.inner.to_local || mimes.is_empty() {
            return;
        }
        let provider: RemoteContent = glib::Object::new();
        provider.imp().mimes.replace(mimes);
        provider.imp().bridge.replace(Rc::downgrade(&self.inner));
        if self
            .inner
            .clipboard
            .set_content(Some(provider.upcast_ref::<gdk::ContentProvider>()))
            .is_ok()
        {
            self.inner.provider.replace(Some(provider));
        }
    }

    pub fn remote_data(&self, id: u64, data: Option<Vec<u8>>) {
        if let Some(tx) = self.inner.pending.borrow_mut().remove(&id) {
            let _ = tx.try_send(data);
        }
    }

    /// The server wants local clipboard content; answer through `reply`.
    pub fn local_get(&self, mime: String, reply: SyncSender<Option<Vec<u8>>>) {
        if !self.inner.to_remote {
            let _ = reply.send(None);
            return;
        }
        let clipboard = self.inner.clipboard.clone();
        glib::spawn_future_local(async move {
            let data = read_clipboard(&clipboard, &mime).await;
            let _ = reply.send(data);
        });
    }

    /// Stop syncing; drop our content from the clipboard if it is still there.
    pub fn detach(&self) {
        if let Some(id) = self.inner.changed.take() {
            self.inner.clipboard.disconnect(id);
        }
        if let Some(provider) = self.inner.provider.take() {
            let ours = self
                .inner
                .clipboard
                .content()
                .is_some_and(|c| c == *provider.upcast_ref::<gdk::ContentProvider>());
            if ours {
                let _ = self
                    .inner
                    .clipboard
                    .set_content(None::<&gdk::ContentProvider>);
            }
        }
        self.inner.pending.borrow_mut().clear();
    }
}

impl Drop for ClipboardBridge {
    fn drop(&mut self) {
        self.detach();
    }
}

fn announce_local(inner: &Inner) {
    // Our own provider was just installed: nothing new to tell the server.
    if inner.clipboard.is_local() {
        return;
    }
    let formats = inner.clipboard.formats();
    let mut mimes: Vec<String> = formats
        .mime_types()
        .iter()
        .map(|m| m.to_string())
        .filter(|m| SUPPORTED.contains(&m.as_str()))
        .collect();
    // GTK can always convert a texture to PNG.
    if formats.contains_type(gdk::Texture::static_type()) && !mimes.iter().any(|m| m == "image/png")
    {
        mimes.push("image/png".into());
    }
    inner.handle.clipboard_offer(&mimes);
}

async fn read_clipboard(clipboard: &gdk::Clipboard, mime: &str) -> Option<Vec<u8>> {
    let (input, _) = clipboard
        .read_future(&[mime], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let out = gio::MemoryOutputStream::new_resizable();
    out.splice_future(
        &input,
        gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
        glib::Priority::DEFAULT,
    )
    .await
    .ok()?;
    Some(out.steal_as_bytes().to_vec())
}
