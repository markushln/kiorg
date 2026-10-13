# xdg-desktop-portal-kiorg

Portal backend for `org.freedesktop.impl.portal.FileChooser` that launches `kiorg --picker`.

## Build & install (user-local example)

```sh
cargo build --release
sudo install -Dm755 target/release/xdg-desktop-portal-kiorg /usr/libexec/xdg-desktop-portal-kiorg
sudo install -Dm644 data/kiorg.portal /usr/share/xdg-desktop-portal/portals/kiorg.portal
sudo install -Dm644 data/org.freedesktop.impl.portal.desktop.kiorg.service /usr/share/dbus-1/services/
sudo install -Dm644 data/xdg-desktop-portal-kiorg.service /usr/lib/systemd/user/
mkdir -p ~/.config/xdg-desktop-portal && cp data/portals.conf.example ~/.config/xdg-desktop-portal/portals.conf
systemctl --user restart xdg-desktop-portal
```

Firefox: `about:config` -> `widget.use-xdg-desktop-portal.file-picker = 1`.

```
## Kiorg picker contract
```

kiorg --picker [--directory | --save] [--multiple] [--title T] [--accept-label L]
      [--current-folder DIR] [--current-name NAME]
stdout: one absolute path per line; exit 0 = selection (empty = cancel), exit 1 = cancel

```
Override binary with `KIORG_PICKER_BIN`. Debug logs: `RUST_LOG=debug`.


# Communication

```mermaid
  sequenceDiagram
      autonumber
      participant B as Browser<br/>(Firefox)
      participant D as D-Bus<br/>(session bus)
      participant P as xdg-desktop-portal<br/>(frontend)
      participant K as xdg-desktop-portal-kiorg<br/>(backend)
      participant G as kiorg --picker<br/>(GUI)

      B->>D: FileChooser.OpenFile(parent_window, title, options)
      D->>P: route to org.freedesktop.portal.Desktop<br/>(activates the portal if not running)
      P-->>B: Request handle (/org/freedesktop/portal/desktop/request/...)
      Note over B: subscribes to Request::Response

      P->>D: impl.FileChooser.OpenFile(handle, app_id, parent_window, title, options)<br/>backend chosen via kiorg.portal + portals.conf
      D->>K: route to org.freedesktop.impl.portal.desktop.kiorg<br/>(activates backend via .service file)
      K->>D: export impl.Request at handle (for Close)
      K->>G: spawn: kiorg --picker [--multiple] [--filter ...] [--current-folder ...]
      Note over G: user browses, marks files, confirms

      alt selection made
          G-->>K: stdout: one absolute path per line, exit 0
          K-->>D: reply (0, {uris: [file://...]})
      else cancelled
          G-->>K: exit 1
          K-->>D: reply (1, {})
      else error
          G-->>K: exit 2 / spawn failure
          K-->>D: reply (2, {})
      end

      D-->>P: backend reply
      P->>D: emit Request::Response(code, results)
      D-->>B: Request::Response(code, results)

      opt browser aborts (tab closed, dialog dismissed)
          B->>D: Request.Close()
          D->>P: Close
          P->>D: impl.Request.Close()
          D->>K: Close
          K->>G: kill child process
      end
```
