//! D-Bus implementation of the portal FileChooser interface and per-request
//! Request objects.
//!
//! The FileChooser interface receives requests from applications. Each request
//! is passed to the picker backend, which opens the appropriate file-selection
//! UI and returns the result.
//!
//! Spec:
//! https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.FileChooser.html

use std::{collections::HashMap, os::unix::ffi::OsStrExt, path::PathBuf, sync::Arc};

use tokio::sync::Notify;
use zbus::{
    interface,
    zvariant::{Array, ObjectPath, OwnedObjectPath, OwnedValue, Value},
    ObjectServer,
};

use crate::picker::{path_to_uri, Mode, Outcome, Picker, PickerRequest};

/// D-Bus response codes returned by the FileChooser methods.
const RESPONSE_OK: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

/// Map of result fields returned to the application.
type Results = HashMap<String, Value<'static>>;

struct Request {
    /// Shared cancellation notification observed by the picker.
    cancel: Arc<Notify>,
}

/// Exposes the Request object's methods over D-Bus.
#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    /// Called by the frontend to close the request and notify the picker that the operation should be cancelled.
    async fn close(&self) {
        self.cancel.notify_one();
    }
}

pub struct FileChooser {
    /// Shared picker backend responsible for displaying the UI and collecting the user's selection.
    picker: Arc<Picker>,
}

// ---- Option parsing ----------------------------------------------------------
// These helpers convert D-Bus option values into ordinary Rust types.

/// Returns false if the key is absent or its value cannot be converted to bool.
fn opt_bool(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

/// Returns None if the key is absent, the value cannot be cloned, or it is not a valid D-Bus string.
fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = options.get(key)?.try_clone().ok()?;
    String::try_from(value).ok()
}

/// Reads a filesystem path encoded as a D-Bus byte array (`ay`).
///
/// Returns None if the option is missing, invalid, or empty after trimming.
fn opt_path(options: &HashMap<String, OwnedValue>, key: &str) -> Option<PathBuf> {
    let value = options.get(key)?.try_clone().ok()?;
    let mut bytes = Vec::<u8>::try_from(value).ok()?;

    // Remove the terminating NUL byte(s), if present.
    while bytes.last() == Some(&0) {
        bytes.pop();
    }

    // Unix paths can be represented as raw bytes and need not be UTF-8.
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

/// Converts the options supplied by the D-Bus caller into a PickerRequest.
pub fn request_from_options(
    mode: Mode,
    title: &str,
    options: &HashMap<String, OwnedValue>,
) -> PickerRequest {
    PickerRequest {
        mode,
        multiple: opt_bool(options, "multiple"),
        title: (!title.is_empty()).then(|| title.to_owned()),
        accept_label: opt_string(options, "accept_label"),
        current_folder: opt_path(options, "current_folder"),
        current_name: opt_string(options, "current_name"),
    }
}

/// Internal Implementation

impl FileChooser {
    pub fn new(picker: Arc<Picker>) -> Self {
        Self { picker }
    }

    /// Handles one file-selection request from start to finish.
    ///
    /// This method:
    /// 1. Converts the D-Bus options into a PickerRequest.
    /// 2. Exports a temporary Request object so the frontend can call Close.
    /// 3. Runs the picker and waits for its outcome.
    /// 4. Removes the temporary Request object.
    /// 5. Converts the outcome into a D-Bus response code and result map.
    async fn handle(
        &self,
        server: &ObjectServer,
        handle: ObjectPath<'_>,
        mode: Mode,
        title: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let req = request_from_options(mode, title, options);

        // Share a cancellation signal between the exported Request object and the picker backend.
        let cancel = Arc::new(Notify::new());

        // Keep an owned copy of the request's D-Bus object path.
        let path = OwnedObjectPath::from(handle);

        // Export a temporary Request object at the caller-provided handle.
        // The frontend can call its Close method to notify the picker.
        // e.g. /org/freedesktop/portal/desktop/request/1_42/example
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

        // Remove the temporary Request object after the picker finishes.
        let _ = server.remove::<Request, _>(ObjectPath::from(&path)).await;

        // Translate the picker outcome into the portal's D-Bus response format.
        match outcome {
            Outcome::Selected(paths) => {
                // Convert selected filesystem paths into file URIs.
                let uris: Vec<String> = paths.iter().map(|path| path_to_uri(path)).collect();

                // Return the selected URIs under the standard "uris" result key.
                let mut results = Results::new();
                results.insert("uris".into(), Value::from(Array::from(uris)));

                (RESPONSE_OK, results)
            }
            // The user cancelled the operation. We return empty results.
            Outcome::Cancelled => (RESPONSE_CANCELLED, Results::new()),

            // The picker encountered an error; log its details and return the generic portal error response.
            Outcome::Failed(msg) => {
                tracing::error!("picker failed: {msg}");
                (RESPONSE_OTHER, Results::new())
            }
        }
    }
}

/// Public Implementation exposed on D-Bus
#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    /// Reports the version of the interface supported by this implementation.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    /// Requests that the user select a file.
    ///
    /// If the "directory" option is true, this method instead opens a
    /// directory-selection operation.
    async fn open_file(
        &self,
        // Gives this method access to the D-Bus object server so it can
        // register and remove the temporary Request object.
        #[zbus(object_server)] server: &ObjectServer,
        handle: ObjectPath<'_>,
        // Application ID and parent window identifier supplied by the caller.
        // Currently unused by this implementation.
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        // Options supplied by the application as D-Bus key/value pairs.
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let mode = if opt_bool(&options, "directory") {
            Mode::OpenDirectory
        } else {
            Mode::OpenFile
        };

        // Delegate request handling to the shared implementation.
        self.handle(server, handle, mode, title, &options).await
    }

    /// Requests that the user select a destination for saving a file.
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
