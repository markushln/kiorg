{
  lib,
  rustPlatform,
  src,
  kiorgBin,
}:

rustPlatform.buildRustPackage {
  pname = "xdg-desktop-portal-kiorg";
  version = "0.1.0";

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  cargoBuildFlags = [
    "-p"
    "xdg-desktop-portal-kiorg"
  ];

  installPhase = ''
    runHook preInstall

    # Install the portal backend executable.
    install -Dm755 \
      target/release/xdg-desktop-portal-kiorg \
      "$out/bin/xdg-desktop-portal-kiorg"

    # Install the portal descriptor.
    install -Dm644 \
      crates/xdg-desktop-portal-kiorg/data/kiorg.portal \
      "$out/share/xdg-desktop-portal/portals/kiorg.portal"

    # Install the D-Bus service with the Nix store executable path.
    substitute \
      crates/xdg-desktop-portal-kiorg/data/org.freedesktop.impl.portal.desktop.kiorg.service \
      "$out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.kiorg.service" \
      --replace-fail \
        "/usr/libexec/xdg-desktop-portal-kiorg" \
        "$out/bin/xdg-desktop-portal-kiorg"

    # Install the systemd user service with the Nix store executable path.
    substitute \
      crates/xdg-desktop-portal-kiorg/data/xdg-desktop-portal-kiorg.service \
      "$out/lib/systemd/user/xdg-desktop-portal-kiorg.service" \
      --replace-fail \
        "/usr/libexec/xdg-desktop-portal-kiorg" \
        "$out/bin/xdg-desktop-portal-kiorg"

    # Tell the portal which Kiorg binary to launch in picker mode.
    sed -i \
      "/^ExecStart=/i Environment=\"KIORG_PICKER_BIN=${kiorgBin}/bin/kiorg\"" \
      "$out/lib/systemd/user/xdg-desktop-portal-kiorg.service"

    runHook postInstall
  '';
}
