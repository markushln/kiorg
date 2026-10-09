//! D-Bus side: `org.freedesktop.impl.portal.FileChooser` + per-request `Request` objects.
//!
//! Spec: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.FileChooser.html

use std::{collections::HashMap, os::unix::ffi::OsStrExt, path::PathBuf, sync::Arc};

use tokio::sync::Notify;
use zbus::{
    interface,
    zvariant::{Array, ObjectPath, OwnedObjectPath, OwnedValue, Value},
    ObjectServer,
};

use crate::picker::{path_to_uri, Mode, Outcome, Picker, PickerRequest};

/// Portal response codes.
const RESPONSE_OK: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

type Results = HashMap<String, Value<'static>>;

/// Exported at the request `handle`; the frontend calls `Close` to abort.
struct Request {
    cancel: Arc<Notify>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    async fn close(&self) {
        self.cancel.notify_one();
    }
}

pub struct FileChooser {
    picker: Arc<Picker>,
}

impl FileChooser {
    pub fn new(picker: Arc<Picker>) -> Self {
        Self { picker }
    }

    async fn handle(
        &self,
        server: &ObjectServer,
        handle: ObjectPath<'_>,
        mode: Mode,
        title: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let req = request_from_options(mode, title, options);
        let cancel = Arc::new(Notify::new());
        let path = OwnedObjectPath::from(handle);

        if let Err(e) = server
            .at(
                ObjectPath::from(&path),
                Request {
                    cancel: cancel.clone(),
                },
            )
            .await
        {
            tracing::warn!("cannot export request object {path}: {e}");
        }

        let outcome = self.picker.run(&req, &cancel).await;

        let _ = server.remove::<Request, _>(ObjectPath::from(&path)).await;

        match outcome {
            Outcome::Selected(paths) => {
                let uris: Vec<String> = paths.iter().map(|p| path_to_uri(p)).collect();
                let mut r = Results::new();
                r.insert("uris".into(), Value::from(Array::from(uris)));
                (RESPONSE_OK, r)
            }
            Outcome::Cancelled => (RESPONSE_CANCELLED, Results::new()),
            Outcome::Failed(msg) => {
                tracing::error!("picker failed: {msg}");
                (RESPONSE_OTHER, Results::new())
            }
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    /// Also covers "select folder" via `options["directory"] == true`.
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let mode = if opt_bool(&options, "directory") {
            Mode::OpenDirectory
        } else {
            Mode::OpenFile
        };
        self.handle(server, handle, mode, title, &options).await
    }

    async fn save_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        self.handle(server, handle, Mode::Save, title, &options)
            .await
    }
}

// ---- option parsing (pure, unit-testable) -------------------------------------------------

fn opt_bool(o: &HashMap<String, OwnedValue>, k: &str) -> bool {
    o.get(k)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn opt_string(o: &HashMap<String, OwnedValue>, k: &str) -> Option<String> {
    let v = o.get(k)?.try_clone().ok()?;
    String::try_from(v).ok()
}

/// `ay` byte strings are NUL-terminated paths.
fn opt_path(o: &HashMap<String, OwnedValue>, k: &str) -> Option<PathBuf> {
    let v = o.get(k)?.try_clone().ok()?;
    let mut bytes = Vec::<u8>::try_from(v).ok()?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

pub fn request_from_options(
    mode: Mode,
    title: &str,
    o: &HashMap<String, OwnedValue>,
) -> PickerRequest {
    PickerRequest {
        mode,
        multiple: opt_bool(o, "multiple"),
        title: (!title.is_empty()).then(|| title.to_owned()),
        accept_label: opt_string(o, "accept_label"),
        current_folder: opt_path(o, "current_folder"),
        current_name: opt_string(o, "current_name"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov(v: impl Into<Value<'static>>) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn options_are_mapped() {
        let mut o = HashMap::new();
        o.insert("multiple".to_string(), ov(true));
        o.insert("accept_label".to_string(), ov("Open"));
        o.insert(
            "current_folder".to_string(),
            ov(Array::from(b"/home/me\0".to_vec())),
        );
        let r = request_from_options(Mode::OpenFile, "Select", &o);
        assert!(r.multiple);
        assert_eq!(r.title.as_deref(), Some("Select"));
        assert_eq!(r.accept_label.as_deref(), Some("Open"));
        assert_eq!(r.current_folder, Some(PathBuf::from("/home/me")));
    }
}
