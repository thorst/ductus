**A pre-release.** Ductus is in development until v1.0: try it, test it, pilot it, but don't run
production on it yet, and expect settings to change between releases (each change is in the
upgrade notes). No support is offered for pre-releases.

## Which file

| File | For | How |
|---|---|---|
| `ductus_@VERSION@_amd64.deb` | Ubuntu 22.04+, Debian 12+ | [Linux packages](https://docs.getductus.com/install/linux.html) |
| `ductus-@VERSION@-1.x86_64.rpm` | RHEL, Rocky, Alma 9 | [Linux packages](https://docs.getductus.com/install/linux.html) |
| `ductus-@VERSION@-x64.msi` | Windows Server, Windows 11 | [Windows installer](https://docs.getductus.com/install/windows.html) |
| `ductus-@VERSION@-linux-x64.tar.gz`, `ductus-@VERSION@-windows-x64.zip` | Checking an upgrade before installing it (`ductus upgrade --check`); servers without the packages | [The portable archive](https://docs.getductus.com/install/archive.html) |
| `ductus-docs-@VERSION@.zip` | These docs as they are for this version, offline: unzip, open `ductus-docs-@VERSION@/index.html` | |
| `ductus-@VERSION@-linux-x64.cdx.json`, `ductus-@VERSION@-windows-x64.cdx.json`, `ductus-@VERSION@-web-ui.cdx.json` | Your security team's tools: every library in each build (CycloneDX SBOMs) | [Libraries and versions](https://github.com/thorst/ductus/blob/v@VERSION@/docs/reference/libraries.md) |
| `SHA256SUMS` | Checking a download | Below |

Every file was built and tested by this repository's CI from the tagged commit: every test, every
demo, the packages under real systemd, the MSI installed, upgraded and removed as a service.

## Check a download before installing it

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify ductus_@VERSION@_amd64.deb -R thorst/ductus
```

On Windows, compare `(Get-FileHash ductus-@VERSION@-x64.msi).Hash` with the file's line in
`SHA256SUMS`. The attestation proves GitHub built the file from this repository's tagged source
([how](https://docs.getductus.com/install/download.html)). The MSI isn't code-signed before v1.0:
SmartScreen says "unknown publisher" until it is.

## Before upgrading

Run the new version's `ductus upgrade --check` (from the portable archive) against your database
and project first: [Upgrade](https://docs.getductus.com/upgrade.html).
